# Arquitetura: Visão Geral

> English version: [../architecture.md](../architecture.md)

## Visão Geral

git-drive é um filesystem FUSE em Rust que monta o conteúdo de um repositório Git (no branch
ativo) como um sistema de arquivos virtual e mantém sincronização bidirecional com o remote via
Git: escritas locais viram commits + push automáticos e mudanças no remote chegam através de um
pull watcher periódico. O comportamento é o de um drive "estilo Dropbox" onde o armazenamento
durável é um repositório Git (incluindo GitHub).

Duas transformações de armazenamento opcionais acontecem entre o drive e o repo, ambas invisíveis
para quem usa o mount:

- **Chunking** (Fase 2.6): arquivo acima de `chunk_threshold_mb` vira um manifest pequeno no path
  original + chunks content-addressed em `.chunks/` — cada blob abaixo do limite de 100 MB por
  arquivo do GitHub.
- **Criptografia em repouso** (Fase 2.7): com `encryption_enabled`, conteúdo E nomes vivem
  cifrados no repo (working tree incluso); o mount apresenta tudo claro.

```mermaid
flowchart LR
    CLI[CLI: main.rs] --> Config[Config::load_from<br/>~/.config/git-drive/config.json]
    Config --> Sync[SyncManager<br/>2 watchers async]
    Sync --> Write[Watcher de escrita<br/>debounce -> ops -> chunking/cripto -> commit -> push]
    Sync --> Pull[Watcher de pull<br/>fetch + FF ou merge 3-way]
    Sync --> FS[GitDriveFS<br/>indices hierarquicos da visao CLARA]
    FS --> Mount[fuser::spawn_mount2]
    Mount --> Kernel[(kernel FUSE)]
    Kernel -- syscalls --> FS
    FS -- queue_* em paths claros --> Sync
    Write -- git commit + push --> Remote[(origin)]
    Pull -- git fetch + merge --> Remote
    Pull -- refresh_flag --> FS
```

---

## Estrutura de Diretórios

```text
git-drive/
├── Cargo.toml                    # fuser, git2, tokio, clap, serde, sha2, chacha20poly1305, hkdf...
├── AGENTS.md                     # invariantes, armadilhas e guia para agentes
├── README.md
├── .github/workflows/ci.yml     # CI de verificação (espelha os comandos do AGENTS.md)
├── docs/
│   ├── README.md
│   ├── usage.md                  # guia passo a passo do usuário
│   ├── architecture.md
│   ├── sync-flow.md
│   ├── fuse-filesystem.md
│   └── pt-br/                    # versões em português dos docs
├── src/
│   ├── main.rs                   # CLI, dispatch de subcomandos, lifecycle do mount
│   ├── lib.rs                    # módulos públicos p/ testes + redirect de logs
│   ├── fs.rs                     # GitDriveFS + impl fuser::Filesystem + apply_*
│   ├── git.rs                    # wrapper sobre git2 (leitura, commit, push, pull, ignore, mtime)
│   ├── sync.rs                   # SyncManager + fila causal + drain + varreduras
│   ├── chunk.rs                  # manifest + fatiamento content-addressed (Fase 2.6)
│   ├── crypt.rs                  # criptografia em repouso (Fase 2.7)
│   ├── config.rs                 # loader do config.json
│   ├── paths.rs                  # resolução de paths (XDG ~/.config/git-drive + legados)
│   ├── status.rs                 # status.json atômico + subcomando status
│   ├── notify.rs                 # notificações macOS (osascript + rate limit)
│   ├── tray.rs                   # ícone da barra de menus
│   └── launchd.rs                # LaunchAgents (install/uninstall)
├── tests/
│   ├── common/mod.rs             # helpers (init_repo determinístico, commit_*, bare remote)
│   ├── smoke.rs
│   ├── fs_hierarchy.rs           # modelo hierárquico, buffers, chunking e cripto na visão FUSE
│   ├── sync_flow.rs              # drain, commit, push, pull, migrações
│   └── e2e_integration.rs        # cenários ponta-a-ponta
└── vendor/fuser/                 # fuser 0.14 com patches locais (upstream no roadmap)
```

O config e a chave de criptografia vivem em `~/.config/git-drive/` (com fallback de leitura para
o local legado em `~/Library/Application Support/git-drive/`); status.json fica no Application
Support e logs em `~/Library/Logs/git-drive/`.

---

## Camadas do Sistema

