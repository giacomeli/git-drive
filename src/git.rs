use git2::{Repository, ObjectType, Error};
use std::path::Path;


pub fn open_repo(path: &str) -> Result<Repository, Error> {
    Repository::open(path)
}

pub fn get_commit<'repo>(repo: &'repo Repository, revision: &str) -> Result<git2::Commit<'repo>, Error> {
    let obj = repo.revparse_single(revision)?;
    obj.peel_to_commit()
}

pub fn read_file_at_commit(repo: &Repository, commit: &git2::Commit, file_path: &str) -> Result<Vec<u8>, Error> {
    let tree = commit.tree()?;
    let entry = tree.get_path(Path::new(file_path))?;
    let object = entry.to_object(repo)?;
    let blob = object.as_blob().ok_or_else(|| Error::from_str("Not a blob"))?;
    Ok(blob.content().to_vec())
}

/// Tipo de uma entrada de arquivo numa tree do Git. Symlink é um blob com
/// filemode 120000 cujo conteúdo é o texto do alvo — sem distinguir o
/// filemode, um symlink seria achatado em arquivo regular.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Symlink,
}

/// Entrada de arquivo listada de uma tree (blob), com path completo.
#[derive(Debug, Clone)]
pub struct ListedFile {
    pub path: String,
    pub size: u64,
    pub kind: FileKind,
}

/// Filemode de symlink nas trees do Git (S_IFLNK).
const FILEMODE_SYMLINK: i32 = 0o120000;

