use fuser::{Filesystem, Request, ReplyAttr, ReplyData, ReplyEntry, ReplyDirectory, FileAttr};
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};
use crate::git;

pub struct GitDriveFS {
    repo_path: String,
}

impl GitDriveFS {
    pub fn new(repo_path: &str) -> anyhow::Result<Self> {
        Ok(GitDriveFS {
            repo_path: repo_path.to_string(),
        })
    }
}

const TTL: Duration = Duration::from_secs(1);
const FILE_MODE: u32 = 0o644;

impl Filesystem for GitDriveFS {
    fn lookup(&mut self, _req: &Request, _parent: u64, name: &OsStr, reply: ReplyEntry) {
        let filename = name.to_str().unwrap_or("<invalid>");

        if filename == "README.md" {
            let repo = git::open_repo(&self.repo_path).unwrap();
            let commit = git::get_commit(&repo, "HEAD").unwrap();
            let content = git::read_file_at_commit(&repo, &commit, "README.md").unwrap();

            println!("🔍 [LOOKUP] {} → encontrado (tamanho: {} bytes)", filename, content.len());

            let attr = FileAttr {
                ino: 2,
                size: content.len() as u64,
                blocks: 1,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: fuser::FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };
            reply.entry(&TTL, &attr, 0);
        } else {
            println!("❌ [LOOKUP] {} → não encontrado", filename);
            reply.error(libc::ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let repo = git::open_repo(&self.repo_path).unwrap();
        let commit = git::get_commit(&repo, "HEAD").unwrap();
        let content = git::read_file_at_commit(&repo, &commit, "README.md").unwrap();

        println!("📄 [READ] README.md offset: {}, size: {}", offset, size);

        let end = std::cmp::min(offset as usize + size as usize, content.len());
        let slice = &content[offset as usize..end];
        reply.data(slice);
    }

    fn readdir(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        println!("📁 [READDIR] inode: 1, offset: {}", offset);

        if offset == 0 {
            let _ = reply.add(1, 0, fuser::FileType::Directory, ".");
            let _ = reply.add(1, 1, fuser::FileType::Directory, "..");
            let _ = reply.add(2, 2, fuser::FileType::RegularFile, "README.md");
        }

        reply.ok();
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino == 1 {
            println!("📦 [GETATTR] inode 1 → diretório raiz");

            let attr = FileAttr {
                ino,
                size: 0,
                blocks: 1,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: fuser::FileType::Directory,
                perm: 0o755,
                nlink: 1,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };
            reply.attr(&TTL, &attr);
        } else {
            let repo = git::open_repo(&self.repo_path).unwrap();
            let commit = git::get_commit(&repo, "HEAD").unwrap();
            let content = git::read_file_at_commit(&repo, &commit, "README.md").unwrap();

            println!("📦 [GETATTR] inode 2 → README.md ({} bytes)", content.len());

            let attr = FileAttr {
                ino,
                size: content.len() as u64,
                blocks: 1,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: fuser::FileType::RegularFile,
                perm: FILE_MODE as u16,
                nlink: 1,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };
            reply.attr(&TTL, &attr);
        }
    }
}