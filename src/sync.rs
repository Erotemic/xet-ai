use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

pub const HASH_VERIFY_LIMIT: u64 = 8 * 1024 * 1024;
const ALLOWLIST_TOP_LEVEL: &[&str] = &["cas", "shards", "mdb", "xorbs", "merkledb"];

#[derive(Debug, Default)]
pub struct SyncSummary {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub files_verified: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub relpath: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub repo_id: String,
    pub git_sha: String,
    pub total_bytes: u64,
    pub entries: Vec<ManifestEntry>,
}

fn first_component(path: &Path) -> Option<String> {
    path.components().find_map(|c| match c {
        Component::Normal(s) => Some(s.to_string_lossy().to_string()),
        _ => None,
    })
}

pub fn sync_included(rel_path: &Path) -> bool {
    let Some(first) = first_component(rel_path) else {
        return false;
    };
    if !ALLOWLIST_TOP_LEVEL.iter().any(|x| x == &first) {
        return false;
    }

    let rel = rel_path.to_string_lossy();
    if rel.starts_with("xorbs/global_dedup_lookup.db/") {
        return false;
    }
    true
}

pub fn describe_cas_tree(cas_root: &Path) -> Result<Vec<(String, bool)>> {
    if !cas_root.exists() {
        return Ok(Vec::new());
    }

    let mut dirs = Vec::new();
    for entry in fs::read_dir(cas_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            let included = ALLOWLIST_TOP_LEVEL.iter().any(|x| x == &name);
            dirs.push((name, included));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(dirs)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn build_manifest(repo_id: &str, git_sha: &str, cas_root: &Path) -> Result<Manifest> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    if cas_root.exists() {
        for entry in WalkDir::new(cas_root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(cas_root)?;
            if !sync_included(rel) {
                continue;
            }

            let size = entry.metadata()?.len();
            let sha256 = sha256_file(entry.path())?;
            total_bytes = total_bytes.saturating_add(size);
            entries.push(ManifestEntry {
                relpath: rel.to_string_lossy().to_string(),
                size,
                sha256,
            });
        }
    }

    entries.sort_by(|a, b| a.relpath.cmp(&b.relpath));

    Ok(Manifest {
        repo_id: repo_id.to_string(),
        git_sha: git_sha.to_string(),
        total_bytes,
        entries,
    })
}

fn atomic_write_bytes(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dest.display()))?;
    fs::create_dir_all(parent)?;

    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    fs::rename(&tmp, dest)?;
    Ok(())
}

pub fn write_manifest_atomic(path: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    atomic_write_bytes(path, &bytes)
}

pub fn write_head_atomic(path: &Path, git_sha: &str) -> Result<()> {
    atomic_write_bytes(path, format!("{git_sha}\n").as_bytes())
}

pub fn read_head(path: &Path) -> Result<String> {
    let content = fs::read_to_string(path).with_context(|| {
        format!(
            "remote has no data HEAD; run `xet-ai push` first ({})",
            path.display()
        )
    })?;
    let sha = content.trim();
    if sha.is_empty() {
        bail!("remote HEAD is empty: {}", path.display());
    }
    Ok(sha.to_string())
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("manifest missing: {}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&content)
        .with_context(|| format!("manifest corrupt: {}", path.display()))?;
    Ok(manifest)
}

pub fn cache_manifest(local_path: &Path, manifest: &Manifest) -> Result<()> {
    write_manifest_atomic(local_path, manifest)
}

pub fn copy_file_atomic_verified(
    src: &Path,
    dst: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
) -> Result<u64> {
    if dst.exists() {
        let size = fs::metadata(dst)?.len();
        if size != expected_size {
            bail!(
                "destination corruption detected: {} has size {}, expected {}",
                dst.display(),
                size,
                expected_size
            );
        }
        if let Some(expected_hash) = expected_sha256 {
            if expected_size <= HASH_VERIFY_LIMIT {
                let got = sha256_file(dst)?;
                if got != expected_hash {
                    bail!(
                        "destination hash mismatch: {} expected {} got {}",
                        dst.display(),
                        expected_hash,
                        got
                    );
                }
            }
        }
        return Ok(0);
    }

    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dst.display()))?;
    fs::create_dir_all(parent)?;

    if !src.exists() {
        bail!("source missing: {}", src.display());
    }

    let tmp = parent.join(format!(
        "{}.tmp.{}",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    fs::copy(src, &tmp)?;

    let tmp_size = fs::metadata(&tmp)?.len();
    if tmp_size != expected_size {
        let _ = fs::remove_file(&tmp);
        bail!(
            "copied file has incorrect size: {} expected {} got {}",
            tmp.display(),
            expected_size,
            tmp_size
        );
    }

    if let Some(expected_hash) = expected_sha256 {
        if expected_size <= HASH_VERIFY_LIMIT {
            let got = sha256_file(&tmp)?;
            if got != expected_hash {
                let _ = fs::remove_file(&tmp);
                bail!(
                    "copied file has incorrect hash: {} expected {} got {}",
                    tmp.display(),
                    expected_hash,
                    got
                );
            }
        }
    }

    fs::rename(&tmp, dst)?;
    Ok(expected_size)
}

pub fn push_with_manifest(
    local_cas_root: &Path,
    remote_cas_root: &Path,
    manifest: &Manifest,
) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();

    for entry in &manifest.entries {
        let src = local_cas_root.join(&entry.relpath);
        let dst = remote_cas_root.join(&entry.relpath);
        let copied = copy_file_atomic_verified(&src, &dst, entry.size, Some(&entry.sha256))?;
        if copied > 0 {
            summary.files_copied += 1;
            summary.bytes_copied += copied;
        }
        summary.files_verified += 1;
    }

    Ok(summary)
}

pub fn pull_from_manifest(
    remote_cas_root: &Path,
    local_cas_root: &Path,
    manifest: &Manifest,
) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();

    for entry in &manifest.entries {
        let src = remote_cas_root.join(&entry.relpath);
        if !src.exists() {
            bail!(
                "remote CAS file referenced by manifest is missing: {}",
                src.display()
            );
        }
        let dst = local_cas_root.join(&entry.relpath);
        let copied = copy_file_atomic_verified(&src, &dst, entry.size, Some(&entry.sha256))?;
        if copied > 0 {
            summary.files_copied += 1;
            summary.bytes_copied += copied;
        }
        summary.files_verified += 1;
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_filter_works() {
        assert!(sync_included(Path::new("xorbs/a")));
        assert!(sync_included(Path::new("cas/x")));
        assert!(!sync_included(Path::new("shard-cache/a")));
        assert!(!sync_included(Path::new("tmp/a")));
        assert!(!sync_included(Path::new(
            "xorbs/global_dedup_lookup.db/lock.mdb"
        )));
    }
}