pub fn list_files_at_commit(
    repo: &Repository,
    commit: &git2::Commit,
) -> Result<Vec<ListedFile>, Error> {
    let tree = commit.tree()?;
    let mut files = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(name) = entry.name() {
            if let Some(ObjectType::Blob) = entry.kind() {
                if let Ok(obj) = entry.to_object(repo) {
                    if let Some(blob) = obj.as_blob() {
                        let kind = if entry.filemode() == FILEMODE_SYMLINK {
                            FileKind::Symlink
                        } else {
                            FileKind::Regular
                        };
                        files.push(ListedFile {
                            path: format!("{}{}", root, name),
                            size: blob.size() as u64,
                            kind,
                        });
                    }
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    Ok(files)
}

/// Lista diretórios (entries do tipo Tree) em profundidade, retornando
/// paths completos sem barra final (ex: "foo", "foo/bar").
pub fn list_dirs_at_commit(
    _repo: &Repository,
    commit: &git2::Commit,
) -> Result<Vec<String>, Error> {
    let tree = commit.tree()?;
    let mut dirs = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(name) = entry.name() {
            if let Some(ObjectType::Tree) = entry.kind() {
                let full = format!("{}{}", root, name);
                dirs.push(full);
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    Ok(dirs)
}

/// Stage todo o working tree (add -A equivalente) e cria um commit no branch
/// indicado com a identidade fornecida. Retorna `Ok(None)` se não há diferença
/// entre a tree do working directory e a do HEAD (nada a commitar).
pub fn commit_working_tree(
    repo: &Repository,
    branch: &str,
    user_name: &str,
    user_email: &str,
    message: &str,
) -> Result<Option<git2::Oid>, Error> {
    let mut index = repo.index()?;
    // Callback que pula paths nested `.git/` quando o user cola um repositório
    // git completo dentro do mount (ex.: `athena-docs/` com `.git/objects/...`).
    // Sem o filtro, libgit2 trata `athena-docs/` como submodule sem registro
    // em `.gitmodules` e retorna `invalid path: 'athena-docs/'; class=Index (10)`.
    // O commit inteiro falhava em loop, deixando os arquivos como untracked.
    // Com o filtro, o conteúdo de `athena-docs/` (Dockerfile, _media/, etc.) é
    // commitado normalmente; só o subdir `.git/` interno é deixado de fora.
    let mut filter = |path: &std::path::Path, _matched: &[u8]| -> i32 {
        let s = path.to_string_lossy();
        if s == ".git"
            || s.starts_with(".git/")
            || s.contains("/.git/")
            || s.ends_with("/.git")
        {
            1 // skip
        } else {
            0 // include
        }
    };
    index.add_all(
        ["*"].iter(),
        git2::IndexAddOption::DEFAULT,
        Some(&mut filter),
    )?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let parent_commit = repo.head()?.peel_to_commit().ok();
    if let Some(ref parent) = parent_commit {
        if parent.tree_id() == tree_id {
            return Ok(None);
        }
    }

    let sig = git2::Signature::now(user_name, user_email)?;
    let parents: Vec<&git2::Commit> = parent_commit.as_ref().map(|c| vec![c]).unwrap_or_default();
    let ref_name = format!("refs/heads/{}", branch);
    let oid = repo.commit(Some(&ref_name), &sig, &sig, message, &tree, &parents)?;
    Ok(Some(oid))
}

/// Dá push de `refs/heads/<branch>:refs/heads/<branch>` para o remote indicado.
/// Se `token` for Some, usa autenticação HTTPS userpass ("x-access-token").
/// Caso contrário, delega ao sistema (ssh-agent / credential helper).
pub fn push_to_remote(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    let token_owned = token.map(|t| t.to_string());
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        if let Some(ref t) = token_owned {
            git2::Cred::userpass_plaintext("x-access-token", t)
        } else if let Some(username) = username_from_url {
            git2::Cred::ssh_key_from_agent(username).or_else(|_| git2::Cred::default())
        } else {
            git2::Cred::default()
        }
    });
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(callbacks);
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
    remote.push(&[&refspec], Some(&mut push_opts))
}

/// Faz `git fetch` do remote indicado (apenas o branch requerido). Reutiliza
/// a mesma estratégia de credenciais de `push_to_remote`.
pub fn fetch_remote(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    let token_owned = token.map(|t| t.to_string());
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        if let Some(ref t) = token_owned {
            git2::Cred::userpass_plaintext("x-access-token", t)
        } else if let Some(username) = username_from_url {
            git2::Cred::ssh_key_from_agent(username).or_else(|_| git2::Cred::default())
        } else {
            git2::Cred::default()
        }
    });
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    remote.fetch(&[branch], Some(&mut fetch_opts), None)
}

/// Tenta aplicar um fast-forward do `origin/<branch>` sobre o HEAD local.
///
/// - `Ok(true)` se um fast-forward foi aplicado (HEAD e working tree avançaram).
/// - `Ok(false)` se o local já está na mesma revisão do remote.
/// - `Err(_)` se os históricos divergiram (merge real necessário) ou em caso
///   de erro de I/O / referência ausente.
pub fn merge_ff_only(repo: &Repository, branch: &str) -> Result<bool, Error> {
    let remote_ref_name = format!("refs/remotes/origin/{}", branch);
    let remote_ref = repo.find_reference(&remote_ref_name)?;
    let fetch_commit = repo.reference_to_annotated_commit(&remote_ref)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(false);
    }
    if !analysis.is_fast_forward() {
        return Err(Error::from_str(
            "merge não-FF: históricos divergiram, resolução manual necessária",
        ));
    }

    let local_ref_name = format!("refs/heads/{}", branch);
    let mut local_ref = repo.find_reference(&local_ref_name)?;
    let target_oid = fetch_commit.id();
    local_ref.set_target(target_oid, "drive-sync: fast-forward")?;
    repo.set_head(&local_ref_name)?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::default().force(),
    ))?;
    Ok(true)
}

/// Resultado de uma tentativa de pull (fetch + merge).
#[derive(Debug, Clone)]
pub enum MergeOutcome {
    /// Remote não tem o branch ou local já está na mesma revisão.
    NoOp,
    /// Fast-forward aplicado — HEAD e working tree avançaram para `refs/remotes/.../<branch>`.
    FastForwarded,
    /// Merge 3-way aplicado sem conflitos, com commit de merge criado.
    Merged,
    /// Merge 3-way com conflitos. Para cada path conflitado, a versão local foi
    /// preservada em `<path>.<suffix>` e a versão remota foi aceita como canônica.
    Conflicted {
        /// Paths originais que conflitavam (antes da resolução).
        resolved: Vec<String>,
        /// Sufixo usado para as conflicted copies (sem o `.` inicial).
        suffix: String,
    },
}

