use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr,
    Request,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use crate::git;
use crate::log_ts;
use crate::sync::SyncManager;

const TTL: Duration = Duration::from_secs(1);
const ROOT_INODE: u64 = 1;

/// Flags do `renameatx_np(2)` do macOS, propagadas pelo macFUSE no
/// `fuse_rename_in` (patch local no vendor/fuser). SWAP = troca atômica de
/// conteúdo (exchange); EXCL = falhar se o destino existe.
const APPLE_RENAME_SWAP: u32 = 0x2;
const APPLE_RENAME_EXCL: u32 = 0x4;

/// Entradas (inode, path) de uma travessia de subárvore.
type SubtreeEntries = Vec<(u64, String)>;
const FILE_MODE: u16 = 0o644;
const DIR_MODE: u16 = 0o755;
/// Modo default exposto para symlinks (lrwxr-xr-x, como o macOS cria).
const SYMLINK_MODE: u16 = 0o755;

/// Cache LRU de blobs de leitura, keyed por inode. Orçamento em bytes; blob
/// maior que o orçamento inteiro não entra (leitura serve direto do repo).
/// Escala esperada: dezenas de entradas de um mount pessoal — eviction O(n)
/// é invisível perto do custo de abrir o repo que cada hit evita.
pub(crate) struct BlobCache {
    entries: HashMap<u64, BlobCacheEntry>,
    budget_bytes: u64,
    total_bytes: u64,
    tick: u64,
    hits: u64,
    misses: u64,
}

struct BlobCacheEntry {
    data: Vec<u8>,
    last_access: u64,
}

