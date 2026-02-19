use std::fs;
use std::path::Path;

use anyhow::Result;
use data::XetFileInfo;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::sync::atomic_write_string;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerEntry {
    pub path: String,
    pub file_info: XetFileInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerIndex {
    pub repo_id: String,
    pub git_sha: String,
    pub entries: Vec<PointerEntry>,
}

pub fn build_pointer_index(repo_root: &Path, git_sha: &str, repo_id: &str) -> Result<PointerIndex> {
    let repo = git::open_repo(repo_root)?;
    let files = git::list_files_at_commit(&repo, git_sha)?;

    let mut entries = Vec::new();
    for path in files {
        let data = git::read_file_at_commit(&repo, git_sha, &path)?;
        if let Ok(info) = serde_json::from_slice::<XetFileInfo>(&data) {
            entries.push(PointerEntry {
                path,
                file_info: info,
            });
        }
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(PointerIndex {
        repo_id: repo_id.to_string(),
        git_sha: git_sha.to_string(),
        entries,
    })
}

pub fn pointer_cache_path(repo_root: &Path, sha: &str) -> std::path::PathBuf {
    repo_root.join(".xet_ai").join("pointers").join(format!("{sha}.json"))
}

pub fn cache_pointer_index(repo_root: &Path, index: &PointerIndex) -> Result<()> {
    let path = pointer_cache_path(repo_root, &index.git_sha);
    let content = serde_json::to_string_pretty(index)?;
    atomic_write_string(&path, &content)
}

pub fn read_pointer_index(path: &Path) -> Result<PointerIndex> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

pub fn list_cached_pointer_indexes(repo_root: &Path) -> Result<Vec<PointerIndex>> {
    let dir = repo_root.join(".xet_ai").join("pointers");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        out.push(read_pointer_index(&path)?);
    }
    out.sort_by(|a, b| a.git_sha.cmp(&b.git_sha));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use uuid::Uuid;

    fn init_repo() -> (std::path::PathBuf, Repository) {
        let root = std::env::temp_dir().join(format!("xet-ai-pointer-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create dir");
        let repo = Repository::init(&root).expect("init repo");
        (root, repo)
    }

    fn commit_files(repo: &Repository, files: &[(&str, &[u8])], message: &str) -> String {
        let workdir = repo.workdir().expect("workdir");
        for (path, data) in files {
            let p = workdir.join(path);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("mkdirs");
            }
            fs::write(p, data).expect("write file");
        }

        let mut index = repo.index().expect("index");
        for (path, _) in files {
            index
                .add_path(std::path::Path::new(path))
                .expect("add path");
        }
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = Signature::now("xet-ai", "xet-ai@example.com").expect("sig");

        let oid = if let Ok(head) = repo.head() {
            let parent = repo
                .find_commit(head.target().expect("target"))
                .expect("find parent");
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
                .expect("commit")
        } else {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
                .expect("initial commit")
        };
        oid.to_string()
    }

    #[test]
    fn build_pointer_index_finds_only_pointer_files() {
        let (root, repo) = init_repo();
        let pointer_json = br#"{"hash":"0123456789abcdef0123456789abcdef01234567","file_size":123}"#;
        let sha = commit_files(
            &repo,
            &[("big.bin", pointer_json), ("notes.txt", b"hello")],
            "add files",
        );

        let idx = build_pointer_index(&root, &sha, "repo-id").expect("build index");
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].path, "big.bin");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pointer_cache_roundtrip() {
        let (root, repo) = init_repo();
        let pointer_json = br#"{"hash":"0123456789abcdef0123456789abcdef01234567","file_size":123}"#;
        let sha = commit_files(&repo, &[("big.bin", pointer_json)], "add pointer");
        let idx = build_pointer_index(&root, &sha, "repo-id").expect("build index");

        cache_pointer_index(&root, &idx).expect("cache");
        let loaded = read_pointer_index(&pointer_cache_path(&root, &sha)).expect("read");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.git_sha, sha);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_files_at_commit_works() {
        let (root, repo) = init_repo();
        let sha = commit_files(&repo, &[("a.txt", b"a"), ("sub/b.txt", b"b")], "add");
        let files = crate::git::list_files_at_commit(&repo, &sha).expect("list files");
        assert_eq!(files, vec!["a.txt".to_string(), "sub/b.txt".to_string()]);
        let _ = fs::remove_dir_all(root);
    }
}