/// Faz merge do `refs/remotes/<remote>/<branch>` sobre o HEAD local.
///
/// Comportamento:
/// - Se o remote não tem o branch ou já estamos na mesma revisão → `NoOp`.
/// - Se pode ser fast-forward → aplica e retorna `FastForwarded`.
/// - Se é divergência real → tenta merge 3-way.
///   - Sem conflitos → cria merge commit e retorna `Merged`.
///   - Com conflitos → para cada arquivo conflitado, salva a versão local
///     em `<path>.<conflict_suffix>`, aceita a versão remota como canônica,
///     e cria commit de merge. Retorna `Conflicted`.
///
/// Requer identidade (`user_name`/`user_email`) para criar merge commits.
pub fn pull_and_merge(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    user_name: &str,
    user_email: &str,
    conflict_suffix: &str,
) -> Result<MergeOutcome, Error> {
    let remote_ref_name = format!("refs/remotes/{}/{}", remote_name, branch);
    let remote_ref = match repo.find_reference(&remote_ref_name) {
        Ok(r) => r,
        Err(_) => return Ok(MergeOutcome::NoOp),
    };
    let fetch_commit = repo.reference_to_annotated_commit(&remote_ref)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(MergeOutcome::NoOp);
    }

    if analysis.is_fast_forward() {
        let local_ref_name = format!("refs/heads/{}", branch);
        let mut local_ref = repo.find_reference(&local_ref_name)?;
        local_ref.set_target(fetch_commit.id(), "drive-sync: fast-forward")?;
        repo.set_head(&local_ref_name)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
        return Ok(MergeOutcome::FastForwarded);
    }

    if !analysis.is_normal() {
        return Err(Error::from_str("merge analysis: estado inesperado"));
    }

    repo.merge(&[&fetch_commit], None, None)?;
    let mut index = repo.index()?;
    let mut conflicted_paths: Vec<String> = Vec::new();

    if index.has_conflicts() {
        let workdir = repo
            .workdir()
            .ok_or_else(|| Error::from_str("repo sem workdir (bare?) — merge inviável"))?
            .to_path_buf();

        let conflicts: Vec<(Option<git2::IndexEntry>, Option<git2::IndexEntry>)> = {
            let iter = index.conflicts()?;
            iter.filter_map(|c| c.ok())
                .map(|c| (c.our, c.their))
                .collect()
        };

        for (our, their) in conflicts {
            let path_bytes = our
                .as_ref()
                .or(their.as_ref())
                .map(|e| e.path.clone())
                .ok_or_else(|| Error::from_str("conflito sem path identificável"))?;
            let path_str = String::from_utf8(path_bytes)
                .map_err(|_| Error::from_str("path com bytes inválidos"))?;

            if let Some(our_entry) = &our {
                let our_blob = repo.find_blob(our_entry.id)?;
                let conflict_name = format!("{}.{}", path_str, conflict_suffix);
                let full_conflict = workdir.join(&conflict_name);
                if let Some(parent) = full_conflict.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::from_str(&format!("create_dir: {}", e)))?;
                }
                std::fs::write(&full_conflict, our_blob.content())
                    .map_err(|e| Error::from_str(&format!("write conflict copy: {}", e)))?;
                index.add_path(Path::new(&conflict_name))?;
            }

            let full = workdir.join(&path_str);
            if let Some(their_entry) = &their {
                let their_blob = repo.find_blob(their_entry.id)?;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::from_str(&format!("create_dir: {}", e)))?;
                }
                std::fs::write(&full, their_blob.content())
                    .map_err(|e| Error::from_str(&format!("write their: {}", e)))?;
                index.add_path(Path::new(&path_str))?;
            } else {
                let _ = std::fs::remove_file(&full);
                index.remove_path(Path::new(&path_str))?;
            }

            conflicted_paths.push(path_str);
        }

        index.write()?;
    }

    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = git2::Signature::now(user_name, user_email)?;

    let head_commit = repo.head()?.peel_to_commit()?;
    let their_commit = repo.find_commit(fetch_commit.id())?;

    let message = if conflicted_paths.is_empty() {
        format!("drive-sync: merge {}/{}", remote_name, branch)
    } else {
        format!(
            "drive-sync: merge {}/{} com {} conflito(s) — locais preservados como *.{}",
            remote_name,
            branch,
            conflicted_paths.len(),
            conflict_suffix
        )
    };

    let ref_name = format!("refs/heads/{}", branch);
    repo.commit(
        Some(&ref_name),
        &sig,
        &sig,
        &message,
        &tree,
        &[&head_commit, &their_commit],
    )?;

    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    repo.cleanup_state()?;

    if conflicted_paths.is_empty() {
        Ok(MergeOutcome::Merged)
    } else {
        Ok(MergeOutcome::Conflicted {
            resolved: conflicted_paths,
            suffix: conflict_suffix.to_string(),
        })
    }
}

