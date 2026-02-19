use std::path::{Path, PathBuf};

use anyhow::Result;

pub trait Remote {
    fn repo_root(&self) -> &Path;
    fn cas_root(&self) -> PathBuf {
        self.repo_root().join("xet")
    }
    fn manifests_root(&self) -> PathBuf {
        self.repo_root().join("manifests")
    }
    fn refs_root(&self) -> PathBuf {
        self.repo_root().join("refs")
    }
    fn pointers_root(&self) -> PathBuf {
        self.repo_root().join("pointers")
    }
}

#[derive(Debug, Clone)]
pub struct FilesystemRemote {
    repo_root: PathBuf,
}

impl FilesystemRemote {
    pub fn new(base_remote_path: &Path, repo_id: &str) -> Self {
        Self {
            repo_root: base_remote_path.join(repo_id),
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        std::fs::create_dir_all(self.cas_root())?;
        std::fs::create_dir_all(self.manifests_root())?;
        std::fs::create_dir_all(self.refs_root())?;
        std::fs::create_dir_all(self.pointers_root())?;
        Ok(())
    }
}

impl Remote for FilesystemRemote {
    fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}
