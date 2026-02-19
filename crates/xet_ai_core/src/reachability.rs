use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use data::XetFileInfo;

use crate::pointers::{self, PointerIndex};
use crate::sync;

#[derive(Debug, Clone)]
pub struct ReachabilityPlan {
    pub git_sha: String,
    pub pointer_paths: Vec<String>,
    pub required_cas_relpaths: Vec<String>,
}

pub trait CasFileAccess {
    fn read_cas_relpath(&mut self, relpath: &str) -> Result<()>;
}

pub trait PointerHydrator {
    fn hydrate_pointer(&mut self, pointer: &XetFileInfo, cas: &mut dyn CasFileAccess)
        -> Result<()>;
}

pub struct TracingCasAccess {
    cas_root: PathBuf,
    accessed: BTreeSet<String>,
}

impl TracingCasAccess {
    pub fn new(cas_root: &Path) -> Self {
        Self {
            cas_root: cas_root.to_path_buf(),
            accessed: BTreeSet::new(),
        }
    }

    pub fn accessed(self) -> Vec<String> {
        self.accessed.into_iter().collect()
    }
}

impl CasFileAccess for TracingCasAccess {
    fn read_cas_relpath(&mut self, relpath: &str) -> Result<()> {
        let p = self.cas_root.join(relpath);
        let _ = std::fs::read(&p)?;
        self.accessed.insert(relpath.to_string());
        Ok(())
    }
}

pub struct PointerHashHydrator {
    relpaths: Vec<String>,
}

impl PointerHashHydrator {
    pub fn new(cas_root: &Path) -> Result<Self> {
        let mut relpaths = Vec::new();
        if cas_root.exists() {
            for entry in walkdir::WalkDir::new(cas_root)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry.path().strip_prefix(cas_root)?;
                if sync::sync_included(rel) {
                    relpaths.push(rel.to_string_lossy().to_string());
                }
            }
        }
        relpaths.sort();
        Ok(Self { relpaths })
    }

    fn pointer_tokens(pointer: &XetFileInfo) -> HashSet<String> {
        let mut out = HashSet::new();
        if let Ok(v) = serde_json::to_value(pointer) {
            for key in ["hash", "cas_hash", "content_hash"] {
                if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                    let token = s.trim().to_ascii_lowercase();
                    if !token.is_empty() {
                        out.insert(token.clone());
                        if token.len() >= 16 {
                            out.insert(token[..16].to_string());
                        }
                    }
                }
            }
        }
        out
    }
}

impl PointerHydrator for PointerHashHydrator {
    fn hydrate_pointer(
        &mut self,
        pointer: &XetFileInfo,
        cas: &mut dyn CasFileAccess,
    ) -> Result<()> {
        let tokens = Self::pointer_tokens(pointer);
        if tokens.is_empty() {
            return Ok(());
        }

        let mut matched = 0usize;
        for relpath in &self.relpaths {
            let lower = relpath.to_ascii_lowercase();
            if tokens.iter().any(|tok| lower.contains(tok)) {
                cas.read_cas_relpath(relpath)?;
                matched += 1;
            }
        }

        if matched == 0 {
            for relpath in &self.relpaths {
                if relpath.starts_with("xorbs/") {
                    cas.read_cas_relpath(relpath)?;
                }
            }
        }

        Ok(())
    }
}

pub fn load_or_build_pointer_index(
    repo_root: &Path,
    git_sha: &str,
    repo_id: &str,
) -> Result<PointerIndex> {
    let cache_path = pointers::pointer_cache_path(repo_root, git_sha);
    if cache_path.exists() {
        return pointers::read_pointer_index(&cache_path);
    }

    let idx = pointers::build_pointer_index(repo_root, git_sha, repo_id)?;
    pointers::cache_pointer_index(repo_root, &idx)?;
    Ok(idx)
}

pub fn plan_reachable_cas(
    repo_root: &Path,
    cas_root: &Path,
    repo_id: &str,
    git_sha: &str,
    hydrator: &mut dyn PointerHydrator,
) -> Result<ReachabilityPlan> {
    let idx = load_or_build_pointer_index(repo_root, git_sha, repo_id)?;

    let mut pointer_paths = idx
        .entries
        .iter()
        .map(|e| e.path.clone())
        .collect::<Vec<_>>();
    pointer_paths.sort();

    let mut access = TracingCasAccess::new(cas_root);
    for entry in &idx.entries {
        hydrator.hydrate_pointer(&entry.file_info, &mut access)?;
    }

    Ok(ReachabilityPlan {
        git_sha: git_sha.to_string(),
        pointer_paths,
        required_cas_relpaths: access.accessed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use git2::{Repository, Signature};

    struct FakeHydrator;

    impl PointerHydrator for FakeHydrator {
        fn hydrate_pointer(
            &mut self,
            _pointer: &XetFileInfo,
            cas: &mut dyn CasFileAccess,
        ) -> Result<()> {
            cas.read_cas_relpath("cas/needed-a")?;
            cas.read_cas_relpath("xorbs/needed-b")?;
            Ok(())
        }
    }

    fn commit_files(repo: &Repository, files: &[(&str, &[u8])]) -> String {
        let workdir = repo.workdir().expect("workdir");
        for (path, data) in files {
            let p = workdir.join(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdirs");
            }
            std::fs::write(&p, data).expect("write");
        }

        let mut index = repo.index().expect("index");
        for (path, _) in files {
            index
                .add_path(std::path::Path::new(path))
                .expect("add_path");
        }
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = Signature::now("xet-ai", "xet-ai@example.com").expect("sig");

        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[])
            .expect("commit")
            .to_string()
    }

    #[test]
    fn reachability_plan_traces_only_hydrator_reads() {
        let root =
            std::env::temp_dir().join(format!("xet-ai-reachability-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("root");
        let repo = Repository::init(&root).expect("repo");

        let pointer_json =
            br#"{"hash":"0123456789abcdef0123456789abcdef01234567","file_size":123}"#;
        let sha = commit_files(&repo, &[("big.bin", pointer_json), ("note.txt", b"hello")]);

        let cas_root = root.join(".xet_ai").join("xet");
        std::fs::create_dir_all(cas_root.join("cas")).expect("cas dir");
        std::fs::create_dir_all(cas_root.join("xorbs")).expect("xorbs dir");
        std::fs::write(cas_root.join("cas/needed-a"), b"a").expect("a");
        std::fs::write(cas_root.join("xorbs/needed-b"), b"b").expect("b");
        std::fs::write(cas_root.join("cas/ignored"), b"c").expect("c");

        let mut hydrator = FakeHydrator;
        let plan =
            plan_reachable_cas(&root, &cas_root, "repo-id", &sha, &mut hydrator).expect("plan");

        assert_eq!(plan.git_sha, sha);
        assert_eq!(plan.pointer_paths, vec!["big.bin".to_string()]);
        assert_eq!(
            plan.required_cas_relpaths,
            vec!["cas/needed-a".to_string(), "xorbs/needed-b".to_string()]
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