/// Lista arquivos do working tree que casam com regras de ignore
/// (`.gitignore`, `info/exclude`, `core.excludesfile`) e NÃO estão no índice.
/// São os candidatos a "local-only visível": existem no disco, nunca entram
/// em commit. Symlinks vêm com `FileKind::Symlink` e size = len do alvo.
pub fn list_ignored_files(repo: &Repository) -> Result<Vec<ListedFile>, Error> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::from_str("repo sem workdir (bare?)"))?
        .to_path_buf();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut out = Vec::new();
    for entry in statuses.iter() {
        if !entry.status().contains(git2::Status::IGNORED) {
            continue;
        }
        let Some(path) = entry.path() else { continue };
        let full = workdir.join(path);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let size = std::fs::read_link(&full)
                .map(|t| t.as_os_str().len() as u64)
                .unwrap_or(0);
            out.push(ListedFile {
                path: path.to_string(),
                size,
                kind: FileKind::Symlink,
            });
        } else if meta.is_file() {
            out.push(ListedFile {
                path: path.to_string(),
                size: meta.len(),
                kind: FileKind::Regular,
            });
        }
        // Diretórios ignorados não entram: os arquivos internos já vêm
        // individualmente via recurse_ignored_dirs.
    }
    Ok(out)
}

/// `true` se o working tree não tem NENHUMA mudança pendente (staged,
/// unstaged ou untracked; ignorados ficam de fora). Guarda do pull watcher:
/// merge/checkout force só podem rodar com tree limpa — senão mudanças locais
/// não commitadas seriam destruídas (RF6 da Fase 0).
pub fn is_working_tree_clean(repo: &Repository) -> Result<bool, Error> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses.is_empty())
}

