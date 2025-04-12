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
const FILE_MODE: u32 = 0o644; // Mantemos como u32, mas vamos converter para u16 onde necessário

impl Filesystem for GitDriveFS {
    fn lookup(&mut self, _req: &Request, _parent: u64, name: &OsStr, reply: ReplyEntry) {
        if name == "README.md" {
            let attr = FileAttr {
                ino: 2,
                size: 1024,
                blocks: 1,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: fuser::FileType::RegularFile,
                perm: FILE_MODE as u16, // conversão correta
                nlink: 1,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                flags: 0,
                blksize: 512,
            };
            reply.entry(&TTL, &attr, 0);
        } else {
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
        if offset == 0 {
            reply.add(1, 0, fuser::FileType::Directory, ".");
            reply.add(1, 1, fuser::FileType::Directory, "..");
            reply.add(2, 2, fuser::FileType::RegularFile, "README.md");
        }
        reply.ok();
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let attr = FileAttr {
            ino,
            size: 1024,
            blocks: 1,
            atime: SystemTime::now(),
            mtime: SystemTime::now(),
            ctime: SystemTime::now(),
            crtime: SystemTime::now(),
            kind: if ino == 1 {
                fuser::FileType::Directory
            } else {
                fuser::FileType::RegularFile
            },
            perm: if ino == 1 { 0o755 } else { FILE_MODE as u16 }, // conversão aqui também
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