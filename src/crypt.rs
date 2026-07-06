//! Núcleo puro da criptografia em repouso (Fase 2.7).
//!
//! O repo (working tree + HEAD) guarda conteúdo E nomes cifrados; a camada
//! FUSE apresenta tudo em claro. AEAD XChaCha20-Poly1305 com nonce derivado do
//! plaintext (criptografia convergente): mesmo conteúdo + mesma chave = mesmo
//! ciphertext — preserva dedup de chunks e estabilidade de blob no Git.
//! Decisões em `specs/plans/2026-07-06-110446_fase-2.7-criptografia/decisoes.md`.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;

/// Primeira linha de todo blob cifrado (com `\n`): permite estado misto
/// durante migração — a leitura detecta claro vs cifrado por arquivo.
pub const CONTENT_MAGIC: &[u8] = b"drive-sync-encrypted v1\n";

const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Limite do filesystem para um componente de nome.
const NAME_COMPONENT_MAX: usize = 255;
/// Sufixo de conflicted copy aplicado pelo pull sobre o nome CIFRADO.
const CONFLICT_MARKER: &str = ".conflict-";

/// Alfabeto base32 (RFC 4648) em minúsculas — à prova de filesystem
/// case-insensitive (APFS default), onde base64 colidiria por case.
const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

type HmacSha256 = Hmac<Sha256>;

pub struct Crypto {
    content_cipher: XChaCha20Poly1305,
    name_cipher: XChaCha20Poly1305,
    content_nonce_key: [u8; 32],
    name_nonce_key: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum CryptError {
    /// Blob sem magic, truncado, adulterado ou chave incorreta.
    OpenFailed,
    /// Nome cifrado excederia o limite do filesystem.
    NameTooLong,
}

impl std::fmt::Display for CryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptError::OpenFailed => {
                write!(f, "falha ao decifrar: chave incorreta ou blob adulterado")
            }
            CryptError::NameTooLong => {
                write!(f, "nome cifrado excede o limite de 255 bytes do filesystem")
            }
        }
    }
}

impl Crypto {
    /// Constrói a partir da chave-mestre de 32 bytes, derivando subchaves de
    /// domínio separado via HKDF-SHA256.
    pub fn from_master_key(master: &[u8; 32]) -> Crypto {
        let hk = Hkdf::<Sha256>::new(Some(b"drive-sync v1"), master);
        let mut content_key = [0u8; 32];
        let mut name_key = [0u8; 32];
        let mut content_nonce_key = [0u8; 32];
        let mut name_nonce_key = [0u8; 32];
        hk.expand(b"content-key", &mut content_key).expect("hkdf");
        hk.expand(b"name-key", &mut name_key).expect("hkdf");
        hk.expand(b"content-nonce-key", &mut content_nonce_key)
            .expect("hkdf");
        hk.expand(b"name-nonce-key", &mut name_nonce_key).expect("hkdf");
        Crypto {
            content_cipher: XChaCha20Poly1305::new((&content_key).into()),
            name_cipher: XChaCha20Poly1305::new((&name_key).into()),
            content_nonce_key,
            name_nonce_key,
        }
    }

