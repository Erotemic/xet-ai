use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use git2::{ObjectType, Repository, TreeWalkMode, TreeWalkResult};

pub fn open_repo(repo_root_or_cwd: &Path) -> Result<Repository> {
    Ok(Repository::discover(repo_root_or_cwd)?)
}

pub fn repo_root(repo_root_or_cwd: &Path) -> Result<PathBuf> {
    let repo = open_repo(repo_root_or_cwd)?;
    repo.workdir()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("bare repositories are not supported"))
}

pub fn head_sha(repo: &Repository) -> Result<String> {
    let oid = repo.head()?.target().ok_or_else(|| anyhow!("invalid HEAD"))?;
    Ok(oid.to_string())
}

pub fn head_refname_or_head(repo: &Repository) -> Result<String> {
    let head = repo.head()?;
    if head.is_branch() {
        Ok(head
            .shorthand()
            .map(str::to_string)
            .unwrap_or_else(|| "HEAD".to_string()))
    } else {
        Ok("HEAD".to_string())
    }
}

pub fn read_file_at_commit(repo: &Repository, commit_sha: &str, path: &str) -> Result<Vec<u8>> {
    let commit = repo.find_commit(git2::Oid::from_str(commit_sha)?)?;
    let tree = commit.tree()?;
    let entry = tree.get_path(Path::new(path))?;
    let blob = repo.find_blob(entry.id())?;
    Ok(blob.content().to_vec())
}

pub fn list_files_at_commit(repo: &Repository, commit_sha: &str) -> Result<Vec<String>> {
    let commit = repo.find_commit(git2::Oid::from_str(commit_sha)?)?;
    let tree = commit.tree()?;
    let mut files = Vec::new();

    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            files.push(format!("{}{}", root, entry.name().unwrap_or_default()));
        }
        TreeWalkResult::Ok
    })?;

    files.sort();
    Ok(files)
}
