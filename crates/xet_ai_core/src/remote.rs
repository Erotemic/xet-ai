//! Remote storage abstraction and filesystem backend implementation.
//!
//! The `RemoteStore` trait provides the capability surface required by sync and
//! publish flows while keeping backend-specific details encapsulated.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use uuid::Uuid;
use walkdir::WalkDir;

/// Feature flags exposed by a remote backend.
#[derive(Debug, Clone, Copy)]
pub struct RemoteCapabilities {
    pub supports_locking: bool,
    pub supports_list_prefix: bool,
}

/// Lightweight metadata returned by [`RemoteStore::stat`].
#[derive(Debug, Clone, Copy)]
pub struct RemoteStat {
    pub size: u64,
}

/// Backend abstraction used by sync and publish flows.
pub trait RemoteStore {
    fn capabilities(&self) -> RemoteCapabilities;
    fn stat(&self, remote_relpath: &str) -> Result<Option<RemoteStat>>;
    fn read_bytes(&self, remote_relpath: &str) -> Result<Option<Vec<u8>>>;
    fn write_bytes_atomic(&self, remote_relpath: &str, bytes: &[u8]) -> Result<()>;
    fn exists(&self, remote_relpath: &str) -> Result<bool>;
    fn open_reader(&self, remote_relpath: &str) -> Result<Box<dyn Read + Send>>;
    fn copy_from_local_atomic(&self, local_path: &Path, remote_relpath: &str) -> Result<()>;
    fn copy_to_local_atomic(&self, remote_relpath: &str, local_path: &Path) -> Result<()>;
    fn list_prefix(&self, prefix: &str) -> Result<Vec<String>>;
}

/// Filesystem-backed implementation of [`RemoteStore`].
#[derive(Debug, Clone)]
pub struct FilesystemRemoteStore {
    root: PathBuf,
}

impl FilesystemRemoteStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn abs(&self, remote_relpath: &str) -> PathBuf {
        self.root.join(remote_relpath)
    }
}

fn random_suffix() -> String {
    Uuid::new_v4().to_string()
}

fn atomic_write_bytes(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dest.display()))?;
    fs::create_dir_all(parent)?;

    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        random_suffix()
    ));

    let wr = fs::write(&tmp, bytes);
    if let Err(e) = wr {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

fn copy_file_atomic(src: &Path, dst: &Path) -> Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dst.display()))?;
    fs::create_dir_all(parent)?;

    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        random_suffix()
    ));

    let cp = fs::copy(src, &tmp);
    if let Err(e) = cp {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }

    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

impl RemoteStore for FilesystemRemoteStore {
    fn capabilities(&self) -> RemoteCapabilities {
        RemoteCapabilities {
            supports_locking: true,
            supports_list_prefix: true,
        }
    }

    fn stat(&self, remote_relpath: &str) -> Result<Option<RemoteStat>> {
        let p = self.abs(remote_relpath);
        if !p.exists() {
            return Ok(None);
        }
        let size = fs::metadata(p)?.len();
        Ok(Some(RemoteStat { size }))
    }

    fn read_bytes(&self, remote_relpath: &str) -> Result<Option<Vec<u8>>> {
        let p = self.abs(remote_relpath);
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(fs::read(p)?))
    }

    fn write_bytes_atomic(&self, remote_relpath: &str, bytes: &[u8]) -> Result<()> {
        atomic_write_bytes(&self.abs(remote_relpath), bytes)
    }

    fn exists(&self, remote_relpath: &str) -> Result<bool> {
        Ok(self.abs(remote_relpath).exists())
    }

    fn open_reader(&self, remote_relpath: &str) -> Result<Box<dyn Read + Send>> {
        let p = self.abs(remote_relpath);
        if !p.exists() {
            return Err(anyhow!("remote path missing: {remote_relpath}"));
        }
        Ok(Box::new(std::fs::File::open(p)?))
    }

    fn copy_from_local_atomic(&self, local_path: &Path, remote_relpath: &str) -> Result<()> {
        copy_file_atomic(local_path, &self.abs(remote_relpath))
    }

    fn copy_to_local_atomic(&self, remote_relpath: &str, local_path: &Path) -> Result<()> {
        let src = self.abs(remote_relpath);
        copy_file_atomic(&src, local_path)
    }

    fn list_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let start = self.abs(prefix);
        if !start.exists() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for entry in WalkDir::new(&start).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&self.root)?;
            out.push(rel.to_string_lossy().to_string());
        }
        out.sort();
        Ok(out)
    }
}