    /// Nonce determinístico: HMAC(subchave, plaintext) truncado. Convergência
    /// intencional (ver decisão 3) — nonce é público (vai no header).
    fn derive_nonce(key: &[u8; 32], plaintext: &[u8]) -> [u8; NONCE_LEN] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
        mac.update(plaintext);
        let digest = mac.finalize().into_bytes();
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&digest[..NONCE_LEN]);
        nonce
    }

    /// `true` se os bytes têm o header de conteúdo cifrado.
    pub fn is_encrypted_bytes(bytes: &[u8]) -> bool {
        bytes.starts_with(CONTENT_MAGIC)
    }

    /// Sela conteúdo: `MAGIC || nonce || ciphertext+tag`. Determinístico.
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = Self::derive_nonce(&self.content_nonce_key, plaintext);
        let ct = self
            .content_cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .expect("aead encrypt não falha com params válidos");
        let mut out = Vec::with_capacity(CONTENT_MAGIC.len() + NONCE_LEN + ct.len());
        out.extend_from_slice(CONTENT_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Abre conteúdo selado. Erro para magic ausente, truncamento, adulteração
    /// ou chave errada — nunca retorna lixo.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptError> {
        let body = sealed
            .strip_prefix(CONTENT_MAGIC)
            .ok_or(CryptError::OpenFailed)?;
        if body.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptError::OpenFailed);
        }
        let (nonce, ct) = body.split_at(NONCE_LEN);
        self.content_cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| CryptError::OpenFailed)
    }

    /// Cifra um componente de nome: AEAD determinístico + base32 lowercase.
    pub fn enc_component(&self, clear: &str) -> Result<String, CryptError> {
        let plaintext = clear.as_bytes();
        let nonce = Self::derive_nonce(&self.name_nonce_key, plaintext);
        let ct = self
            .name_cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .expect("aead encrypt não falha com params válidos");
        let mut raw = Vec::with_capacity(NONCE_LEN + ct.len());
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        let encoded = base32_encode(&raw);
        if encoded.len() > NAME_COMPONENT_MAX {
            return Err(CryptError::NameTooLong);
        }
        Ok(encoded)
    }

    /// Decifra um componente. `None` = não é um componente cifrado válido
    /// (fallback literal do caller — estado misto, `.gitkeep`, `.chunks`).
    pub fn dec_component(&self, encoded: &str) -> Option<String> {
        let raw = base32_decode(encoded)?;
        if raw.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let plain = self
            .name_cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .ok()?;
        String::from_utf8(plain).ok()
    }

    /// Cifra um path relativo componente a componente.
    pub fn enc_path(&self, clear: &str) -> Result<String, CryptError> {
        let parts: Result<Vec<String>, CryptError> = clear
            .split('/')
            .map(|c| self.enc_component(c))
            .collect();
        Ok(parts?.join("/"))
    }

    /// Decifra um path componente a componente, com fallback literal para
    /// componentes não-cifrados e tratamento do sufixo de conflicted copy
    /// (`<cifrado>.conflict-...` vira `<claro>.conflict-...`).
    pub fn dec_path(&self, cipher: &str) -> String {
        cipher
            .split('/')
            .map(|c| self.dec_component_lossy(c))
            .collect::<Vec<_>>()
            .join("/")
    }

    fn dec_component_lossy(&self, component: &str) -> String {
        if let Some(clear) = self.dec_component(component) {
            return clear;
        }
        // Conflicted copy: sufixo claro aplicado sobre nome cifrado.
        if let Some(idx) = component.find(CONFLICT_MARKER) {
            let (prefix, suffix) = component.split_at(idx);
            if let Some(clear) = self.dec_component(prefix) {
                return format!("{}{}", clear, suffix);
            }
        }
        // Fallback literal: estado misto na migração e artefatos internos.
        component.to_string()
    }
}

/// Gera arquivo de chave novo: 64 hex chars, permissão 0600. Recusa
/// sobrescrever a menos que `force`.
pub fn generate_key_file(path: &Path, force: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if path.exists() && !force {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "arquivo de chave já existe (use --force para sobrescrever — a chave antiga será PERDIDA)",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut master = [0u8; 32];
    getrandom::getrandom(&mut master)
        .map_err(|e| std::io::Error::other(format!("getrandom: {}", e)))?;
    let hex: String = master.iter().map(|b| format!("{:02x}", b)).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let mut file = opts.open(path)?;
    file.write_all(hex.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Carrega a chave-mestre do arquivo. Erros objetivos: ausente, permissão
/// aberta demais (group/other), formato inválido.
pub fn load_key_file(path: &Path) -> std::io::Result<[u8; 32]> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("arquivo de chave inacessível em {}: {}", path.display(), e),
        )
    })?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "arquivo de chave {} com permissão insegura (esperado 0600) — rode: chmod 600 nele",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)?;
    let hex = text.trim();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(std::io::Error::other(
            "arquivo de chave inválido: esperado 64 caracteres hex",
        ));
    }
    let mut master = [0u8; 32];
    for i in 0..32 {
        master[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| std::io::Error::other("hex inválido no arquivo de chave"))?;
    }
    Ok(master)
}

/// Base32 RFC 4648 minúscula, sem padding.
fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from(buf[0]) << 32
            | u64::from(buf[1]) << 24
            | u64::from(buf[2]) << 16
            | u64::from(buf[3]) << 8
            | u64::from(buf[4]);
        let n_chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..n_chars {
            let idx = ((bits >> (35 - i * 5)) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }
    out
}