impl BlobCache {
    pub(crate) fn new(budget_bytes: u64) -> Self {
        BlobCache {
            entries: HashMap::new(),
            budget_bytes,
            total_bytes: 0,
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn get(&mut self, ino: u64) -> Option<&Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        match self.entries.get_mut(&ino) {
            Some(entry) => {
                entry.last_access = tick;
                self.hits += 1;
                Some(&entry.data)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn insert(&mut self, ino: u64, data: Vec<u8>) {
        let len = data.len() as u64;
        if len > self.budget_bytes {
            return;
        }
        self.remove(ino);
        while self.total_bytes + len > self.budget_bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_access)
                .map(|(ino, _)| *ino)
            else {
                break;
            };
            self.remove(oldest);
        }
        self.tick += 1;
        self.total_bytes += len;
        self.entries.insert(
            ino,
            BlobCacheEntry {
                data,
                last_access: self.tick,
            },
        );
    }

    fn remove(&mut self, ino: u64) {
        if let Some(entry) = self.entries.remove(&ino) {
            self.total_bytes -= entry.data.len() as u64;
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    fn contains(&self, ino: u64) -> bool {
        self.entries.contains_key(&ino)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,
    pub(crate) parent: u64,
    pub(crate) size: u64,
    /// Regular ou Symlink (filemode 120000 no Git). Para symlink, o conteúdo
    /// do blob é o texto do alvo e o attr expõe `FileType::Symlink`.
    pub(crate) kind: git::FileKind,
    /// Arquivo que casa com regras de ignore do repo montado: vive no working
    /// tree, nunca entra em commit. Leituras vêm do disco, não do HEAD.
    pub(crate) local_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DirEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,
    pub(crate) parent: u64,
    pub(crate) children: HashSet<u64>,
}

pub struct GitDriveFS {
    repo_path: String,
    #[allow(dead_code)]
    branch: String,
    files_by_inode: HashMap<u64, FileEntry>,
    dirs_by_inode: HashMap<u64, DirEntry>,
    inodes_by_path: HashMap<String, u64>,
    next_inode: Arc<AtomicU64>,
    sync: SyncManager,
    write_buffers: Arc<parking_lot::Mutex<HashMap<u64, Vec<u8>>>>,
    /// Inodes com escrita ainda não enfileirada no sync ("dirty"). O flush só
    /// gera `SyncOp::Write` para inodes marcados — flush roda em todo close(2),
    /// inclusive de fds de leitura, e sem a marca re-enfileiraria conteúdo
    /// antigo (podendo reverter mudanças remotas mergeadas).
    dirty: HashSet<u64>,
    /// Última `commit_generation` do `SyncManager` observada. Quando avança,
    /// buffers limpos (não-dirty) são descartados — o conteúdo deles já está
    /// no HEAD e mantê-los esconderia mudanças remotas futuras do `read`.
    last_commit_gen: u64,
    /// Cache LRU de blobs para leitura. Invalidado por inteiro nos mesmos
    /// pontos do gc de buffers (HEAD mudou); por inode em unlink/write/
    /// truncate/rename.
    read_cache: BlobCache,
    uid: u32,
    gid: u32,
    mode_overrides: Arc<parking_lot::Mutex<HashMap<u64, u16>>>,
    times: Arc<parking_lot::Mutex<HashMap<u64, SystemTime>>>,
    /// mtime derivado do último commit que tocou cada path (revwalk único no
    /// boot/refresh). Consultado depois de `times` (sessão) e antes de
    /// `boot_time` (resíduo: local-only e diretórios).
    derived_times: HashMap<u64, SystemTime>,
    boot_time: SystemTime,
    xattrs: Arc<parking_lot::Mutex<HashMap<u64, HashMap<OsString, Vec<u8>>>>>,
}

/// Default do orçamento do cache de leitura quando o caller não especifica
/// (espelha o default de `read_cache_mb` no config).
const DEFAULT_READ_CACHE_BYTES: u64 = 64 * 1024 * 1024;

impl GitDriveFS {
    pub fn new(repo_path: &str, branch: String, sync: SyncManager) -> anyhow::Result<Self> {
        Self::new_with_cache_budget(repo_path, branch, sync, DEFAULT_READ_CACHE_BYTES)
    }

    pub fn new_with_cache_budget(
        repo_path: &str,
        branch: String,
        sync: SyncManager,
        read_cache_bytes: u64,
    ) -> anyhow::Result<Self> {
        let repo = git::open_repo(repo_path)?;
        let commit = git::get_commit(&repo, "HEAD")?;
        let dirs = git::list_dirs_at_commit(&repo, &commit)?;
        let files = git::list_files_at_commit(&repo, &commit)?;

        let mut files_by_inode = HashMap::new();
        let mut dirs_by_inode = HashMap::new();
        let mut inodes_by_path = HashMap::new();

        let root = DirEntry {
            inode: ROOT_INODE,
            path: String::new(),
            parent: 0,
            children: HashSet::new(),
        };
        dirs_by_inode.insert(ROOT_INODE, root);
        inodes_by_path.insert(String::new(), ROOT_INODE);

        let next_inode = Arc::new(AtomicU64::new(2));

        // Processar diretórios em ordem crescente de profundidade para garantir
        // que o pai exista quando o filho for inserido.
        let mut dirs_sorted = dirs;
        dirs_sorted.sort_by_key(|p| p.matches('/').count());

        for dir_path in dirs_sorted {
            let inode = next_inode.fetch_add(1, Ordering::SeqCst);
            let parent_path = match dir_path.rfind('/') {
                Some(i) => dir_path[..i].to_string(),
                None => String::new(),
            };
            let parent_inode = *inodes_by_path
                .get(&parent_path)
                .expect("pai deve existir (dirs ordenados por profundidade)");
            let entry = DirEntry {
                inode,
                path: dir_path.clone(),
                parent: parent_inode,
                children: HashSet::new(),
            };
            dirs_by_inode.insert(inode, entry);
            inodes_by_path.insert(dir_path.clone(), inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        for git::ListedFile { path: file_path, size, kind } in files {
            // `.gitkeep` é artefato interno da ilusão "Git não rastreia dir
            // vazio" (decisão 8 da Fase 2): não entra nos índices nem aparece
            // no readdir — sem isso, o rmdir de pasta "vazia" falhava com
            // ENOTEMPTY após um refresh (achado da validação de 03-07-2026).
            if leaf_name(&file_path) == ".gitkeep" {
                continue;
            }
            let inode = next_inode.fetch_add(1, Ordering::SeqCst);
            let parent_path = match file_path.rfind('/') {
                Some(i) => file_path[..i].to_string(),
                None => String::new(),
            };
            let parent_inode = *inodes_by_path
                .get(&parent_path)
                .expect("pai de arquivo deve existir");
            let entry = FileEntry {
                inode,
                path: file_path.clone(),
                parent: parent_inode,
                size,
                kind,
                local_only: false,
            };
            files_by_inode.insert(inode, entry);
            inodes_by_path.insert(file_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        let mut fs = GitDriveFS {
            repo_path: repo_path.to_string(),
            branch,
            files_by_inode,
            dirs_by_inode,
            inodes_by_path,
            next_inode,
            sync,
            write_buffers: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            dirty: HashSet::new(),
            last_commit_gen: 0,
            read_cache: BlobCache::new(read_cache_bytes),
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            mode_overrides: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            times: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            derived_times: HashMap::new(),
            boot_time: SystemTime::now(),
            xattrs: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        };
        // Arquivos ignorados presentes no working tree entram como local-only
        // visíveis (Fase 2.5): aparecem no mount, nunca em commit.
        fs.incorporate_local_only(&HashMap::new());
        // mtime derivado do histórico: sem isso, todo remount "renovaria" o
        // mtime de todos os arquivos para o instante do boot.
        fs.refresh_derived_times();
        Ok(fs)
    }

    fn perm_for(&self, ino: u64, default: u16) -> u16 {
        self.mode_overrides
            .lock()
            .get(&ino)
            .copied()
            .unwrap_or(default)
    }

    fn time_for(&self, ino: u64) -> SystemTime {
        if let Some(t) = self.times.lock().get(&ino) {
            return *t;
        }
        self.derived_times
            .get(&ino)
            .copied()
            .unwrap_or(self.boot_time)
    }

    fn touch(&self, ino: u64) {
        self.times.lock().insert(ino, SystemTime::now());
    }

    fn file_attr_for(&self, ino: u64, size: u64) -> FileAttr {
        let (ftype, default_mode) = match self.files_by_inode.get(&ino).map(|f| f.kind) {
            Some(git::FileKind::Symlink) => (FileType::Symlink, SYMLINK_MODE),
            _ => (FileType::RegularFile, FILE_MODE),
        };
        file_attr(
            ino,
            size,
            self.perm_for(ino, default_mode),
            self.uid,
            self.gid,
            self.time_for(ino),
            ftype,
        )
    }

    fn dir_attr_for(&self, ino: u64) -> FileAttr {
        dir_attr(
            ino,
            self.perm_for(ino, DIR_MODE),
            self.uid,
            self.gid,
            self.time_for(ino),
        )
    }

    fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    fn resolve_parent_path(path: &str) -> String {
        match path.rfind('/') {
            Some(i) => path[..i].to_string(),
            None => String::new(),
        }
    }

    fn resolve_parent_inode(&self, path: &str) -> Option<u64> {
        let parent_path = Self::resolve_parent_path(path);
        self.inodes_by_path.get(&parent_path).copied()
    }

    // ---- introspecção para testes ----
    pub fn file_count(&self) -> usize {
        self.files_by_inode.len()
    }

    pub fn dir_count(&self) -> usize {
        // raiz não conta
        self.dirs_by_inode.len() - 1
    }

    pub fn find_inode(&self, path: &str) -> Option<u64> {
        self.inodes_by_path.get(path).copied()
    }

    pub fn alloc_inode_for_test(&self) -> u64 {
        self.alloc_inode()
    }

    pub fn resolve_parent_for_test(&self, path: &str) -> Option<u64> {
        self.resolve_parent_inode(path)
    }

    /// Resolve o inode filho de `parent` pelo nome. `None` se não existir.
    fn find_child_inode(&self, parent: u64, name: &str) -> Option<u64> {
        let dir = self.dirs_by_inode.get(&parent)?;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return Some(*child);
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return Some(*child);
                }
            }
        }
        None
    }

    /// Lê o conteúdo corrente de um arquivo: primeiro do `write_buffer` (escritas
    /// ainda não drenadas), depois do blob no HEAD do Git.
    fn current_file_content(&self, ino: u64) -> Option<Vec<u8>> {
        let entry = self.files_by_inode.get(&ino)?;
        let path = entry.path.clone();
        self.content_for_inode_at_path(ino, &path)
    }

    /// Versão explícita: consulta o buffer de escritas (chaveado por inode) e,
    /// se vazio, lê o blob do HEAD pelo caminho passado. Útil quando `apply_rename`
    /// já mexeu no path em memória mas ainda precisa do conteúdo antigo.
    fn content_for_inode_at_path(&self, ino: u64, path: &str) -> Option<Vec<u8>> {
        if let Some(buf) = self.write_buffers.lock().get(&ino) {
            return Some(buf.clone());
        }
        if let Some(f) = self.files_by_inode.get(&ino) {
            if f.local_only {
                return self.read_local_only_content(f.kind, path);
            }
        }
        let repo = git::open_repo(&self.repo_path).ok()?;
        let commit = git::get_commit(&repo, "HEAD").ok()?;
        git::read_file_at_commit(&repo, &commit, path).ok()
    }

    // ---- funções de lógica pura para testes de integração ----

    pub fn apply_lookup(&self, parent: u64, name: &str) -> Result<FileAttr, i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return Ok(self.file_attr_for(f.inode, f.size));
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return Ok(self.dir_attr_for(d.inode));
                }
            }
        }
        Err(libc::ENOENT)
    }

    pub fn apply_getattr(&self, ino: u64) -> Result<FileAttr, i32> {
        if let Some(d) = self.dirs_by_inode.get(&ino) {
            Ok(self.dir_attr_for(d.inode))
        } else if let Some(f) = self.files_by_inode.get(&ino) {
            Ok(self.file_attr_for(f.inode, f.size))
        } else {
            Err(libc::ENOENT)
        }
    }

    /// Garante que o inode tem buffer de escrita, inicializando-o com o
    /// conteúdo corrente do arquivo (blob no HEAD) na primeira mutação.
    /// Semântica POSIX: `write`/`truncate` operam sobre o conteúdo existente —
    /// buffer nascendo vazio zerava tudo que não fosse reescrito (corrupção em
    /// append, páginas sujas de editor e truncate).
    fn ensure_buffer(&self, ino: u64, path: &str) {
        if self.write_buffers.lock().contains_key(&ino) {
            return;
        }
        let base = self.content_for_inode_at_path(ino, path).unwrap_or_default();
        self.write_buffers.lock().entry(ino).or_insert(base);
    }

    /// Escrita em offset sobre o buffer do inode. Retorna bytes aceitos.
    pub fn apply_write(&mut self, ino: u64, offset: usize, data: &[u8]) -> Result<u32, i32> {
        let path = match self.files_by_inode.get(&ino) {
            Some(f) => f.path.clone(),
            None => return Err(libc::ENOENT),
        };
        self.ensure_buffer(ino, &path);
        let new_len = {
            let mut buffers = self.write_buffers.lock();
            let buf = buffers.get_mut(&ino).expect("buffer garantido acima");
            if buf.len() < offset + data.len() {
                buf.resize(offset + data.len(), 0);
            }
            buf[offset..offset + data.len()].copy_from_slice(data);
            buf.len() as u64
        };
        if let Some(f) = self.files_by_inode.get_mut(&ino) {
            f.size = new_len;
        }
        self.dirty.insert(ino);
        // Buffer passa a ter precedência no read; a entrada do cache só
        // ocuparia memória (e ficaria stale após o commit).
        self.read_cache.remove(ino);
        Ok(data.len() as u32)
    }

    /// Truncate (setattr com `size`). Materializa a op imediatamente — um
    /// `truncate(2)` por path pode não ser seguido de flush.
    pub fn apply_truncate(&mut self, ino: u64, new_size: u64) -> Result<(), i32> {
        let path = match self.files_by_inode.get(&ino) {
            Some(f) => f.path.clone(),
            None => return Err(libc::ENOENT),
        };
        self.ensure_buffer(ino, &path);
        let data = {
            let mut buffers = self.write_buffers.lock();
            let buf = buffers.get_mut(&ino).expect("buffer garantido acima");
            buf.resize(new_size as usize, 0);
            buf.clone()
        };
        if let Some(f) = self.files_by_inode.get_mut(&ino) {
            f.size = new_size;
        }
        self.sync.queue_write(&path, data);
        // Conteúdo do buffer acabou de ser enfileirado — inode volta a "limpo";
        // um flush subsequente sem escrita nova não deve duplicar a op.
        self.dirty.remove(&ino);
        self.read_cache.remove(ino);
        Ok(())
    }

    /// Leitura de um intervalo do arquivo. Ordem das fontes: write buffer
    /// (mutações pendentes), cache de blobs, blob do HEAD (miss popula o
    /// cache). Erros do git viram EIO.
    pub fn apply_read(&mut self, ino: u64, offset: usize, size: usize) -> Result<Vec<u8>, i32> {
        fn slice_of(data: &[u8], offset: usize, size: usize) -> Vec<u8> {
            let start = offset.min(data.len());
            let end = (offset + size).min(data.len());
            data[start..end].to_vec()
        }

        if let Some(buf) = self.write_buffers.lock().get(&ino) {
            return Ok(slice_of(buf, offset, size));
        }
        let (path, local_only, kind) = match self.files_by_inode.get(&ino) {
            Some(f) => (f.path.clone(), f.local_only, f.kind),
            None => return Err(libc::ENOENT),
        };
        if let Some(data) = self.read_cache.get(ino) {
            return Ok(slice_of(data, offset, size));
        }
        let content = if local_only {
            self.read_local_only_content(kind, &path).ok_or(libc::EIO)?
        } else {
            let repo = git::open_repo(&self.repo_path).map_err(|_| libc::EIO)?;
            let commit = git::get_commit(&repo, "HEAD").map_err(|_| libc::EIO)?;
            git::read_file_at_commit(&repo, &commit, &path).map_err(|_| libc::EIO)?
        };
        let out = slice_of(&content, offset, size);
        self.read_cache.insert(ino, content);
        Ok(out)
    }

    /// Alvo de um symlink (conteúdo do blob = texto do alvo). EINVAL para
    /// arquivo regular, ENOENT para inode desconhecido.
    pub fn apply_readlink(&mut self, ino: u64) -> Result<Vec<u8>, i32> {
        match self.files_by_inode.get(&ino) {
            Some(f) if f.kind == git::FileKind::Symlink => {}
            Some(_) => return Err(libc::EINVAL),
            None => return Err(libc::ENOENT),
        }
        self.apply_read(ino, 0, usize::MAX)
    }

    /// Flush de um inode: enfileira o buffer no sync APENAS se houve escrita
    /// desde o último flush (dirty). Retorna o path enfileirado, ou `None` se
    /// não havia nada a fazer (ex.: close de fd aberto só para leitura).
    pub fn apply_flush(&mut self, ino: u64) -> Option<String> {
        if !self.dirty.contains(&ino) {
            return None;
        }
        let path = self.files_by_inode.get(&ino)?.path.clone();
        let data = self.write_buffers.lock().get(&ino).cloned()?;
        self.sync.queue_write(&path, data);
        self.dirty.remove(&ino);
        self.touch(ino);
        Some(path)
    }

    /// Descarta buffers de inodes sem escrita pendente (não-dirty) e o cache
    /// de blobs inteiro — os pontos de chamada significam "o HEAD mudou".
    /// Buffers dirty nunca são tocados — o conteúdo deles ainda não chegou
    /// ao sync.
    fn gc_clean_buffers(&mut self) {
        let dirty = &self.dirty;
        self.write_buffers.lock().retain(|ino, _| dirty.contains(ino));
        self.read_cache.clear();
    }

    /// Conteúdo corrente visível para leitura (buffer ou blob do HEAD).
    pub fn read_content_for_test(&self, ino: u64) -> Option<Vec<u8>> {
        self.current_file_content(ino)
    }

    /// Quantidade de write buffers vivos (introspecção para testes).
    pub fn buffered_count_for_test(&self) -> usize {
        self.write_buffers.lock().len()
    }

    pub fn read_cache_contains_for_test(&self, ino: u64) -> bool {
        self.read_cache.contains(ino)
    }

    pub fn read_cache_len_for_test(&self) -> usize {
        self.read_cache.len()
    }

    /// (hits, misses) acumulados do cache de leitura.
    pub fn read_cache_stats_for_test(&self) -> (u64, u64) {
        self.read_cache.stats()
    }

    pub fn apply_create(
        &mut self,
        parent: u64,
        name: &str,
    ) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }

        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = FileEntry {
            inode,
            path: full_path.clone(),
            parent,
            size: 0,
            kind: git::FileKind::Regular,
            local_only: false,
        };
        self.files_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        // Enfileirar write vazio para materializar no disco (flush posterior pode sobrescrever)
        self.sync.queue_write(&full_path, Vec::new());
        // Buffer vazio (não-dirty) para que reads antes do commit retornem
        // vazio em vez de EIO — o path novo ainda não existe no HEAD.
        self.write_buffers.lock().insert(inode, Vec::new());

        Ok((inode, self.file_attr_for(inode, 0)))
    }