| Camada | Arquivo | Responsabilidade |
| --- | --- | --- |
| CLI / Orquestração | `src/main.rs` | Subcomandos (`status`/`install`/`uninstall`/`tray`/`key generate`) e modo mount: runtime tokio explícito, recuperação de mount zumbi, validação da chave de criptografia (habilitada sem chave = aborta), espera de sinais e shutdown limpo (unmount -> drain final). |
| Filesystem | `src/fs.rs` | Implementa `fuser::Filesystem`. Índices hierárquicos da visão CLARA, buffers de escrita (RAM copy-on-write ou spool em disco), leitura com cache LRU + handle de repo persistente, detecção lazy de manifests, decifração AEAD, mtime derivado, local-only. |
| Git | `src/git.rs` | Leitura de trees/blobs (com `FileKind` p/ symlinks), commit do working tree (`add_all` respeitando ignore), push com detecção de rejeição por ref, fetch, FF e merge 3-way com conflicted copies, `is_working_tree_clean`, ignorados e `commit_times_by_path`. |
| Sincronização | `src/sync.rs` | Fila causal de `SyncOp` (coalescing só de Write, barreiras no resto), drain com tradução claro->cifrado, varredura de chunking, migrações de criptografia nos dois sentidos, GC de chunks órfãos, push com reconcile e pull watcher. |
| Chunking | `src/chunk.rs` | Manifest texto versionado, sharding `.chunks/<2 hex>/<sha256>`, fatiamento em streaming com transform opcional (selagem) e dedup por content-addressing. |
| Criptografia | `src/crypt.rs` | XChaCha20-Poly1305 convergente (nonce derivado do plaintext), HKDF de subchaves, nomes por componente em base32 lowercase com fallback literal, arquivo de chave 0600. |
| Observabilidade | `src/status.rs`, `src/notify.rs`, `src/tray.rs` | `status.json` atômico, notificações macOS com rate limit e tray com 4 estados. |
| Instalação / Paths | `src/launchd.rs`, `src/paths.rs` | LaunchAgents por template, migração de config/chave do local legado, resolução XDG. |
| Configuração | `src/config.rs` | Loader com `#[serde(default)]` por campo (11 campos). Degrada para defaults com log. |

---

## Estruturas de Dados

### `FileEntry` — `src/fs.rs`

```rust
#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,          // caminho completo CLARO
    pub(crate) parent: u64,
    pub(crate) size: u64,             // tamanho do plaintext
    pub(crate) kind: git::FileKind,   // Regular | Symlink (filemode 120000)
    pub(crate) local_only: bool,      // ignorado pelo .gitignore: vive no worktree, nunca commita
}
```

### `SyncOp` e `WriteContent` — `src/sync.rs`

```rust
#[derive(Debug, Clone)]
pub enum WriteContent {
    Mem(Arc<Vec<u8>>),      // compartilhado com o write buffer (zero cópia no flush)
    Spooled(PathBuf),       // snapshot em disco, aplicado com rename no drain
}

#[derive(Debug, Clone)]
pub enum SyncOp {
    Write { path: String, content: WriteContent },
    Delete { path: String },
    Mkdir { path: String },
    Rmdir { path: String },
    Rename { from: String, to: String },
    Symlink { path: String, target: String },
}
```

A fila interna é um `Vec<SyncOp>` em **ordem de chegada** — a ordem do kernel FUSE é a ordem
causal (invariante da Fase 0). Todos os paths na fila são CLAROS; a tradução para o disco
(cifrada ou não) acontece na materialização do drain.

### `Manifest` — `src/chunk.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub total_size: u64,        // bytes de PLAINTEXT
    pub chunk_size: u64,
    pub hashes: Vec<String>,    // sha256 de cada chunk (do ciphertext, se cifrado)
}
```

### `Crypto` — `src/crypt.rs`

```rust
pub struct Crypto {
    content_cipher: XChaCha20Poly1305,   // subchave HKDF "content-key"
    name_cipher: XChaCha20Poly1305,      // subchave HKDF "name-key"
    content_nonce_key: [u8; 32],         // HMAC p/ nonce determinístico (convergente)
    name_nonce_key: [u8; 32],
}
```

### `Config` — `src/config.rs`

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub write_debounce_secs: u64,          // 5
    pub pull_interval_secs: u64,           // 60
    pub read_cache_mb: u64,                // 64
    pub large_file_threshold_mb: u64,      // 64  (spool de write buffers)
    pub chunk_threshold_mb: u64,           // 95  (manifest + .chunks)
    pub chunk_size_mb: u64,                // 80
    pub encryption_enabled: bool,          // false
    pub encryption_key_file: Option<String>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub remote_token: Option<String>,
}
```

---

## Notas de Performance

- Leitura: buffer do inode -> cache LRU de blobs (`read_cache_mb`) -> repo via **handle
  persistente** (uma abertura por mount, não por syscall). Arquivo maior que o orçamento nunca
  entra no cache; chunkados usam cache do último chunk.
