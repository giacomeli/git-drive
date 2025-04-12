use git2::{Repository, Error};
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