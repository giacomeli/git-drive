//! Device registry of the P2P channel: one file per device at
//! `.git-drive/peers/<id>`, where the id is the device's public key in 64
//! lowercase hex characters. One file per device means two devices
//! registering at once never conflict: the merge just joins the entries.
//! The content is `{"name": ...}`, sealed with the drive key on an encrypted
//! drive; the id itself is a public key and stays readable.

use std::path::Path;

use git2::Repository;
use serde::{Deserialize, Serialize};

use crate::crypt::Crypto;

/// Where the entries live (under the reserved `.git-drive/`).
pub const PEERS_DIR: &str = ".git-drive/peers";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub id: String,
    /// Empty when the content cannot be read (sealed with a key we lack).
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct EntryFile {
    name: String,
}

/// Whether `s` is a device id: 64 lowercase hex characters.
pub fn is_device_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The devices registered in HEAD's tree, sorted by id. Files whose name is
/// not a device id are ignored; an unreadable content only loses the name.
pub fn list(repo: &Repository, crypto: Option<&Crypto>) -> Result<Vec<DeviceEntry>, git2::Error> {
    let Ok(tree) = repo.head().and_then(|h| h.peel_to_tree()) else {
        return Ok(Vec::new());
    };
    let Ok(dir) = tree.get_path(Path::new(PEERS_DIR)) else {
        return Ok(Vec::new());
    };
    let Ok(dir_tree) = dir.to_object(repo).and_then(|o| o.peel_to_tree()) else {
        return Ok(Vec::new());
    };
    let mut entries: Vec<DeviceEntry> = dir_tree
        .iter()
        .filter_map(|entry| {
            let id = entry.name().filter(|n| is_device_id(n))?.to_string();
            let name = repo
                .find_blob(entry.id())
                .ok()
                .and_then(|blob| decode_entry(blob.content(), crypto))
                .unwrap_or_default();
            Some(DeviceEntry { id, name })
        })
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

/// Name to show for an entry: the one read from the registry, else the one
/// the daemon published in its status (it holds the key; a CLI run with
/// another config may not), else `?`.
pub fn display_name(entry: &DeviceEntry, p2p: Option<&crate::status::P2pInfo>) -> String {
    if !entry.name.is_empty() {
        return entry.name.clone();
    }
    let published = p2p.and_then(|p| {
        if p.device_id == entry.id {
            Some(p.device_name.clone())
        } else {
            p.peers.iter().find(|peer| peer.id == entry.id).map(|peer| peer.name.clone())
        }
    });
    published.filter(|n| !n.is_empty()).unwrap_or_else(|| "?".to_string())
}

fn decode_entry(bytes: &[u8], crypto: Option<&Crypto>) -> Option<String> {
    let opened = crypto.and_then(|c| c.open(bytes).ok());
    let json = opened.as_deref().unwrap_or(bytes);
    serde_json::from_slice::<EntryFile>(json).ok().map(|e| e.name)
}

/// Adds this device to the registry with its own commit, unless HEAD already
/// lists it. Returns whether a commit was made. The caller holds the repo
/// lock. Refuses a dirty working tree: the commit would carry pending
/// changes under a registry message.
pub fn register_self(
    repo_path: &Path,
    id: &str,
    name: &str,
    crypto: Option<&Crypto>,
    identity: (&str, &str),
) -> Result<bool, git2::Error> {
    if !is_device_id(id) {
        return Err(git2::Error::from_str(&format!("not a device id: {:?}", id)));
    }
    let repo = Repository::open(repo_path)?;
    if list(&repo, None)?.iter().any(|e| e.id == id) {
        return Ok(false);
    }
    ensure_clean(&repo)?;
    let json = serde_json::to_vec(&EntryFile { name: name.to_string() })
        .expect("a registry entry always serializes");
    let content = match crypto {
        Some(c) => c.seal(&json),
        None => json,
    };
    let path = repo_path.join(PEERS_DIR).join(id);
    std::fs::create_dir_all(path.parent().expect("entry has a parent"))
        .and_then(|()| std::fs::write(&path, content))
        .map_err(|e| git2::Error::from_str(&format!("writing {}: {}", path.display(), e)))?;
    commit(&repo, "register", name, crypto.is_some(), identity)?;
    Ok(true)
}

/// Removes a device (by id or name) from the registry with its own commit.
/// Returns the removed id. The daemon must not be running: there is no lock
/// shared with it.
pub fn remove(
    repo_path: &Path,
    id_or_name: &str,
    crypto: Option<&Crypto>,
    identity: (&str, &str),
) -> Result<String, git2::Error> {
    let repo = Repository::open(repo_path)?;
    let entries = list(&repo, crypto)?;
    let found: Vec<&DeviceEntry> = entries
        .iter()
        .filter(|e| e.id == id_or_name || e.name == id_or_name)
        .collect();
    let entry = match found.as_slice() {
        [one] => *one,
        [] => return Err(git2::Error::from_str(&format!("no device {:?} in the registry", id_or_name))),
        _ => return Err(git2::Error::from_str(&format!("{:?} matches several devices; use the id", id_or_name))),
    };
    ensure_clean(&repo)?;
    let path = repo_path.join(PEERS_DIR).join(&entry.id);
    std::fs::remove_file(&path)
        .map_err(|e| git2::Error::from_str(&format!("removing {}: {}", path.display(), e)))?;
    commit(&repo, "remove", &entry.name, crypto.is_some(), identity)?;
    Ok(entry.id.clone())
}

fn ensure_clean(repo: &Repository) -> Result<(), git2::Error> {
    if crate::git::is_working_tree_clean(repo)? {
        Ok(())
    } else {
        Err(git2::Error::from_str(
            "the working tree has uncommitted changes — let the daemon commit them first",
        ))
    }
}

fn commit(
    repo: &Repository,
    verb: &str,
    name: &str,
    encrypted: bool,
    identity: (&str, &str),
) -> Result<(), git2::Error> {
    // Same rule as the drain: names never reach a commit message of an
    // encrypted drive.
    let message = if encrypted || name.is_empty() {
        format!("git-drive: {} device", verb)
    } else {
        format!("git-drive: {} device {}", verb, name)
    };
    let branch = crate::git::current_branch_name(repo)?;
    crate::git::commit_working_tree(repo, &branch, identity.0, identity.1, &message)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};

    const ID_A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const ID_B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";
    const IDENTITY: (&str, &str) = ("Alice", "a@x.com");

    fn repo_with_commit() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let sig = Signature::now("T", "t@x.com").unwrap();
            let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        }
        (dir, repo)
    }

    fn crypto() -> crate::crypt::Crypto {
        crate::crypt::Crypto::from_master_key(&[7u8; 32])
    }

    fn head_message(repo: &Repository) -> String {
        repo.head().unwrap().peel_to_commit().unwrap().message().unwrap().to_string()
    }

    #[test]
    fn registering_commits_once_and_lists_the_device() {
        let (dir, repo) = repo_with_commit();
        assert!(register_self(dir.path(), ID_A, "mac", None, IDENTITY).unwrap());
        assert_eq!(head_message(&repo), "git-drive: register device mac");
        let head = repo.head().unwrap().target().unwrap();
        assert!(!register_self(dir.path(), ID_A, "mac", None, IDENTITY).unwrap(), "already there");
        assert_eq!(repo.head().unwrap().target().unwrap(), head, "no second commit");
        assert_eq!(list(&repo, None).unwrap(), vec![DeviceEntry { id: ID_A.into(), name: "mac".into() }]);
    }

    #[test]
    fn encrypted_drive_seals_the_name_and_redacts_the_message() {
        let (dir, repo) = repo_with_commit();
        let c = crypto();
        register_self(dir.path(), ID_A, "mac", Some(&c), IDENTITY).unwrap();
        assert_eq!(head_message(&repo), "git-drive: register device");
        let raw = std::fs::read(dir.path().join(PEERS_DIR).join(ID_A)).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("mac"), "name sealed in the repo");
        assert_eq!(list(&repo, Some(&c)).unwrap()[0].name, "mac");
        assert_eq!(list(&repo, None).unwrap()[0].id, ID_A, "the id is readable without the key");
    }

    #[test]
    fn listing_ignores_files_that_are_not_device_ids() {
        let (dir, repo) = repo_with_commit();
        register_self(dir.path(), ID_B, "allmight", None, IDENTITY).unwrap();
        std::fs::write(dir.path().join(PEERS_DIR).join("notes.txt"), b"x").unwrap();
        let branch = crate::git::current_branch_name(&repo).unwrap();
        crate::git::commit_working_tree(&repo, &branch, "T", "t@x.com", "noise").unwrap();
        let ids: Vec<String> = list(&repo, None).unwrap().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![ID_B.to_string()]);
    }

    #[test]
    fn display_name_falls_back_to_what_the_daemon_published() {
        let p2p = crate::status::P2pInfo {
            online: true,
            device_id: ID_A.into(),
            device_name: "mac".into(),
            peers: vec![crate::status::PeerInfo {
                name: "allmight".into(),
                id: ID_B.into(),
                online: true,
                last_seen: None,
            }],
        };
        let entry = |id: &str, name: &str| DeviceEntry { id: id.into(), name: name.into() };
        assert_eq!(display_name(&entry(ID_B, "read"), Some(&p2p)), "read", "own read wins");
        assert_eq!(display_name(&entry(ID_B, ""), Some(&p2p)), "allmight", "sealed: daemon's name");
        assert_eq!(display_name(&entry(ID_A, ""), Some(&p2p)), "mac", "this device");
        assert_eq!(display_name(&entry(ID_B, ""), None), "?", "nothing to go on");
    }

    #[test]
    fn remove_by_name_or_id_commits_the_deletion() {
        let (dir, repo) = repo_with_commit();
        register_self(dir.path(), ID_A, "mac", None, IDENTITY).unwrap();
        register_self(dir.path(), ID_B, "allmight", None, IDENTITY).unwrap();
        assert_eq!(remove(dir.path(), "mac", None, IDENTITY).unwrap(), ID_A);
        assert_eq!(remove(dir.path(), ID_B, None, IDENTITY).unwrap(), ID_B);
        assert!(list(&repo, None).unwrap().is_empty());
        assert!(remove(dir.path(), "nobody", None, IDENTITY).is_err());
    }
}