pub fn current_branch_name(repo: &Repository) -> Result<String, Error> {
    let head = repo.head()?;
    if !head.is_branch() {
        return Err(Error::from_str(
            "HEAD está detached — drive-sync requer um branch",
        ));
    }
    head.shorthand()
        .map(|s| s.to_string())
        .ok_or_else(|| Error::from_str("branch sem nome (shorthand vazio)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn init_repo_with_commit() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let sig = Signature::now("Test", "test@example.com").unwrap();
            let tree_id = {
                let mut index = repo.index().unwrap();
                index.write_tree().unwrap()
            };
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn current_branch_name_returns_default_branch_on_fresh_repo() {
        let (_dir, repo) = init_repo_with_commit();
        let name = current_branch_name(&repo).unwrap();
        assert!(
            name == "main" || name == "master",
            "esperado main ou master, obtido {}",
            name
        );
    }

    #[test]
    fn current_branch_name_after_checkout_new_branch() {
        let (_dir, repo) = init_repo_with_commit();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/x", &head_commit, false).unwrap();
        repo.set_head("refs/heads/feature/x").unwrap();
        assert_eq!(current_branch_name(&repo).unwrap(), "feature/x");
    }

    #[test]
    fn current_branch_name_returns_err_on_detached_head() {
        let (_dir, repo) = init_repo_with_commit();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.set_head_detached(head_commit.id()).unwrap();
        assert!(current_branch_name(&repo).is_err());
    }

    fn commit_file(repo: &Repository, relpath: &str, content: &[u8]) {
        let workdir = repo.workdir().unwrap();
        let full = workdir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add", &tree, &[&parent_commit])
            .unwrap();
    }

    /// Cria um symlink real no working tree e o commita (entrada 120000).
    fn commit_symlink(repo: &Repository, relpath: &str, target: &str) {
        let workdir = repo.workdir().unwrap();
        let full = workdir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::os::unix::fs::symlink(target, &full).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add symlink", &tree, &[&parent_commit])
            .unwrap();
    }

    #[test]
    fn list_files_at_commit_distinguishes_symlink_kind() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "regular.txt", b"data");
        commit_symlink(&repo, "link.txt", "regular.txt");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();

        let regular = files.iter().find(|f| f.path == "regular.txt").unwrap();
        let link = files.iter().find(|f| f.path == "link.txt").unwrap();
        assert_eq!(regular.kind, FileKind::Regular);
        assert_eq!(link.kind, FileKind::Symlink);
        // O blob de um symlink é o texto do alvo.
        assert_eq!(link.size, "regular.txt".len() as u64);
    }

    #[test]
    fn list_files_at_commit_nested_symlink_kind() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "docs/real.md", b"conteudo");
        commit_symlink(&repo, "docs/alias.md", "real.md");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();
        let link = files.iter().find(|f| f.path == "docs/alias.md").unwrap();
        assert_eq!(link.kind, FileKind::Symlink);
    }

    #[test]
    fn list_files_at_commit_flat_repo() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"aa");
        commit_file(&repo, "b.txt", b"bbb");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut files: Vec<(String, u64)> = list_files_at_commit(&repo, &commit)
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![("a.txt".to_string(), 2), ("b.txt".to_string(), 3)]
        );
    }

    #[test]
    fn list_files_at_commit_nested() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        commit_file(&repo, "foo/b.txt", b"yy");
        commit_file(&repo, "foo/bar/c.txt", b"zzz");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut files: Vec<(String, u64)> = list_files_at_commit(&repo, &commit)
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                ("a.txt".to_string(), 1),
                ("foo/b.txt".to_string(), 2),
                ("foo/bar/c.txt".to_string(), 3),
            ]
        );
    }

    #[test]
    fn list_files_at_commit_empty_tree() {
        let (_dir, repo) = init_repo_with_commit();
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn list_dirs_at_commit_nested() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        commit_file(&repo, "foo/b.txt", b"y");
        commit_file(&repo, "foo/bar/c.txt", b"z");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut dirs = list_dirs_at_commit(&repo, &commit).unwrap();
        dirs.sort();
        assert_eq!(dirs, vec!["foo".to_string(), "foo/bar".to_string()]);
    }

    #[test]
    fn list_dirs_at_commit_empty_on_flat_repo() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let dirs = list_dirs_at_commit(&repo, &commit).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn commit_working_tree_creates_commit() {
        let (dir, repo) = init_repo_with_commit();
        std::fs::write(dir.path().join("novo.txt"), b"hello").unwrap();

        let branch = current_branch_name(&repo).unwrap();
        let oid = commit_working_tree(
            &repo,
            &branch,
            "Alice",
            "alice@example.com",
            "manual add",
        )
        .unwrap()
        .expect("deve retornar Some (há mudanças)");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id(), oid);
        assert_eq!(head.message().unwrap(), "manual add");
        assert_eq!(head.author().name().unwrap(), "Alice");
    }

    #[test]
    fn commit_working_tree_returns_none_when_clean() {
        let (_dir, repo) = init_repo_with_commit();
        let branch = current_branch_name(&repo).unwrap();
        let res = commit_working_tree(&repo, &branch, "Alice", "alice@example.com", "noop")
            .unwrap();
        assert!(res.is_none(), "esperado None quando sem mudanças");
    }

    fn setup_local_with_bare_remote() -> (tempfile::TempDir, Repository, tempfile::TempDir) {
        let (dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        let url = format!("file://{}", bare.path().to_str().unwrap());
        repo.remote("origin", &url).unwrap();
        let branch = current_branch_name(&repo).unwrap();
        push_to_remote(&repo, &branch, "origin", None).unwrap();
        (dir, repo, bare)
    }

    #[test]
    fn fetch_remote_brings_new_commits() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        // Outro clone (B) faz mudança e push
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"hello");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        // A faz fetch
        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let remote_ref = repo_a
            .find_reference(&format!("refs/remotes/origin/{}", branch))
            .unwrap();
        let remote_commit = remote_ref.peel_to_commit().unwrap();
        assert_eq!(remote_commit.message().unwrap().trim(), "add");
    }

    #[test]
    fn merge_ff_only_applies_fast_forward() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"hi");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let result = merge_ff_only(&repo_a, &branch).unwrap();
        assert!(result, "esperado Ok(true) para fast-forward aplicado");

        // Working tree de A agora tem from_b.txt
        let workdir = repo_a.workdir().unwrap();
        assert!(workdir.join("from_b.txt").exists());
    }

    #[test]
    fn merge_ff_only_returns_false_when_up_to_date() {
        let (_local, repo, _bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo).unwrap();
        fetch_remote(&repo, &branch, "origin", None).unwrap();
        let result = merge_ff_only(&repo, &branch).unwrap();
        assert!(!result, "esperado Ok(false) quando já atualizado");
    }

    #[test]
    fn merge_ff_only_returns_err_on_divergence() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();

        // A faz commit local divergente (não publica)
        commit_file(&repo_a, "local_only.txt", b"a");

        // B publica um commit também
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"b");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        assert!(
            merge_ff_only(&repo_a, &branch).is_err(),
            "esperado erro quando divergente"
        );
    }

    fn clone_of(bare: &tempfile::TempDir, author: &str) -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir.path(),
        )
        .unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", author).unwrap();
            cfg.set_str("user.email", &format!("{}@x.com", author)).unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn pull_and_merge_noop_when_remote_branch_missing() {
        let (_dir, repo) = init_repo_with_commit();
        let branch = current_branch_name(&repo).unwrap();
        let outcome =
            pull_and_merge(&repo, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::NoOp));
    }

    #[test]
    fn pull_and_merge_noop_when_up_to_date() {
        let (_local, repo, _bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo).unwrap();
        fetch_remote(&repo, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::NoOp));
    }

    #[test]
    fn pull_and_merge_fast_forwards_when_only_remote_advanced() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "from_b.txt", b"hi");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::FastForwarded));
        assert!(repo_a.workdir().unwrap().join("from_b.txt").exists());
    }

    #[test]
    fn pull_and_merge_3way_merges_when_different_files_touched() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        {
            let mut cfg = repo_a.config().unwrap();
            cfg.set_str("user.name", "Alice").unwrap();
            cfg.set_str("user.email", "a@x.com").unwrap();
        }
        commit_file(&repo_a, "only_a.txt", b"from A");

        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "only_b.txt", b"from B");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "Alice", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::Merged), "esperado Merged, obtido {:?}", outcome);

        let wd = repo_a.workdir().unwrap();
        assert!(wd.join("only_a.txt").exists(), "arquivo local A deve persistir");
        assert!(wd.join("only_b.txt").exists(), "arquivo remoto B deve chegar");

        let head = repo_a.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2, "merge commit deve ter 2 pais");
    }

    #[test]
    fn pull_and_merge_conflict_creates_conflict_copy_and_accepts_remote() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        {
            let mut cfg = repo_a.config().unwrap();
            cfg.set_str("user.name", "Alice").unwrap();
            cfg.set_str("user.email", "a@x.com").unwrap();
        }
        // Arquivo "shared.txt" existe em ambos os lados com conteúdo diferente
        commit_file(&repo_a, "shared.txt", b"local version");

        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "shared.txt", b"remote version");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "Alice", "a@x.com", "conflict-xyz").unwrap();

        match outcome {
            MergeOutcome::Conflicted { resolved, suffix } => {
                assert_eq!(resolved, vec!["shared.txt".to_string()]);
                assert_eq!(suffix, "conflict-xyz");
            }
            other => panic!("esperado Conflicted, obtido {:?}", other),
        }

        let wd = repo_a.workdir().unwrap();
        let canonical = std::fs::read(wd.join("shared.txt")).unwrap();
        assert_eq!(canonical, b"remote version", "canônico deve ser a versão remota");
        let conflict_copy = std::fs::read(wd.join("shared.txt.conflict-xyz")).unwrap();
        assert_eq!(conflict_copy, b"local version", "cópia de conflito deve ter a versão local");

        let head = repo_a.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2, "merge commit de conflito deve ter 2 pais");
        // Estado de merge deve estar limpo (sem MERGE_HEAD pendurado)
        assert!(repo_a.state() == git2::RepositoryState::Clean);
    }

    #[test]
    fn is_working_tree_clean_true_on_fresh_commit() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        assert!(is_working_tree_clean(&repo).unwrap());
    }

    #[test]
    fn is_working_tree_clean_false_on_modified_tracked_file() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        std::fs::write(dir.path().join("a.txt"), b"editado").unwrap();
        assert!(!is_working_tree_clean(&repo).unwrap());
    }

    #[test]
    fn is_working_tree_clean_false_on_untracked_file() {
        let (dir, repo) = init_repo_with_commit();
        std::fs::write(dir.path().join("novo.txt"), b"untracked").unwrap();
        assert!(!is_working_tree_clean(&repo).unwrap());
    }

    // ---- Fase 2.5, Bloco A: política de commit e ignorados ----

    #[test]
    fn is_working_tree_clean_true_with_only_ignored_files() {
        // Pin do comportamento: ignorados não bloqueiam a guarda do pull.
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\n");
        std::fs::write(dir.path().join("app.log"), b"ignorado").unwrap();
        assert!(
            is_working_tree_clean(&repo).unwrap(),
            "arquivo ignorado não pode sujar a working tree para a guarda do pull"
        );
    }

    #[test]
    fn commit_working_tree_skips_ignored_file() {
        // Pin do comportamento: IndexAddOption::DEFAULT respeita as regras de
        // ignore — antes era efeito colateral, agora é especificado.
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\n");
        std::fs::write(dir.path().join("app.log"), b"ignorado").unwrap();
        std::fs::write(dir.path().join("real.txt"), b"real").unwrap();

        let branch = current_branch_name(&repo).unwrap();
        let oid = commit_working_tree(&repo, &branch, "A", "a@x.com", "batch")
            .unwrap()
            .expect("real.txt deve gerar commit");
        let commit = repo.find_commit(oid).unwrap();
        let tree = commit.tree().unwrap();
        assert!(tree.get_path(Path::new("real.txt")).is_ok());
        assert!(
            tree.get_path(Path::new("app.log")).is_err(),
            "app.log é ignorado e não pode entrar no commit"
        );
    }

    #[test]
    fn list_ignored_files_reports_untracked_ignored_with_sizes() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\nbuild/\n");
        std::fs::write(dir.path().join("app.log"), b"12345").unwrap();
        std::fs::create_dir_all(dir.path().join("build")).unwrap();
        std::fs::write(dir.path().join("build/out.bin"), b"123").unwrap();
        // Untracked NÃO-ignorado fica de fora
        std::fs::write(dir.path().join("real.txt"), b"x").unwrap();

        let mut files = list_ignored_files(&repo).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: Vec<(&str, u64)> = files.iter().map(|f| (f.path.as_str(), f.size)).collect();
        assert_eq!(
            paths,
            vec![("app.log", 5), ("build/out.bin", 3)],
            "só ignorados untracked, com tamanhos do disco"
        );
        assert!(files.iter().all(|f| f.kind == FileKind::Regular));
    }

    #[test]
    fn list_ignored_files_reports_symlink_kind() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.lnk\n");
        std::os::unix::fs::symlink("alvo.txt", dir.path().join("a.lnk")).unwrap();
        let files = list_ignored_files(&repo).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.lnk");
        assert_eq!(files[0].kind, FileKind::Symlink);
        assert_eq!(files[0].size, "alvo.txt".len() as u64);
    }

    #[test]
    fn push_to_remote_via_bare_remote_succeeds() {
        let (dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        let url = format!("file://{}", bare.path().to_str().unwrap());
        repo.remote("origin", &url).unwrap();

        commit_file(&repo, "a.txt", b"hello");
        let branch = current_branch_name(&repo).unwrap();
        push_to_remote(&repo, &branch, "origin", None).unwrap();

        let bare_repo = Repository::open(bare.path()).unwrap();
        let bare_ref = bare_repo
            .find_reference(&format!("refs/heads/{}", branch))
            .unwrap();
        let bare_head = bare_ref.peel_to_commit().unwrap();
        let local_head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(bare_head.id(), local_head.id());
        drop(dir);
    }
}