- Escrita: pico de RAM ~1x o tamanho do arquivo (Arc compartilhado buffer/fila); acima de
  `large_file_threshold_mb` o buffer vive em spool no disco.
- Pull watcher pula iteração se há lote pendente (evita corrida com commit local).
- `refresh_if_needed` é chamado em cada `lookup`/`getattr`/`readdir`. Barato em steady state;
  caro após pull (rebuild) ou commit local (gc + varredura de local-only).
- `Config::load_from` roda uma única vez — não há hot-reload (mudar `encryption_enabled` exige
  reiniciar o daemon; a migração roda no drain seguinte).

---

## Limitações Conhecidas

| # | Limitação | Origem |
| --- | --- | --- |
| 1 | `.gitkeep` automático em `mkdir`/`rmdir` | Git não rastreia diretórios vazios — workaround em `src/sync.rs` |
| 2 | Credenciais e chave em texto plano no disco local | `~/.config/git-drive/` (keychain é candidato futuro) |
| 3 | Conflito de merge vira conflicted copy | `pull_and_merge` 3-way; sem identidade degrada para FF-only |
| 4 | Push falho não é revertido | commit local permanece; reconcile + retry automáticos |
| 5 | HEAD detached aborta boot | `src/main.rs` |
| 6 | Sem FIFOs/sockets/hardlinks reais | symlinks suportados; `link` é cópia |
| 7 | `chmod` em memória, `chown` ignorado | uid/gid são os do processo do mount |
| 8 | Cache de blobs não cobre arquivo maior que o orçamento | miss usa handle de repo persistente |
| 9 | Refresh depende de syscalls FUSE | TTL de 1s no kernel pode mascarar mudanças remotas por ~1s |
| 10 | Arquivos acima de `chunk_threshold_mb` viram manifest + `.chunks/` | transparente no mount; clone puro vê os artefatos (`src/chunk.rs`) |
| 11 | Criptografia em repouso é opcional e não protege o passado | história pré-migração permanece legível; ignore/local-only desativados no modo cifrado (`src/crypt.rs`) |
| 12 | Runtime só em macOS | a suíte passa em Linux (CI); o port do mount fuse3 está no roadmap |

---

## Como adicionar um novo campo de configuração

1. Declarar em `src/config.rs` com `#[serde(default = "...")]` ou `#[serde(default)]` para `Option`.
1. Atualizar `Config::default` para espelhar o valor.
1. Propagar em `src/main.rs` (parâmetro de construtor ou setter pós-construção, padrão
   `set_chunking`/`set_crypto`).
1. Atualizar as tabelas em `README.md`, `docs/usage.md` (e `./uso.md`) e `AGENTS.md`.
1. Adicionar teste em `src/config.rs::tests` cobrindo default e override.

## Como adicionar uma nova operação de mutação

1. Adicionar variante em `SyncOp` (`src/sync.rs`) — decidir se coalesce ou é barreira
   (`push_coalescing`); barreira é o default seguro.
1. Implementar `queue_*` em `SyncManager` chamando `enqueue`.
1. Implementar tratamento em `apply_op_to_disk` (`src/sync.rs`), incluindo a tradução de path e
   selagem quando `crypto` estiver ativo.
1. Implementar `apply_*` puro em `GitDriveFS` (`src/fs.rs`) que manipula índices e chama o `queue_*`.
1. Criar wrapper fino no `impl Filesystem` chamando o `apply_*`.
1. Testes: unitário para a fila, integração para `apply_*` e fluxo em `tests/sync_flow.rs`.

---

## Referências

| Arquivo | Responsabilidade |
| --- | --- |
| `src/main.rs` | Entry point, CLI, lifecycle do mount e shutdown limpo |
| `src/fs.rs` | `GitDriveFS`, modelo hierárquico, `apply_*`, `refresh_if_needed` |
| `src/git.rs` | Wrapper sobre `git2` (leitura, commit, push, pull, ignore, mtime) |
| `src/sync.rs` | `SyncManager`, fila causal, drain, varreduras e GC |
| `src/chunk.rs` | Manifest + fatiamento content-addressed |
| `src/crypt.rs` | Criptografia em repouso |
| `src/config.rs` | Loader de `config.json` |
| `src/paths.rs` | Resolução XDG + legados |
| `tests/common/mod.rs` | Helpers de repositório Git para testes |

Docs relacionadas:

- [uso.md](./uso.md)
- [fluxo-de-sincronizacao.md](./fluxo-de-sincronizacao.md)
- [filesystem-fuse.md](./filesystem-fuse.md)
