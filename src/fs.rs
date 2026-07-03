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
const FILE_MODE: u16 = 0o644;
const DIR_MODE: u16 = 0o755;

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,
    pub(crate) parent: u64,
    pub(crate) size: u64,
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
    uid: u32,
    gid: u32,
    mode_overrides: Arc<parking_lot::Mutex<HashMap<u64, u16>>>,
    times: Arc<parking_lot::Mutex<HashMap<u64, SystemTime>>>,
    boot_time: SystemTime,
    xattrs: Arc<parking_lot::Mutex<HashMap<u64, HashMap<OsString, Vec<u8>>>>>,
}

impl GitDriveFS {
    pub fn new(repo_path: &str, branch: String, sync: SyncManager) -> anyhow::Result<Self> {
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

        for (file_path, size) in files {
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
            };
            files_by_inode.insert(inode, entry);
            inodes_by_path.insert(file_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        Ok(GitDriveFS {
            repo_path: repo_path.to_string(),
            branch,
            files_by_inode,
            dirs_by_inode,
            inodes_by_path,
            next_inode,
            sync,
            write_buffers: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            mode_overrides: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            times: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            boot_time: SystemTime::now(),
            xattrs: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        })
    }

    fn perm_for(&self, ino: u64, default: u16) -> u16 {
        self.mode_overrides
            .lock()
            .get(&ino)
            .copied()
            .unwrap_or(default)
    }

    fn time_for(&self, ino: u64) -> SystemTime {
        self.times
            .lock()
            .get(&ino)
            .copied()
            .unwrap_or(self.boot_time)
    }

    fn touch(&self, ino: u64) {
        self.times.lock().insert(ino, SystemTime::now());
    }

    fn file_attr_for(&self, ino: u64, size: u64) -> FileAttr {
        file_attr(
            ino,
            size,
            self.perm_for(ino, FILE_MODE),
            self.uid,
            self.gid,
            self.time_for(ino),
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
        Ok(())
    }

    /// Conteúdo corrente visível para leitura (buffer ou blob do HEAD).
    pub fn read_content_for_test(&self, ino: u64) -> Option<Vec<u8>> {
        self.current_file_content(ino)
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

        Ok((inode, self.file_attr_for(inode, 0)))
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
        if !self.sync.buffer_snapshot().is_empty() {
            return;
        }
        if !self.sync.take_refresh_flag() {
            return;
        }
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
            // `unwrap_or_else` (lazy): `unwrap_or(&self.alloc_inode())` avaliava
            // o alloc SEMPRE, vazando um inode por path existente a cada refresh.
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

        for (file_path, size) in files {
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
            let mut picked: Option<u64> = None;
            for child in &dir.children {
                if self.files_by_inode.contains_key(child) {
                    picked = Some(*child);
                    break;
                }
            }
            let ino = picked.ok_or(libc::ENOENT)?;
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
        self.touch(src_ino);
        self.touch(tgt_ino);

        Ok(())
    }

    pub fn apply_rename(
        &mut self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> Result<(), i32> {
        // Caso especial: macFUSE encoda o swap atômico do TextEdit como
        // FUSE_RENAME com ambos os nomes vazios. Quando isso chega, a intenção
        // é "trocar o conteúdo do único arquivo em source_parent com o de
        // mesmo nome em target_parent" — equivalente a exchangedata(2).
        // Sem esse tratamento o save no TextEdit falha com ENOENT → -43 no Finder.
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
            // Diretório: usamos Rename nativo no disco (mv). Dirs vazios/cheios
            // transitam tranquilamente.
            self.sync.queue_rename(&old_path, &new_path);
        } else {
            // Arquivo: traduz rename em Write(dest) + Delete(src). Isso desacopla
            // da existência do src em disco (que pode ter sido criada em ops que
            // foram coalescidas no buffer, nunca virando arquivo real). Precisa
            // ler o conteúdo pelo path ANTIGO, já que o in-memory path foi mudado.
            let content = self
                .content_for_inode_at_path(src, &old_path)
                .unwrap_or_default();
            self.sync.queue_write(&new_path, content);
            self.sync.queue_delete(&old_path);
        }
        Ok(())
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
                    Some((*child, FileType::RegularFile, leaf_name(&f.path).to_string()))
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
        // Clone em vez de remove: o conteúdo precisa ficar disponível para
        // reads subsequentes (e pro `apply_implicit_swap`, que roda DEPOIS do
        // flush no ciclo de safe save do TextEdit). Antes com `remove`, o swap
        // lia do blob HEAD (ou cai em empty), porque o buffer já tinha sido
        // drenado — resultado: commits sobrescrevendo hi.txt com conteúdo vazio.
        // O buffer é limpo em unlink/rename/rmdir via `discard_buffered`.
        if let Some(entry) = self.files_by_inode.get(&ino) {
            if let Some(data) = self.write_buffers.lock().get(&ino).cloned() {
                println!("[{}] FLUSH inode={} path={}", log_ts(), ino, entry.path);
                self.sync.queue_write(&entry.path, data);
                self.touch(ino);
            }
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

        let new_inode = self.alloc_inode();
        let entry = FileEntry {
            inode: new_inode,
            path: full_path.clone(),
            parent: newparent,
            size,
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

        let ino_a = match self.find_child_inode(parent, name) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => {
                reply.error(libc::EISDIR);
                return;
            }
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let ino_b = match self.find_child_inode(newparent, newname) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => {
                reply.error(libc::EISDIR);
                return;
            }
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let content_a = self.current_file_content(ino_a).unwrap_or_default();
        let content_b = self.current_file_content(ino_b).unwrap_or_default();

        let path_a = self.files_by_inode.get(&ino_a).map(|f| f.path.clone());
        let path_b = self.files_by_inode.get(&ino_b).map(|f| f.path.clone());
        let (Some(path_a), Some(path_b)) = (path_a, path_b) else {
            reply.error(libc::ENOENT);
            return;
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

        // Atualizar write_buffers pra que reads subsequentes vejam o conteúdo trocado
        // antes do commit
        {
            let mut buffers = self.write_buffers.lock();
            buffers.insert(ino_a, content_b.clone());
            buffers.insert(ino_b, content_a.clone());
        }

        // Queue writes pra materializar em disco + commit
        self.sync.queue_write(&path_a, content_b);
        self.sync.queue_write(&path_b, content_a);

        self.touch(ino_a);
        self.touch(ino_b);

        reply.ok();
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
                        FileType::RegularFile => {
                            let size = self
                                .files_by_inode
                                .get(child_ino)
                                .map(|f| f.size)
                                .unwrap_or(0);
                            self.file_attr_for(*child_ino, size)
                        }
                        _ => self.file_attr_for(*child_ino, 0),
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
        // Bloqueia criação de `.Trashes` no root. Quando o user faz "Move to
        // Trash" no Finder, macOS cria `.Trashes/<uid>/` e tenta usar
        // exchangedata pra mover o arquivo selecionado pra lá. Como
        // exchangedata é encodado pelo macFUSE como FUSE_RENAME com nomes
        // vazios, e o target dir está vazio (sem par pra swap), nosso
        // `apply_implicit_swap` falhava com ENOENT → "(error code -43)" no
        // Finder. Recusando `.Trashes` aqui (EPERM), o Finder degrada para o
        // fluxo de "permanent delete via UNLINK direto" (mostra dialog
        // "Are you sure? This will permanently delete..."), que envia o nome
        // do arquivo explicitamente e funciona.
        if parent == ROOT_INODE && filename == ".Trashes" {
            eprintln!(
                "[{}] MKDIR REJECT .Trashes (forçar UNLINK pelo Finder em vez de exchangedata)",
                log_ts()
            );
            reply.error(libc::EPERM);
            return;
        }
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
        // Preferir write_buffer (escritas não drenadas, inclui conteúdo posicionado
        // por exchange). Fallback no blob do HEAD.
        if let Some(buf) = self.write_buffers.lock().get(&ino).cloned() {
            let end = std::cmp::min(offset as usize + size as usize, buf.len());
            let start = (offset as usize).min(buf.len());
            reply.data(&buf[start..end]);
            return;
        }
        if let Some(entry) = self.files_by_inode.get(&ino) {
            let repo = match git::open_repo(&self.repo_path) {
                Ok(r) => r,
                Err(_) => {
                    reply.error(libc::EIO);
                    return;
                }
            };
            let commit = match git::get_commit(&repo, "HEAD") {
                Ok(c) => c,
                Err(_) => {
                    reply.error(libc::EIO);
                    return;
                }
            };
            let content = match git::read_file_at_commit(&repo, &commit, &entry.path) {
                Ok(v) => v,
                Err(_) => {
                    reply.error(libc::EIO);
                    return;
                }
            };
            let end = std::cmp::min(offset as usize + size as usize, content.len());
            let start = (offset as usize).min(content.len());
            reply.data(&content[start..end]);
        } else {
            reply.error(libc::ENOENT);
        }
    }
}

fn file_attr(ino: u64, size: u64, perm: u16, uid: u32, gid: u32, time: SystemTime) -> FileAttr {
    FileAttr {
        ino,
        size,
        // FUSE conta em unidades de 512 bytes, independente do blksize reportado.
        blocks: size.div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: FileType::RegularFile,
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