    /// Cria um symlink `parent/name -> target`. O alvo é texto opaco (nunca
    /// resolvido pelo drive-sync); o blob commitado é o próprio texto.
    pub fn apply_symlink(
        &mut self,
        parent: u64,
        name: &str,
        target: &str,
    ) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }

        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = FileEntry {
            inode,
            path: full_path.clone(),
            parent,
            size: target.len() as u64,
            kind: git::FileKind::Symlink,
            local_only: false,
        };
        self.files_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        // Buffer (não-dirty) com o texto do alvo: readlink funciona antes do
        // commit — o path ainda não existe no HEAD.
        self.write_buffers
            .lock()
            .insert(inode, target.as_bytes().to_vec());
        self.sync.queue_symlink(&full_path, target);
        self.touch(inode);

        Ok((inode, self.file_attr_for(inode, target.len() as u64)))
    }

    pub fn apply_unlink(&mut self, parent: u64, name: &str) -> Result<(), i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut target_ino: Option<u64> = None;
        let mut is_dir = false;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    target_ino = Some(*child);
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    target_ino = Some(*child);
                    is_dir = true;
                    break;
                }
            }
        }
        let Some(ino) = target_ino else {
            return Err(libc::ENOENT);
        };
        if is_dir {
            return Err(libc::EISDIR);
        }
        let entry = self.files_by_inode.remove(&ino).unwrap();
        self.inodes_by_path.remove(&entry.path);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&ino);
        self.write_buffers.lock().remove(&ino);
        self.dirty.remove(&ino);
        self.read_cache.remove(ino);
        self.sync.queue_delete(&entry.path);
        Ok(())
    }

    fn child_exists(&self, parent: u64, name: &str) -> bool {
        let Some(dir) = self.dirs_by_inode.get(&parent) else {
            return false;
        };
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return true;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return true;
                }
            }
        }
        false
    }

    pub fn sync_snapshot_for_test(&self) -> Vec<crate::sync::SyncOp> {
        self.sync.buffer_snapshot()
    }

    pub fn sync_queue_write_for_test(&self, path: &str, content: Vec<u8>) {
        self.sync.queue_write(path, content);
    }

    /// Se o `SyncManager` sinalizou mudanças vindas do pull watcher,
    /// reconstrói os índices `files_by_inode`/`dirs_by_inode`/`inodes_by_path`.
    /// Preserva os inodes de paths ainda existentes e não regride `next_inode`.
    pub fn refresh_if_needed(&mut self) {
        // Guarda crítica: se há ops pendentes no sync buffer (writes/mkdirs/etc
        // ainda não drenados pelo watcher de debounce), NÃO refresh — o refresh
        // reconstrói índices a partir do HEAD do git, descartando qualquer
        // arquivo/dir criado localmente que ainda não chegou ao commit. Sem essa
        // guarda, paths recém-criados via CREATE/MKDIR sumiam dos índices, e o
        // próximo rename/unlink/rmdir pelo Finder retornava ENOENT — Finder
        // mostra "(error code -43)". A flag continua setada (não consumimos
        // com take_refresh_flag), então o refresh roda no próximo
        // lookup/getattr/readdir após o drain.
        if self.sync.has_pending_ops() {
            return;
        }
        // GC de buffers limpos quando um commit local novo aconteceu: com o
        // sync buffer vazio (checado acima), tudo que foi enfileirado já foi
        // drenado, e o commit garante que o conteúdo está no HEAD. Descartar
        // o buffer faz o `read` voltar a enxergar o HEAD — sem isso, mudanças
        // remotas nesse arquivo ficariam invisíveis para sempre.
        let gen = self.sync.commit_generation();
        if gen != self.last_commit_gen {
            self.last_commit_gen = gen;
            self.gc_clean_buffers();
            // Arquivos ignorados que participaram do lote ficaram fora do
            // commit — marcá-los local-only redireciona os reads para o
            // working tree (o buffer que os servia acabou de ser coletado).
            self.incorporate_local_only(&HashMap::new());
        }
        if !self.sync.take_refresh_flag() {
            return;
        }
        // Refresh pós-pull: o HEAD avançou por fetch/merge (que só roda com
        // working tree limpo). Buffers limpos refletem o HEAD antigo — descartar.
        self.gc_clean_buffers();
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{}] refresh_if_needed: erro ao abrir repo: {}", log_ts(), e);
                return;
            }
        };
        let commit = match git::get_commit(&repo, "HEAD") {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[{}] refresh_if_needed: erro ao resolver HEAD: {}", log_ts(), e);
                return;
            }
        };
        let dirs = match git::list_dirs_at_commit(&repo, &commit) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[{}] refresh_if_needed: list_dirs falhou: {}", log_ts(), e);
                return;
            }
        };
        let files = match git::list_files_at_commit(&repo, &commit) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{}] refresh_if_needed: list_files falhou: {}", log_ts(), e);
                return;
            }
        };

        let previous: HashMap<String, u64> = self.inodes_by_path.clone();

        let mut files_by_inode: HashMap<u64, FileEntry> = HashMap::new();
        let mut dirs_by_inode: HashMap<u64, DirEntry> = HashMap::new();
        let mut inodes_by_path: HashMap<String, u64> = HashMap::new();

        let root = DirEntry {
            inode: ROOT_INODE,
            path: String::new(),
            parent: 0,
            children: HashSet::new(),
        };
        dirs_by_inode.insert(ROOT_INODE, root);
        inodes_by_path.insert(String::new(), ROOT_INODE);

        let mut dirs_sorted = dirs;
        dirs_sorted.sort_by_key(|p| p.matches('/').count());

        for dir_path in dirs_sorted {
            // `unwrap_or_else` (lazy): a forma eager com `unwrap_or` avaliava
            // o alloc sempre, vazando um inode por path existente a cada refresh.
            let inode = previous
                .get(&dir_path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let parent_path = Self::resolve_parent_path(&dir_path);
            let parent_inode = *inodes_by_path.get(&parent_path).unwrap_or(&ROOT_INODE);
            let entry = DirEntry {
                inode,
                path: dir_path.clone(),
                parent: parent_inode,
                children: HashSet::new(),
            };
            dirs_by_inode.insert(inode, entry);
            inodes_by_path.insert(dir_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        for git::ListedFile { path: file_path, size, kind } in files {
            // Mesma regra da ingestão inicial: `.gitkeep` fica fora dos índices.
            if leaf_name(&file_path) == ".gitkeep" {
                continue;
            }
            let inode = previous
                .get(&file_path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let parent_path = Self::resolve_parent_path(&file_path);
            let parent_inode = *inodes_by_path.get(&parent_path).unwrap_or(&ROOT_INODE);
            let entry = FileEntry {
                inode,
                path: file_path.clone(),
                parent: parent_inode,
                size,
                kind,
                local_only: false,
            };
            files_by_inode.insert(inode, entry);
            inodes_by_path.insert(file_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        self.files_by_inode = files_by_inode;
        self.dirs_by_inode = dirs_by_inode;
        self.inodes_by_path = inodes_by_path;
        // Local-only não vive no HEAD: reincorpora do working tree, reusando
        // inodes do índice anterior para não invalidar handles do kernel.
        self.incorporate_local_only(&previous);
        // HEAD avançou: paths tocados pelo pull ganham o mtime do commit novo.
        self.refresh_derived_times();
    }

    pub fn apply_mkdir(&mut self, parent: u64, name: &str) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }
        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = DirEntry {
            inode,
            path: full_path.clone(),
            parent,
            children: HashSet::new(),
        };
        self.dirs_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        self.sync.queue_mkdir(&full_path);
        Ok((inode, self.dir_attr_for(inode)))
    }

    /// Swap de conteúdo entre o único arquivo-filho de `source_parent` e o
    /// arquivo de mesmo nome em `target_parent`. Usado quando macFUSE envia
    /// FUSE_RENAME com nomes vazios como encoding alternativo de exchangedata.
    /// Semanticamente: após a chamada, ambos os arquivos continuam existindo,
    /// mas com o conteúdo trocado.
    fn apply_implicit_swap(&mut self, source_parent: u64, target_parent: u64) -> Result<(), i32> {
        // Quando source_parent == target_parent, RENAME parent/"" -> parent/"" é
        // um sinal pós-commit do macFUSE (refresh de metadata), não um swap real
        // do TextEdit. Retorna Ok como no-op antes de tentar pickar arquivo —
        // sem isso, se o dir não tem arquivos (só dirs), pickar falha e
        // retornamos ENOENT, que vira -43 no Finder.
        if source_parent == target_parent {
            return Ok(());
        }
        let (src_ino, src_name, src_path) = {
            let dir = self.dirs_by_inode.get(&source_parent).ok_or(libc::ENOENT)?;
            // O encoding do macFUSE não transporta QUAL arquivo trocar — o
            // contrato do safe-save é um temp dir com UM arquivo. Com zero ou
            // mais de um, qualquer escolha é chute, e chute errado troca o
            // conteúdo do arquivo errado (corrupção silenciosa). EXDEV degrada
            // o app para o fluxo de cópia, que funciona.
            let file_children: Vec<u64> = dir
                .children
                .iter()
                .filter(|c| self.files_by_inode.contains_key(c))
                .copied()
                .collect();
            if file_children.len() != 1 {
                eprintln!(
                    "[{}] swap implicit ambíguo: source_parent={} tem {} arquivos (esperado 1) — retornando EXDEV",
                    log_ts(),
                    source_parent,
                    file_children.len()
                );
                return Err(libc::EXDEV);
            }
            let ino = file_children[0];
            let entry = self.files_by_inode.get(&ino).unwrap();
            (ino, leaf_name(&entry.path).to_string(), entry.path.clone())
        };

        let (tgt_ino, tgt_path) = {
            let dir = self.dirs_by_inode.get(&target_parent).ok_or(libc::ENOENT)?;
            let mut picked: Option<u64> = None;
            for child in &dir.children {
                if let Some(f) = self.files_by_inode.get(child) {
                    if leaf_name(&f.path) == src_name {
                        picked = Some(*child);
                        break;
                    }
                }
            }
            // Sem par no target: não é um swap (caso típico = "Move to Trash"
            // do Finder, onde target=.Trashes/<uid> está vazio). EXDEV sinaliza
            // "operação cross-device" pro caller — preferível a ENOENT (que
            // vira "-43 items can't be found" no Finder). Idealmente, o MKDIR
            // de .Trashes já foi rejeitado antes; isso aqui é defesa em
            // profundidade pra outros encodings de exchangedata ambíguos.
            let Some(ino) = picked else {
                eprintln!(
                    "[{}] swap implicit ambíguo: src={:?} no target_parent={} sem par — retornando EXDEV",
                    log_ts(),
                    src_name,
                    target_parent
                );
                return Err(libc::EXDEV);
            };
            let entry = self.files_by_inode.get(&ino).unwrap();
            (ino, entry.path.clone())
        };

        // Guarda: se source e target são o mesmo inode, não há o que trocar.
        // Acontece quando o Finder emite RENAME 1/"" -> 1/"" em updates de
        // metadata — não é um save real, retorna Ok silenciosamente.
        if src_ino == tgt_ino {
            return Ok(());
        }

        let src_content = self
            .content_for_inode_at_path(src_ino, &src_path)
            .unwrap_or_default();
        let tgt_content = self
            .content_for_inode_at_path(tgt_ino, &tgt_path)
            .unwrap_or_default();

        let src_new_size = tgt_content.len() as u64;
        let tgt_new_size = src_content.len() as u64;
        if let Some(f) = self.files_by_inode.get_mut(&src_ino) {
            f.size = src_new_size;
        }
        if let Some(f) = self.files_by_inode.get_mut(&tgt_ino) {
            f.size = tgt_new_size;
        }

        {
            let mut buffers = self.write_buffers.lock();
            buffers.insert(src_ino, tgt_content.clone());
            buffers.insert(tgt_ino, src_content.clone());
        }

        self.sync.queue_write(&src_path, tgt_content);
        self.sync.queue_write(&tgt_path, src_content);
        self.dirty.remove(&src_ino);
        self.dirty.remove(&tgt_ino);
        self.touch(src_ino);
        self.touch(tgt_ino);

        Ok(())
    }

    /// Troca atômica de conteúdo entre dois arquivos nomeados — semântica do
    /// exchangedata(2)/renameatx_np(RENAME_SWAP). Usada pelo callback
    /// `exchange` (FUSE_EXCHANGE) e por `rename` com a flag SWAP.
    pub fn apply_exchange(
        &mut self,
        parent: u64,
        name: &str,
        newparent: u64,
        newname: &str,
    ) -> Result<(), i32> {
        let ino_a = match self.find_child_inode(parent, name) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => return Err(libc::EISDIR),
            None => return Err(libc::ENOENT),
        };
        let ino_b = match self.find_child_inode(newparent, newname) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => return Err(libc::EISDIR),
            None => return Err(libc::ENOENT),
        };

        let content_a = self.current_file_content(ino_a).unwrap_or_default();
        let content_b = self.current_file_content(ino_b).unwrap_or_default();

        let path_a = self.files_by_inode.get(&ino_a).map(|f| f.path.clone());
        let path_b = self.files_by_inode.get(&ino_b).map(|f| f.path.clone());
        let (Some(path_a), Some(path_b)) = (path_a, path_b) else {
            return Err(libc::ENOENT);
        };

        // Trocar tamanhos em memória
        let size_a = content_a.len() as u64;
        let size_b = content_b.len() as u64;
        if let Some(f) = self.files_by_inode.get_mut(&ino_a) {
            f.size = size_b;
        }
        if let Some(f) = self.files_by_inode.get_mut(&ino_b) {
            f.size = size_a;
        }

        // Buffers refletem o conteúdo trocado para reads antes do commit
        {
            let mut buffers = self.write_buffers.lock();
            buffers.insert(ino_a, content_b.clone());
            buffers.insert(ino_b, content_a.clone());
        }

        self.sync.queue_write(&path_a, content_b);
        self.sync.queue_write(&path_b, content_a);
        self.dirty.remove(&ino_a);
        self.dirty.remove(&ino_b);
        self.read_cache.remove(ino_a);
        self.read_cache.remove(ino_b);

        self.touch(ino_a);
        self.touch(ino_b);
        Ok(())
    }

    pub fn apply_rename(
        &mut self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> Result<(), i32> {
        // Fallback defensivo: nomes vazios eram artefato do parse desalinhado
        // do fuse_rename_in no macFUSE (8 bytes de flags/padding lidos como
        // nomes) — corrigido no vendor/fuser. Se algum encoding ainda chegar
        // assim, cai na heurística antiga de swap do único arquivo-filho.
        if name.is_empty() && new_name.is_empty() {
            return self.apply_implicit_swap(parent, new_parent);
        }

        // 1. Resolver fonte em `parent`
        let src_dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut src_ino: Option<u64> = None;
        let mut src_is_dir = false;
        for child in &src_dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    src_ino = Some(*child);
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    src_ino = Some(*child);
                    src_is_dir = true;
                    break;
                }
            }
        }
        let Some(src) = src_ino else {
            return Err(libc::ENOENT);
        };

        // 2. Validar new_parent é diretório
        if !self.dirs_by_inode.contains_key(&new_parent) {
            return Err(libc::ENOTDIR);
        }

        // 3. Checar destino existente
        let mut dest_existing: Option<(u64, bool)> = None;
        {
            let dst_dir = self.dirs_by_inode.get(&new_parent).unwrap();
            for child in &dst_dir.children {
                if let Some(f) = self.files_by_inode.get(child) {
                    if leaf_name(&f.path) == new_name {
                        dest_existing = Some((*child, false));
                        break;
                    }
                } else if let Some(d) = self.dirs_by_inode.get(child) {
                    if leaf_name(&d.path) == new_name {
                        dest_existing = Some((*child, true));
                        break;
                    }
                }
            }
        }
        if let Some((_dest_ino, dest_is_dir)) = dest_existing {
            if src_is_dir || dest_is_dir {
                return Err(libc::EEXIST);
            }
            // Arquivo em cima de arquivo: overwrite (unlink implícito do destino)
            self.apply_unlink(new_parent, new_name)?;
        }

        // 4. Montar novo path
        let new_parent_path = self.dirs_by_inode.get(&new_parent).unwrap().path.clone();
        let new_path = if new_parent_path.is_empty() {
            new_name.to_string()
        } else {
            format!("{}/{}", new_parent_path, new_name)
        };

        // 5. Capturar old path
        let old_path = if src_is_dir {
            self.dirs_by_inode.get(&src).unwrap().path.clone()
        } else {
            self.files_by_inode.get(&src).unwrap().path.clone()
        };

        // 6. Remover do parent antigo, mover para new_parent
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&src);
        self.dirs_by_inode
            .get_mut(&new_parent)
            .unwrap()
            .children
            .insert(src);

        // 7. Atualizar path/parent da entrada-fonte e descendentes
        if src_is_dir {
            if let Some(d) = self.dirs_by_inode.get_mut(&src) {
                d.path = new_path.clone();
                d.parent = new_parent;
            }
            self.update_descendant_paths(&old_path, &new_path);
        } else {
            if let Some(f) = self.files_by_inode.get_mut(&src) {
                f.path = new_path.clone();
                f.parent = new_parent;
            }
        }
        // Re-indexar paths dos dois (src e descendentes já foram feitos acima)
        self.reindex_paths();

        if src_is_dir {
            let src_eph = crate::sync::is_fully_ephemeral_path(&old_path);
            let dest_eph = crate::sync::is_fully_ephemeral_path(&new_path);
            if dest_eph && !src_eph {
                // Mover dir real para área efêmera (ex.: Trash) = remoção do
                // repo. O filtro descartaria o Rename inteiro e o disco
                // divergiria do índice. Expandimos em Delete/Rmdir dos paths
                // ANTIGOS (reais), bottom-up. Buffers preservam o conteúdo em
                // memória: reads dentro do Trash continuam funcionando e um
                // rename de volta rematerializa.
                let (mut dirs, files) = self.collect_subtree(src);
                let to_old = |p: &str| format!("{}{}", old_path, &p[new_path.len()..]);
                for (ino, path) in &files {
                    let old = to_old(path);
                    let content = self
                        .content_for_inode_at_path(*ino, &old)
                        .unwrap_or_default();
                    self.write_buffers.lock().insert(*ino, content);
                    self.read_cache.remove(*ino);
                    self.dirty.remove(ino);
                    self.sync.queue_delete(&old);
                }
                dirs.sort_by_key(|(_, p)| std::cmp::Reverse(p.matches('/').count()));
                for (_, path) in &dirs {
                    self.sync.queue_rmdir(&to_old(path));
                }
            } else if src_eph && !dest_eph {
                // Dir efêmero virando real = materialização: o conteúdo só
                // existe em memória (ou no HEAD, se era legado commitado).
                // Mkdir top-down + Write com conteúdo corrente.
                let (mut dirs, files) = self.collect_subtree(src);
                let to_old = |p: &str| format!("{}{}", old_path, &p[new_path.len()..]);
                dirs.sort_by_key(|(_, p)| p.matches('/').count());
                for (_, path) in &dirs {
                    self.sync.queue_mkdir(path);
                }
                for (ino, path) in &files {
                    let old = to_old(path);
                    let content = self
                        .content_for_inode_at_path(*ino, &old)
                        .unwrap_or_default();
                    self.sync.queue_write(path, content);
                    self.dirty.remove(ino);
                }
            } else {
                // Diretório: usamos Rename nativo no disco (mv). Dirs
                // vazios/cheios transitam tranquilamente. (Efêmero -> efêmero
                // cai aqui e o filtro do enqueue descarta — memória apenas.)
                self.sync.queue_rename(&old_path, &new_path);
            }
        } else {
            // Arquivo: traduz rename em Write(dest) + Delete(src). Isso desacopla
            // da existência do src em disco (que pode ter sido criada em ops que
            // foram coalescidas no buffer, nunca virando arquivo real). Precisa
            // ler o conteúdo pelo path ANTIGO, já que o in-memory path foi mudado.
            let content = self
                .content_for_inode_at_path(src, &old_path)
                .unwrap_or_default();
            // Buffer (não-dirty) mantém o conteúdo legível até o commit: o
            // path NOVO não existe no HEAD ainda — sem isso, read pós-rename
            // caía no blob inexistente e retornava EIO. O gc descarta após
            // o commit. A entrada de cache do path antigo fica stale — fora.
            self.write_buffers.lock().insert(src, content.clone());
            self.read_cache.remove(src);
            self.sync.queue_write(&new_path, content);
            self.sync.queue_delete(&old_path);
            // Conteúdo corrente (incluindo escritas pendentes) foi enfileirado
            // no destino — inode volta a "limpo".
            self.dirty.remove(&src);
        }
        Ok(())
    }

    /// Incorpora ao índice os arquivos ignorados do working tree (local-only
    /// visíveis). Paths já presentes são marcados/atualizados; ausentes ganham
    /// entrada nova (reusando inode de `previous` quando o path existia antes
    /// de um rebuild). Diretórios intermediários fora do HEAD são criados.
    fn incorporate_local_only(&mut self, previous: &HashMap<String, u64>) {
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{}] local-only: erro ao abrir repo: {}", log_ts(), e);
                return;
            }
        };
        let ignored = match git::list_ignored_files(&repo) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[{}] local-only: erro ao listar ignorados: {}", log_ts(), e);
                return;
            }
        };
        for git::ListedFile { path, size, kind } in ignored {
            if leaf_name(&path) == ".gitkeep" || crate::sync::is_fully_ephemeral_path(&path) {
                continue;
            }
            if let Some(&ino) = self.inodes_by_path.get(&path) {
                if let Some(f) = self.files_by_inode.get_mut(&ino) {
                    f.local_only = true;
                    f.kind = kind;
                    // Buffer pendente tem precedência sobre o tamanho do disco.
                    if !self.write_buffers.lock().contains_key(&ino) {
                        f.size = size;
                    }
                }
                continue;
            }
            let parent_path = Self::resolve_parent_path(&path);
            let parent_ino = self.ensure_dir_chain(&parent_path, previous);
            let ino = previous
                .get(&path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let entry = FileEntry {
                inode: ino,
                path: path.clone(),
                parent: parent_ino,
                size,
                kind,
                local_only: true,
            };
            self.files_by_inode.insert(ino, entry);
            self.inodes_by_path.insert(path, ino);
            self.dirs_by_inode
                .get_mut(&parent_ino)
                .unwrap()
                .children
                .insert(ino);
        }
    }

    /// Garante que a cadeia de diretórios de `dir_path` existe no índice,
    /// criando entradas em memória para dirs fora do HEAD (ex.: dir inteiro
    /// ignorado). Retorna o inode do dir final.
    fn ensure_dir_chain(&mut self, dir_path: &str, previous: &HashMap<String, u64>) -> u64 {
        if dir_path.is_empty() {
            return ROOT_INODE;
        }
        if let Some(&ino) = self.inodes_by_path.get(dir_path) {
            return ino;
        }
        let parent = self.ensure_dir_chain(&Self::resolve_parent_path(dir_path), previous);
        let ino = previous
            .get(dir_path)
            .copied()
            .unwrap_or_else(|| self.alloc_inode());
        let entry = DirEntry {
            inode: ino,
            path: dir_path.to_string(),
            parent,
            children: HashSet::new(),
        };
        self.dirs_by_inode.insert(ino, entry);
        self.inodes_by_path.insert(dir_path.to_string(), ino);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(ino);
        ino
    }

    /// Teto de commits percorridos pelo revwalk de mtime. Histórias mais
    /// longas caem no fallback (timestamp do HEAD) para não onerar o boot.
    const MTIME_WALK_DEPTH_CAP: usize = 4096;

    /// Recalcula o mapa de mtimes derivados (inode -> timestamp do último
    /// commit que tocou o path). Chamado no boot e no refresh pós-pull.
    fn refresh_derived_times(&mut self) {
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{}] mtime: erro ao abrir repo: {}", log_ts(), e);
                return;
            }
        };
        let by_path = match git::commit_times_by_path(&repo, Self::MTIME_WALK_DEPTH_CAP) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[{}] mtime: revwalk falhou: {}", log_ts(), e);
                return;
            }
        };
        self.derived_times.clear();
        for (path, time) in by_path {
            if let Some(&ino) = self.inodes_by_path.get(&path) {
                self.derived_times.insert(ino, time);
            }
        }
    }

    /// Conteúdo de um arquivo local-only, lido do working tree. Para symlink,
    /// o "conteúdo" é o texto do alvo (paridade com o blob 120000 do Git).
    fn read_local_only_content(&self, kind: git::FileKind, path: &str) -> Option<Vec<u8>> {
        let full = std::path::Path::new(&self.repo_path).join(path);
        match kind {
            git::FileKind::Symlink => std::fs::read_link(&full)
                .ok()
                .map(|t| t.as_os_str().as_encoded_bytes().to_vec()),
            git::FileKind::Regular => std::fs::read(&full).ok(),
        }
    }

    /// Coleta a subárvore de `root_ino` (inclusive): dirs e arquivos com os
    /// paths correntes do índice. Travessia local, sem I/O.
    fn collect_subtree(&self, root_ino: u64) -> (SubtreeEntries, SubtreeEntries) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut stack = vec![root_ino];
        while let Some(ino) = stack.pop() {
            if let Some(d) = self.dirs_by_inode.get(&ino) {
                dirs.push((ino, d.path.clone()));
                stack.extend(d.children.iter().copied());
            } else if let Some(f) = self.files_by_inode.get(&ino) {
                files.push((ino, f.path.clone()));
            }
        }
        (dirs, files)
    }

    fn update_descendant_paths(&mut self, old_prefix: &str, new_prefix: &str) {
        let old_with_sep = format!("{}/", old_prefix);
        let new_with_sep = format!("{}/", new_prefix);
        for entry in self.dirs_by_inode.values_mut() {
            if entry.path.starts_with(&old_with_sep) {
                entry.path = format!("{}{}", new_with_sep, &entry.path[old_with_sep.len()..]);
            }
        }
        for entry in self.files_by_inode.values_mut() {
            if entry.path.starts_with(&old_with_sep) {
                entry.path = format!("{}{}", new_with_sep, &entry.path[old_with_sep.len()..]);
            }
        }
    }

    fn reindex_paths(&mut self) {
        self.inodes_by_path.clear();
        self.inodes_by_path.insert(String::new(), ROOT_INODE);
        for (ino, d) in &self.dirs_by_inode {
            if *ino != ROOT_INODE {
                self.inodes_by_path.insert(d.path.clone(), *ino);
            }
        }
        for (ino, f) in &self.files_by_inode {
            self.inodes_by_path.insert(f.path.clone(), *ino);
        }
    }

    pub fn apply_rmdir(&mut self, parent: u64, name: &str) -> Result<(), i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut target_ino: Option<u64> = None;
        let mut is_file = false;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    target_ino = Some(*child);
                    is_file = true;
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    target_ino = Some(*child);
                    break;
                }
            }
        }
        let Some(ino) = target_ino else {
            return Err(libc::ENOENT);
        };
        if is_file {
            return Err(libc::ENOTDIR);
        }
        let target = self.dirs_by_inode.get(&ino).unwrap();
        if !target.children.is_empty() {
            return Err(libc::ENOTEMPTY);
        }
        let path = target.path.clone();
        self.dirs_by_inode.remove(&ino);
        self.inodes_by_path.remove(&path);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&ino);
        self.sync.queue_rmdir(&path);
        Ok(())
    }

    pub fn apply_readdir(&self, ino: u64) -> Result<Vec<(u64, FileType, String)>, i32> {
        let dir = self.dirs_by_inode.get(&ino).ok_or(libc::ENOENT)?;
        let mut out: Vec<(u64, FileType, String)> = Vec::new();
        out.push((ino, FileType::Directory, ".".to_string()));
        out.push((
            dir.parent.max(ROOT_INODE),
            FileType::Directory,
            "..".to_string(),
        ));
        // Ordenação estável por nome. Sem isso, paginação entre chamadas de readdir
        // pode embaralhar entries (HashSet tem ordem não-determinística em iterações
        // separadas após mutações).
        let mut children: Vec<(u64, FileType, String)> = dir
            .children
            .iter()
            .filter_map(|child| {
                if let Some(f) = self.files_by_inode.get(child) {
                    let ftype = match f.kind {
                        git::FileKind::Symlink => FileType::Symlink,
                        git::FileKind::Regular => FileType::RegularFile,
                    };
                    Some((*child, ftype, leaf_name(&f.path).to_string()))
                } else {
                    self.dirs_by_inode
                        .get(child)
                        .map(|d| (*child, FileType::Directory, leaf_name(&d.path).to_string()))
                }
            })
            .collect();
        children.sort_by(|a, b| a.2.cmp(&b.2));
        out.extend(children);
        Ok(out)
    }
}