/// Decodifica base32 minúscula sem padding. `None` para qualquer caractere
/// fora do alfabeto ou comprimento impossível.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    // Comprimentos válidos por bloco de 5 bytes: 2, 4, 5, 7, 8 chars.
    if matches!(text.len() % 8, 1 | 3 | 6) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    for chunk in text.as_bytes().chunks(8) {
        let mut bits: u64 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            let val = BASE32_ALPHABET.iter().position(|&a| a == c)? as u64;
            bits |= val << (35 - i * 5);
        }
        let n_bytes = match chunk.len() {
            2 => 1,
            4 => 2,
            5 => 3,
            7 => 4,
            8 => 5,
            _ => return None,
        };
        for i in 0..n_bytes {
            out.push(((bits >> (32 - i * 8)) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_master_key(&[7u8; 32])
    }

    fn other_crypto() -> Crypto {
        Crypto::from_master_key(&[9u8; 32])
    }

    #[test]
    fn seal_open_roundtrip() {
        let c = crypto();
        let plain = b"conteudo sensivel do drive";
        let sealed = c.seal(plain);
        assert!(Crypto::is_encrypted_bytes(&sealed));
        assert!(!Crypto::is_encrypted_bytes(plain));
        assert_eq!(c.open(&sealed).unwrap(), plain.to_vec());
    }

    #[test]
    fn seal_is_deterministic_and_content_sensitive() {
        let c = crypto();
        assert_eq!(c.seal(b"mesmo"), c.seal(b"mesmo"), "convergente: mesmo plaintext = mesmo blob");
        assert_ne!(c.seal(b"um"), c.seal(b"outro"));
        // Chaves diferentes divergem mesmo para plaintext igual
        assert_ne!(crypto().seal(b"x"), other_crypto().seal(b"x"));
    }

    #[test]
    fn open_rejects_wrong_key_truncation_and_tamper() {
        let c = crypto();
        let sealed = c.seal(b"dados");
        assert_eq!(other_crypto().open(&sealed), Err(CryptError::OpenFailed));
        assert_eq!(c.open(&sealed[..sealed.len() - 1]), Err(CryptError::OpenFailed));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(c.open(&tampered), Err(CryptError::OpenFailed));
        assert_eq!(c.open(b"sem magic"), Err(CryptError::OpenFailed));
    }

    #[test]
    fn name_component_roundtrip_and_determinism() {
        let c = crypto();
        let enc = c.enc_component("Relatório Médico.pdf").unwrap();
        assert_eq!(enc, c.enc_component("Relatório Médico.pdf").unwrap());
        assert_eq!(c.dec_component(&enc).unwrap(), "Relatório Médico.pdf");
        // Só minúsculas e dígitos do alfabeto base32 (seguro em APFS)
        assert!(enc.bytes().all(|b| BASE32_ALPHABET.contains(&b)));
        // Chave errada não decifra
        assert_eq!(other_crypto().dec_component(&enc), None);
    }

    #[test]
    fn path_roundtrip_preserves_hierarchy() {
        let c = crypto();
        let enc = c.enc_path("docs/2026/Relatório.pdf").unwrap();
        assert_eq!(enc.matches('/').count(), 2, "hierarquia 1:1");
        assert_eq!(c.dec_path(&enc), "docs/2026/Relatório.pdf");
    }

    #[test]
    fn dec_path_falls_back_to_literal_for_clear_components() {
        let c = crypto();
        // Estado misto: dir cifrado com artefato literal dentro
        let dir = c.enc_component("projetos").unwrap();
        let mixed = format!("{}/.gitkeep", dir);
        assert_eq!(c.dec_path(&mixed), "projetos/.gitkeep");
        assert_eq!(c.dec_path("nome-claro.txt"), "nome-claro.txt");
    }

    #[test]
    fn dec_path_handles_conflict_suffix_on_cipher_name() {
        let c = crypto();
        let enc = c.enc_component("nota.txt").unwrap();
        let conflicted = format!("{}.conflict-mac-123", enc);
        assert_eq!(c.dec_path(&conflicted), "nota.txt.conflict-mac-123");
    }

    #[test]
    fn enc_component_rejects_names_that_would_exceed_fs_limit() {
        let c = crypto();
        let long = "a".repeat(200);
        assert_eq!(c.enc_component(&long), Err(CryptError::NameTooLong));
        // E o limite prático (~119 bytes) passa
        let ok = "b".repeat(110);
        assert!(c.enc_component(&ok).is_ok());
    }

    #[test]
    fn base32_roundtrip_various_lengths() {
        for len in [1usize, 2, 3, 4, 5, 6, 39, 40, 41, 64] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            let enc = base32_encode(&data);
            assert_eq!(base32_decode(&enc).unwrap(), data, "len {}", len);
        }
        assert_eq!(base32_decode("ABC"), None, "maiúsculas fora do alfabeto");
        assert_eq!(base32_decode("a"), None, "comprimento impossível");
        assert_eq!(base32_decode(""), None);
    }

    #[test]
    fn key_file_generate_load_roundtrip_and_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.key");
        generate_key_file(&path, false).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let master = load_key_file(&path).unwrap();
        let c = Crypto::from_master_key(&master);
        assert_eq!(c.open(&c.seal(b"x")).unwrap(), b"x".to_vec());

        // Não sobrescreve sem force; com force gera chave DIFERENTE
        assert!(generate_key_file(&path, false).is_err());
        generate_key_file(&path, true).unwrap();
        assert_ne!(load_key_file(&path).unwrap(), master);
    }

    #[test]
    fn key_file_rejects_insecure_permissions_and_bad_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.key");
        generate_key_file(&path, false).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_key_file(&path).is_err(), "permissão aberta deve ser recusada");

        use std::os::unix::fs::PermissionsExt;
        let bad = dir.path().join("bad.key");
        std::fs::write(&bad, "não é hex").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_key_file(&bad).is_err());
    }
}