impl Filesystem for GitDriveFS {
    fn init(
        &mut self,
        _req: &Request<'_>,
        config: &mut fuser::KernelConfig,
    ) -> Result<(), libc::c_int> {
        // No macOS, precisa declarar a capability FUSE_EXCHANGE_DATA durante o
        // handshake pra que o kernel macFUSE encaminhe exchangedata(2) como
        // FUSE_EXCHANGE. Sem esse bit, TextEdit e apps iWork falham no safe save
        // com "The file doesn't exist" porque o swap atômico não chega aqui.
        #[cfg(target_os = "macos")]
        {
            let _ = config.add_capabilities(fuser::consts::FUSE_EXCHANGE_DATA);
        }
        println!("[{}] boot: init ok", log_ts());
        Ok(())
    }

    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        println!("[{}] OPEN inode={}", log_ts(), ino);
        reply.opened(ino, 0);
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        match self.apply_write(ino, offset as usize, data) {
            Ok(written) => {
                println!(
                    "[{}] WRITE inode={} offset={} bytes={}",
                    log_ts(),
                    ino,
                    offset,
                    data.len()
                );
                reply.written(written);
            }
            Err(code) => reply.error(code),
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: fuser::ReplyEmpty,
    ) {
        // O buffer NÃO é drenado aqui: o conteúdo fica disponível para reads
        // subsequentes (e pro `apply_implicit_swap`, que roda DEPOIS do flush
        // no ciclo de safe save do TextEdit). O enfileiramento é condicionado
        // ao dirty flag em `apply_flush` — flush de fd de leitura é no-op.
        if let Some(path) = self.apply_flush(ino) {
            println!("[{}] FLUSH inode={} path={}", log_ts(), ino, path);
        }
        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        println!("[{}] RELEASE inode={}", log_ts(), ino);
        reply.ok();
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        self.refresh_if_needed();
        match self.apply_getattr(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(code) => reply.error(code),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        println!(
            "[{}] SETATTR inode={} mode={:?} size={:?} mtime={:?}",
            log_ts(),
            ino,
            mode,
            size,
            mtime
        );

        if let Some(m) = mode {
            self.mode_overrides
                .lock()
                .insert(ino, (m as u16) & 0o7777);
        }

        if let Some(new_size) = size {
            // Erros ignorados de propósito: setattr de size em inode desconhecido
            // mantém o comportamento anterior (segue para o reply abaixo).
            let _ = self.apply_truncate(ino, new_size);
        }

        let resolved = mtime.or(atime).map(|t| match t {
            fuser::TimeOrNow::Now => SystemTime::now(),
            fuser::TimeOrNow::SpecificTime(st) => st,
        });
        if let Some(t) = resolved {
            self.times.lock().insert(ino, t);
        } else if mode.is_some() || size.is_some() {
            self.touch(ino);
        }

        if self.dirs_by_inode.contains_key(&ino) {
            reply.attr(&TTL, &self.dir_attr_for(ino));
        } else if let Some(entry) = self.files_by_inode.get(&ino) {
            let size = entry.size;
            reply.attr(&TTL, &self.file_attr_for(ino, size));
        } else {
            reply.error(libc::ENOENT);
        }
    }

    /// `link(2)` — cria hardlink. TextEdit e apps iWork usam no "safe save" pra
    /// manter um backup do arquivo original antes de sobrescrever. Sem isso o
    /// save falha (mesmo com exchange implementado).
    ///
    /// Nosso modelo interno é 1 inode = 1 path, então hardlink "de verdade" não
    /// dá. Simulamos via CÓPIA com novo inode + mesmo conteúdo. Funciona pro
    /// pattern do safe save porque o backup é transiente (deletado logo em
    /// seguida). Se app mantiver o backup, vai aparecer como um arquivo normal.
    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let Some(newname_str) = newname.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] LINK ino={} -> parent={} name={:?}",
            log_ts(),
            ino,
            newparent,
            newname_str
        );

        if !self.files_by_inode.contains_key(&ino) {
            reply.error(libc::ENOENT);
            return;
        }
        if !self.dirs_by_inode.contains_key(&newparent) {
            reply.error(libc::ENOENT);
            return;
        }
        if self.child_exists(newparent, newname_str) {
            reply.error(libc::EEXIST);
            return;
        }

        let content = self.current_file_content(ino).unwrap_or_default();
        let size = content.len() as u64;

        let parent_path = self.dirs_by_inode.get(&newparent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            newname_str.to_string()
        } else {
            format!("{}/{}", parent_path, newname_str)
        };

        let src_kind = self
            .files_by_inode
            .get(&ino)
            .map(|f| f.kind)
            .unwrap_or(git::FileKind::Regular);
        let new_inode = self.alloc_inode();
        let entry = FileEntry {
            inode: new_inode,
            path: full_path.clone(),
            parent: newparent,
            size,
            kind: src_kind,
            local_only: false,
        };
        self.files_by_inode.insert(new_inode, entry);
        self.inodes_by_path.insert(full_path.clone(), new_inode);
        self.dirs_by_inode
            .get_mut(&newparent)
            .unwrap()
            .children
            .insert(new_inode);

        // Popular write_buffer pra que leituras imediatas vejam o conteúdo
        self.write_buffers.lock().insert(new_inode, content.clone());
        self.sync.queue_write(&full_path, content);
        self.touch(new_inode);

        let attr = self.file_attr_for(new_inode, size);
        reply.entry(&TTL, &attr, 0);
    }

    /// `exchangedata()` no macOS — troca conteúdo de dois arquivos atomicamente.
    /// TextEdit e apps iWork usam isso no "safe save". Se retornarmos ENOSYS
    /// (default), o save falha com "The file doesn't exist" mesmo depois de
    /// escrever o novo conteúdo num temp dir.
    #[cfg(target_os = "macos")]
    fn exchange(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        let (Some(name), Some(newname)) = (name.to_str(), newname.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] EXCHANGE {}/{} <-> {}/{}",
            log_ts(),
            parent,
            name,
            newparent,
            newname
        );

        match self.apply_exchange(parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(code) => reply.error(code),
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        const BSIZE: u32 = 4096;
        const NAMELEN: u32 = 255;

        let total_bytes: u64 = self.files_by_inode.values().map(|f| f.size).sum();
        let used_blocks = total_bytes.div_ceil(BSIZE as u64);

        // Volume advertido com ~1 TiB "livre" pra apps macOS não tratarem o mount
        // como inutilizável ("Zero bytes available" fazia Finder cair em modo de
        // resumo/categorização em vez de listar arquivos). O limite real vem do
        // disco que hospeda o repo Git — isso aqui é sinal pro userland, não quota.
        let total_blocks: u64 = 256 * 1024 * 1024;
        let free_blocks = total_blocks.saturating_sub(used_blocks);

        let files_used = (self.files_by_inode.len() + self.dirs_by_inode.len()) as u64;
        let files_total: u64 = 1_000_000;
        let ffree = files_total.saturating_sub(files_used);

        reply.statfs(
            total_blocks,
            free_blocks,
            free_blocks,
            files_total,
            ffree,
            BSIZE,
            NAMELEN,
            BSIZE,
        );
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        const XATTR_CREATE: i32 = 0x1;
        const XATTR_REPLACE: i32 = 0x2;

        let mut xattrs = self.xattrs.lock();
        let entry = xattrs.entry(ino).or_insert_with(HashMap::new);
        let exists = entry.contains_key(name);
        if flags & XATTR_CREATE != 0 && exists {
            reply.error(libc::EEXIST);
            return;
        }
        if flags & XATTR_REPLACE != 0 && !exists {
            // macOS usa ENOATTR (93) para "xattr não existe"; ENODATA (96 no macOS,
            // 61 no Linux) tem significado diferente e confunde o kernel.
            reply.error(enoattr());
            return;
        }
        entry.insert(name.to_os_string(), value.to_vec());
        reply.ok();
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let xattrs = self.xattrs.lock();
        let value = xattrs.get(&ino).and_then(|m| m.get(name));
        match value {
            Some(v) => {
                if size == 0 {
                    reply.size(v.len() as u32);
                } else if (size as usize) < v.len() {
                    reply.error(libc::ERANGE);
                } else {
                    reply.data(v);
                }
            }
            None => reply.error(enoattr()),
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let xattrs = self.xattrs.lock();
        let mut buf: Vec<u8> = Vec::new();
        if let Some(m) = xattrs.get(&ino) {
            for key in m.keys() {
                buf.extend_from_slice(key.as_encoded_bytes());
                buf.push(0);
            }
        }
        if size == 0 {
            reply.size(buf.len() as u32);
        } else if (size as usize) < buf.len() {
            reply.error(libc::ERANGE);
        } else {
            reply.data(&buf);
        }
    }

    fn removexattr(&mut self, _req: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let mut xattrs = self.xattrs.lock();
        let removed = xattrs
            .get_mut(&ino)
            .and_then(|m| m.remove(name))
            .is_some();
        if removed {
            reply.ok();
        } else {
            reply.error(enoattr());
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        // Só refresca na primeira página; chamadas de continuação (offset > 0) devem
        // ver o mesmo snapshot que a primeira, senão pular entries embaralha o iterador.
        if offset == 0 {
            self.refresh_if_needed();
        }
        match self.apply_readdir(ino) {
            Ok(entries) => {
                let start = offset as usize;
                for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(start) {
                    // fuser::ReplyDirectory::add retorna true quando o buffer enche — a
                    // entry NÃO foi aceita. Paramos e deixamos o kernel chamar de novo
                    // com offset = i (a próxima não entregue).
                    let full = reply.add(*child_ino, (i + 1) as i64, *kind, name);
                    if full {
                        break;
                    }
                }
                reply.ok();
            }
            Err(code) => reply.error(code),
        }
    }

    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        // macOS Finder usa `getattrlistbulk`, que o macFUSE traduz pra readdirplus.
        // Sem essa implementação, o fallback do macFUSE retornava listagem
        // inconsistente (só diretórios apareciam no Finder).
        if offset == 0 {
            self.refresh_if_needed();
        }
        match self.apply_readdir(ino) {
            Ok(entries) => {
                let start = offset as usize;
                for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(start) {
                    let attr = match kind {
                        FileType::Directory => self.dir_attr_for(*child_ino),
                        // RegularFile e Symlink: o kind correto sai do índice
                        // dentro de `file_attr_for`.
                        _ => {
                            let size = self
                                .files_by_inode
                                .get(child_ino)
                                .map(|f| f.size)
                                .unwrap_or(0);
                            self.file_attr_for(*child_ino, size)
                        }
                    };
                    let full = reply.add(
                        *child_ino,
                        (i + 1) as i64,
                        name.as_str(),
                        &TTL,
                        &attr,
                        0,
                    );
                    if full {
                        break;
                    }
                }
                reply.ok();
            }
            Err(code) => reply.error(code),
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] CREATE parent={} name={:?}",
            log_ts(),
            parent,
            filename
        );
        match self.apply_create(parent, filename) {
            Ok((ino, attr)) => reply.created(&TTL, &attr, 0, ino, 0),
            Err(code) => reply.error(code),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] UNLINK parent={} name={:?}",
            log_ts(),
            parent,
            filename
        );
        match self.apply_unlink(parent, filename) {
            Ok(()) => {
                println!("[{}] UNLINK ok parent={} name={:?}", log_ts(), parent, filename);
                reply.ok()
            }
            Err(code) => {
                eprintln!(
                    "[{}] UNLINK ERR parent={} name={:?} code={}",
                    log_ts(),
                    parent,
                    filename,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        // `.Trashes` é permitido (Lixeira nativa do Finder, Fase 2). O EPERM
        // que existia aqui era mitigação para o parse quebrado do rename do
        // macFUSE (nomes vazios + swap ambíguo), corrigido em b8f2647. O dir
        // é efêmero: existe só em memória, e mover itens para dentro vira
        // remoção no repo (expansão da Fase 1) com conteúdo recuperável via
        // buffers até esvaziar a Lixeira ou remontar.
        println!(
            "[{}] MKDIR parent={} name={:?}",
            log_ts(),
            parent,
            filename
        );
        match self.apply_mkdir(parent, filename) {
            Ok((_, attr)) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] RMDIR parent={} name={:?}",
            log_ts(),
            parent,
            filename
        );
        match self.apply_rmdir(parent, filename) {
            Ok(()) => {
                println!("[{}] RMDIR ok parent={} name={:?}", log_ts(), parent, filename);
                reply.ok()
            }
            Err(code) => {
                eprintln!(
                    "[{}] RMDIR ERR parent={} name={:?} code={}",
                    log_ts(),
                    parent,
                    filename,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let (Some(n), Some(nn)) = (name.to_str(), new_name.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] RENAME {}/{:?} -> {}/{:?} flags=0x{:x}",
            log_ts(),
            parent,
            n,
            new_parent,
            nn,
            flags
        );
        // renameatx_np(RENAME_SWAP): TextEdit/iWork usam no safe save quando o
        // FS anuncia suporte. Semântica de exchangedata — troca de conteúdo.
        if flags & APPLE_RENAME_SWAP != 0 {
            match self.apply_exchange(parent, n, new_parent, nn) {
                Ok(()) => {
                    println!("[{}] RENAME(SWAP) ok {:?} <-> {:?}", log_ts(), n, nn);
                    reply.ok();
                }
                Err(code) => {
                    eprintln!(
                        "[{}] RENAME(SWAP) ERR {:?} <-> {:?} code={}",
                        log_ts(),
                        n,
                        nn,
                        code
                    );
                    reply.error(code);
                }
            }
            return;
        }
        // renameatx_np(RENAME_EXCL): falha se o destino já existe.
        if flags & APPLE_RENAME_EXCL != 0 && self.child_exists(new_parent, nn) {
            reply.error(libc::EEXIST);
            return;
        }
        match self.apply_rename(parent, n, new_parent, nn) {
            Ok(()) => {
                println!(
                    "[{}] RENAME ok {}/{:?} -> {}/{:?}",
                    log_ts(),
                    parent,
                    n,
                    new_parent,
                    nn
                );
                reply.ok()
            }
            Err(code) => {
                eprintln!(
                    "[{}] RENAME ERR {}/{:?} -> {}/{:?} code={}",
                    log_ts(),
                    parent,
                    n,
                    new_parent,
                    nn,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let filename = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };
        self.refresh_if_needed();
        match self.apply_lookup(parent, filename) {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.apply_read(ino, offset as usize, size as usize) {
            Ok(data) => reply.data(&data),
            Err(code) => reply.error(code),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.apply_readlink(ino) {
            Ok(target) => reply.data(&target),
            Err(code) => reply.error(code),
        }
    }

    fn symlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let (Some(name), Some(target_str)) = (link_name.to_str(), target.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        println!(
            "[{}] SYMLINK parent={} name={:?} -> {:?}",
            log_ts(),
            parent,
            name,
            target_str
        );
        match self.apply_symlink(parent, name, target_str) {
            Ok((_, attr)) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }
}

fn file_attr(
    ino: u64,
    size: u64,
    perm: u16,
    uid: u32,
    gid: u32,
    time: SystemTime,
    kind: FileType,
) -> FileAttr {
    FileAttr {
        ino,
        size,
        // FUSE conta em unidades de 512 bytes, independente do blksize reportado.
        blocks: size.div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind,
        perm,
        nlink: 1,
        uid,
        gid,
        rdev: 0,
        flags: 0,
        blksize: 4096,
    }
}

fn dir_attr(ino: u64, perm: u16, uid: u32, gid: u32, time: SystemTime) -> FileAttr {
    FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: FileType::Directory,
        // Convenção POSIX: nlink de diretório = 2 (. e seu nome) + subdiretórios.
        // Simplificamos para 2 — alguns apps macOS checam se é > 1 e interpretam 1
        // como diretório "órfão"/sendo deletado e escondem da UI.
        nlink: 2,
        perm,
        uid,
        gid,
        rdev: 0,
        flags: 0,
        blksize: 4096,
    }
}

/// Errno para "extended attribute não existe". macOS usa `ENOATTR` (93); Linux
/// usa `ENODATA` (61). Se a gente fixar um dos dois, o outro kernel interpreta
/// como erro completamente diferente e pode degradar o comportamento do FS.
#[inline]
fn enoattr() -> i32 {
    #[cfg(target_os = "macos")]
    {
        libc::ENOATTR
    }
    #[cfg(not(target_os = "macos"))]
    {
        libc::ENODATA
    }
}

fn leaf_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::BlobCache;

    #[test]
    fn blob_cache_counts_hits_and_misses() {
        let mut c = BlobCache::new(100);
        assert!(c.get(1).is_none());
        c.insert(1, b"abc".to_vec());
        assert_eq!(c.get(1).map(|d| d.as_slice()), Some(b"abc".as_slice()));
        assert_eq!(c.stats(), (1, 1));
    }

    #[test]
    fn blob_cache_evicts_lru_when_over_budget() {
        let mut c = BlobCache::new(10);
        c.insert(1, vec![0; 6]);
        c.insert(2, vec![0; 4]);
        // Acessa 1 para que 2 vire o LRU
        assert!(c.get(1).is_some());
        c.insert(3, vec![0; 4]);
        assert!(c.contains(1));
        assert!(!c.contains(2), "entrada LRU deve ter sido despejada");
        assert!(c.contains(3));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn blob_cache_skips_blob_larger_than_budget() {
        let mut c = BlobCache::new(4);
        c.insert(1, vec![0; 5]);
        assert!(!c.contains(1));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn blob_cache_clear_resets_entries() {
        let mut c = BlobCache::new(100);
        c.insert(1, vec![0; 10]);
        c.insert(2, vec![0; 10]);
        c.clear();
        assert_eq!(c.len(), 0);
        // Espaço liberado: reinsere sem eviction
        c.insert(3, vec![0; 90]);
        assert!(c.contains(3));
    }

    #[test]
    fn blob_cache_insert_same_inode_replaces_without_double_count() {
        let mut c = BlobCache::new(10);
        c.insert(1, vec![0; 6]);
        c.insert(1, vec![0; 8]);
        assert!(c.contains(1));
        assert_eq!(c.len(), 1);
        // Se o total tivesse contado 6+8, este insert despejaria o inode 1
        c.insert(2, vec![0; 2]);
        assert!(c.contains(1));
        assert!(c.contains(2));
    }
}
