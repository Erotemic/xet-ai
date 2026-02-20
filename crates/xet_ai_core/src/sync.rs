use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::remote::RemoteStore;

pub const HASH_VERIFY_LIMIT: u64 = 8 * 1024 * 1024;
const ALLOWLIST_TOP_LEVEL: &[&str] = &["cas", "shards", "mdb", "xorbs", "merkledb"];

#[derive(Debug, Clone, Copy)]
pub struct VerifyPolicy {
    pub hash_verify_limit: u64,
}

impl Default for VerifyPolicy {
    fn default() -> Self {
        Self {
            hash_verify_limit: HASH_VERIFY_LIMIT,
        }
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HashCache {
    entries: BTreeMap<String, HashCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HashCacheEntry {
    size: u64,
    sha256: String,
}

pub trait HashProvider {
    fn hash_file(&mut self, path: &Path) -> Result<String>;
}

struct FileHashProvider;

impl HashProvider for FileHashProvider {
    fn hash_file(&mut self, path: &Path) -> Result<String> {
        sha256_file(path)
    }
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

fn load_hash_cache(path: &Path) -> Result<HashCache> {
    if !path.exists() {
        return Ok(HashCache::default());
    }
    let content = fs::read_to_string(path)?;
    let cache: HashCache = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse hash cache {}", path.display()))?;
    Ok(cache)
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

    let write_result = (|| -> Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }

    Ok(())
}

pub fn atomic_write_string(dest: &Path, content: &str) -> Result<()> {
    atomic_write_bytes(dest, content.as_bytes())
}

fn save_hash_cache(path: &Path, cache: &HashCache) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(cache)?;
    atomic_write_bytes(path, &bytes)
}

pub fn build_manifest(
    repo_id: &str,
    git_sha: &str,
    cas_root: &Path,
    hash_cache_path: &Path,
) -> Result<Manifest> {
    let mut hasher = FileHashProvider;
    build_manifest_with_hasher(
        repo_id,
        git_sha,
        cas_root,
        hash_cache_path,
        None,
        &mut hasher,
    )
}

pub fn build_manifest_for_relpaths(
    repo_id: &str,
    git_sha: &str,
    cas_root: &Path,
    hash_cache_path: &Path,
    relpaths: &[String],
) -> Result<Manifest> {
    let mut hasher = FileHashProvider;
    build_manifest_with_hasher(
        repo_id,
        git_sha,
        cas_root,
        hash_cache_path,
        Some(relpaths),
        &mut hasher,
    )
}

fn build_manifest_with_hasher(
    repo_id: &str,
    git_sha: &str,
    cas_root: &Path,
    hash_cache_path: &Path,
    only_relpaths: Option<&[String]>,
    hasher: &mut dyn HashProvider,
) -> Result<Manifest> {
    let mut cache = load_hash_cache(hash_cache_path)?;
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    if let Some(requested_relpaths) = only_relpaths {
        for relpath in requested_relpaths {
            let rel = Path::new(relpath);
            if !sync_included(rel) {
                continue;
            }
            let abs = cas_root.join(relpath);
            if !abs.exists() {
                continue;
            }

            let size = fs::metadata(&abs)?.len();
            let sha256 = match cache.entries.get(relpath) {
                Some(cached) if cached.size == size => cached.sha256.clone(),
                _ => {
                    let hash = hasher.hash_file(&abs)?;
                    cache.entries.insert(
                        relpath.clone(),
                        HashCacheEntry {
                            size,
                            sha256: hash.clone(),
                        },
                    );
                    hash
                }
            };

            total_bytes = total_bytes.saturating_add(size);
            entries.push(ManifestEntry {
                relpath: relpath.clone(),
                size,
                sha256,
            });
        }
    } else if cas_root.exists() {
        for entry in WalkDir::new(cas_root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(cas_root)?;
            if !sync_included(rel) {
                continue;
            }

            let relpath = rel.to_string_lossy().to_string();
            let size = entry.metadata()?.len();
            let sha256 = match cache.entries.get(&relpath) {
                Some(cached) if cached.size == size => cached.sha256.clone(),
                _ => {
                    let hash = hasher.hash_file(entry.path())?;
                    cache.entries.insert(
                        relpath.clone(),
                        HashCacheEntry {
                            size,
                            sha256: hash.clone(),
                        },
                    );
                    hash
                }
            };

            total_bytes = total_bytes.saturating_add(size);
            entries.push(ManifestEntry {
                relpath,
                size,
                sha256,
            });
        }
    }

    entries.sort_by(|a, b| a.relpath.cmp(&b.relpath));

    save_hash_cache(hash_cache_path, &cache)?;

    Ok(Manifest {
        repo_id: repo_id.to_string(),
        git_sha: git_sha.to_string(),
        total_bytes,
        entries,
    })
}

pub fn write_manifest_atomic(path: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    atomic_write_bytes(path, &bytes)
}

pub fn write_head_atomic(path: &Path, git_sha: &str) -> Result<()> {
    atomic_write_string(path, &format!("{git_sha}\n"))
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
        "{}.tmp.{}.{}",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        random_suffix()
    ));

    if let Err(e) = fs::copy(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }

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

    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
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

fn verify_existing(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    policy: VerifyPolicy,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let size = fs::metadata(path)?.len();
    if size != expected_size {
        bail!(
            "destination corruption detected: {} has size {}, expected {}",
            path.display(),
            size,
            expected_size
        );
    }
    if expected_size <= policy.hash_verify_limit {
        let got = sha256_file(path)?;
        if got != expected_sha256 {
            bail!(
                "destination hash mismatch: {} expected {} got {}",
                path.display(),
                expected_sha256,
                got
            );
        }
    }
    Ok(())
}

fn verify_existing_remote(
    remote_store: &dyn RemoteStore,
    remote_relpath: &str,
    expected_size: u64,
    expected_sha256: &str,
    policy: VerifyPolicy,
) -> Result<bool> {
    let Some(bytes) = remote_store.read_bytes(remote_relpath)? else {
        return Ok(false);
    };
    let size = bytes.len() as u64;
    if size != expected_size {
        bail!(
            "destination corruption detected: {} has size {}, expected {}",
            remote_relpath,
            size,
            expected_size
        );
    }
    if expected_size <= policy.hash_verify_limit {
        let got = format!("{:x}", Sha256::digest(&bytes));
        if got != expected_sha256 {
            bail!(
                "destination hash mismatch: {} expected {} got {}",
                remote_relpath,
                expected_sha256,
                got
            );
        }
    }
    Ok(true)
}

pub fn copy_local_to_remote_atomic_verified(
    local_path: &Path,
    remote_store: &dyn RemoteStore,
    remote_relpath: &str,
    expected_size: u64,
    expected_sha256: &str,
    policy: VerifyPolicy,
) -> Result<u64> {
    if verify_existing_remote(
        remote_store,
        remote_relpath,
        expected_size,
        expected_sha256,
        policy,
    )? {
        return Ok(0);
    }

    remote_store.copy_from_local_atomic(local_path, remote_relpath)?;

    if !verify_existing_remote(
        remote_store,
        remote_relpath,
        expected_size,
        expected_sha256,
        policy,
    )? {
        bail!("remote copy failed to materialize {}", remote_relpath);
    }

    Ok(expected_size)
}

pub fn copy_remote_to_local_atomic_verified(
    remote_store: &dyn RemoteStore,
    remote_relpath: &str,
    local_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    policy: VerifyPolicy,
) -> Result<u64> {
    verify_existing(local_path, expected_size, expected_sha256, policy)?;
    if local_path.exists() {
        return Ok(0);
    }

    remote_store.copy_to_local_atomic(remote_relpath, local_path)?;
    verify_existing(local_path, expected_size, expected_sha256, policy)?;
    Ok(expected_size)
}

pub fn push_with_manifest_store(
    local_cas_root: &Path,
    remote_store: &dyn RemoteStore,
    manifest: &Manifest,
) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();
    let policy = VerifyPolicy::default();

    for entry in &manifest.entries {
        let src = local_cas_root.join(&entry.relpath);
        let remote_relpath = format!("xet/{}", entry.relpath);
        let copied = copy_local_to_remote_atomic_verified(
            &src,
            remote_store,
            &remote_relpath,
            entry.size,
            &entry.sha256,
            policy,
        )?;
        if copied > 0 {
            summary.files_copied += 1;
            summary.bytes_copied += copied;
        }
        summary.files_verified += 1;
    }

    Ok(summary)
}

pub fn pull_from_manifest_store(
    remote_store: &dyn RemoteStore,
    local_cas_root: &Path,
    manifest: &Manifest,
) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();
    let policy = VerifyPolicy::default();

    for entry in &manifest.entries {
        let remote_relpath = format!("xet/{}", entry.relpath);
        if !remote_store.exists(&remote_relpath)? {
            bail!(
                "remote CAS file referenced by manifest is missing: {}",
                remote_relpath
            );
        }

        let dst = local_cas_root.join(&entry.relpath);
        let copied = copy_remote_to_local_atomic_verified(
            remote_store,
            &remote_relpath,
            &dst,
            entry.size,
            &entry.sha256,
            policy,
        )?;
        if copied > 0 {
            summary.files_copied += 1;
            summary.bytes_copied += copied;
        }
        summary.files_verified += 1;
    }

    Ok(summary)
}

pub fn verify_manifest_local(local_cas_root: &Path, manifest: &Manifest) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();

    for entry in &manifest.entries {
        let p = local_cas_root.join(&entry.relpath);
        if !p.exists() {
            bail!("manifest verify failed: missing local file {}", p.display());
        }

        let size = fs::metadata(&p)?.len();
        if size != entry.size {
            bail!(
                "manifest verify failed: size mismatch for {} (expected {}, got {})",
                p.display(),
                entry.size,
                size
            );
        }

        if entry.size <= HASH_VERIFY_LIMIT {
            let got = sha256_file(&p)?;
            if got != entry.sha256 {
                bail!(
                    "manifest verify failed: hash mismatch for {} (expected {}, got {})",
                    p.display(),
                    entry.sha256,
                    got
                );
            }
        }

        summary.files_verified += 1;
    }

    Ok(summary)
}

pub fn is_sha1_hex(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn resolve_ref_or_sha(remote_repo_root: &Path, input: Option<&str>) -> Result<String> {
    match input {
        None => read_head(&remote_repo_root.join("manifests").join("HEAD")),
        Some(v) if is_sha1_hex(v) => Ok(v.to_string()),
        Some(refname) => {
            let ref_path = remote_repo_root.join("refs").join(refname);
            let content = fs::read_to_string(&ref_path)
                .with_context(|| format!("remote ref not found: {}", ref_path.display()))?;
            let sha = content.trim();
            if !is_sha1_hex(sha) {
                bail!("remote ref {} does not resolve to a valid SHA", refname);
            }
            Ok(sha.to_string())
        }
    }
}

pub fn list_remote_refs(remote_repo_root: &Path) -> Result<Vec<(String, String)>> {
    let refs_root = remote_repo_root.join("refs");
    if !refs_root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(&refs_root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&refs_root)?;
        let name = rel.to_string_lossy().to_string();
        let sha = fs::read_to_string(entry.path())?.trim().to_string();
        out.push((name, sha));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

pub struct PushLockGuard {
    lock_path: PathBuf,
}

impl Drop for PushLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

pub fn acquire_push_lock(remote_repo_root: &Path, force_lock: bool) -> Result<PushLockGuard> {
    let lock_path = remote_repo_root.join("locks").join("push.lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let stale_secs = 30 * 60;
    if lock_path.exists() {
        let meta = fs::metadata(&lock_path)?;
        let stale = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() > stale_secs)
            .unwrap_or(false);

        if force_lock || stale {
            eprintln!(
                "xet-ai: warning: breaking existing push lock at {}",
                lock_path.display()
            );
            let _ = fs::remove_file(&lock_path);
        } else {
            bail!(
                "another push appears to be in progress; lock exists at {}",
                lock_path.display()
            );
        }
    }

    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to acquire push lock at {}", lock_path.display()))?;

    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown-host".to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    writeln!(f, "pid={}", std::process::id())?;
    writeln!(f, "host={}", host)?;
    writeln!(f, "unix_ts={}", now)?;

    Ok(PushLockGuard { lock_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingHasher {
        calls: usize,
    }

    impl HashProvider for CountingHasher {
        fn hash_file(&mut self, path: &Path) -> Result<String> {
            self.calls += 1;
            sha256_file(path)
        }
    }

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

    #[test]
    fn hash_cache_reuses_hashes() {
        let root = std::env::temp_dir().join(format!("xet-ai-sync-test-{}", Uuid::new_v4()));
        let cas_root = root.join("xet");
        let hash_cache_path = root.join("hash_cache.json");
        fs::create_dir_all(cas_root.join("xorbs/xorbs")).expect("failed to create test cas dirs");
        fs::write(cas_root.join("xorbs/xorbs/a.bin"), b"hello world")
            .expect("failed to write test cas file");

        let mut h1 = CountingHasher { calls: 0 };
        let _ =
            build_manifest_with_hasher("repo", "sha1", &cas_root, &hash_cache_path, None, &mut h1)
                .expect("manifest build 1 failed");
        assert_eq!(h1.calls, 1);

        let mut h2 = CountingHasher { calls: 0 };
        let _ =
            build_manifest_with_hasher("repo", "sha1", &cas_root, &hash_cache_path, None, &mut h2)
                .expect("manifest build 2 failed");
        assert_eq!(h2.calls, 0);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_ref_or_sha_works() {
        let root = std::env::temp_dir().join(format!("xet-ai-ref-test-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("refs")).expect("create refs");
        fs::create_dir_all(root.join("manifests")).expect("create manifests");

        let sha = "0123456789abcdef0123456789abcdef01234567";
        fs::write(root.join("refs/main"), format!("{}\n", sha)).expect("write ref");
        fs::write(root.join("manifests/HEAD"), format!("{}\n", sha)).expect("write head");

        assert_eq!(
            resolve_ref_or_sha(&root, Some(sha)).expect("sha resolve"),
            sha
        );
        assert_eq!(
            resolve_ref_or_sha(&root, Some("main")).expect("ref resolve"),
            sha
        );
        assert_eq!(resolve_ref_or_sha(&root, None).expect("head resolve"), sha);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verified_copy_detects_remote_corruption() {
        let root = std::env::temp_dir().join(format!("xet-ai-verify-{}", Uuid::new_v4()));
        let local = root.join("local");
        let remote = root.join("remote");
        fs::create_dir_all(local.join("xet/cas")).expect("local dirs");
        let local_file = local.join("xet/cas/a.bin");
        fs::write(&local_file, b"hello").expect("write local");

        let sha = sha256_file(&local_file).expect("hash");
        let store = crate::remote::FilesystemRemoteStore::new(remote.clone());

        copy_local_to_remote_atomic_verified(
            &local_file,
            &store,
            "xet/cas/a.bin",
            5,
            &sha,
            VerifyPolicy::default(),
        )
        .expect("copy");

        fs::write(remote.join("xet/cas/a.bin"), b"helloo").expect("corrupt size");
        let err = copy_local_to_remote_atomic_verified(
            &local_file,
            &store,
            "xet/cas/a.bin",
            5,
            &sha,
            VerifyPolicy::default(),
        )
        .expect_err("must fail on size mismatch");
        assert!(err.to_string().contains("size"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verified_copy_detects_small_file_hash_mismatch() {
        let root = std::env::temp_dir().join(format!("xet-ai-verify-hash-{}", Uuid::new_v4()));
        let local = root.join("local");
        let remote = root.join("remote");
        fs::create_dir_all(local.join("xet/cas")).expect("local dirs");
        let local_file = local.join("xet/cas/a.bin");
        fs::write(&local_file, b"hello").expect("write local");

        let sha = sha256_file(&local_file).expect("hash");
        let store = crate::remote::FilesystemRemoteStore::new(remote.clone());
        copy_local_to_remote_atomic_verified(
            &local_file,
            &store,
            "xet/cas/a.bin",
            5,
            &sha,
            VerifyPolicy::default(),
        )
        .expect("copy");

        fs::write(remote.join("xet/cas/a.bin"), b"jello").expect("corrupt content");
        let err = copy_local_to_remote_atomic_verified(
            &local_file,
            &store,
            "xet/cas/a.bin",
            5,
            &sha,
            VerifyPolicy::default(),
        )
        .expect_err("must fail hash mismatch");
        assert!(err.to_string().contains("hash"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lock_acquire_release() {
        let root = std::env::temp_dir().join(format!("xet-ai-lock-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create root");

        {
            let _guard = acquire_push_lock(&root, false).expect("acquire lock");
            assert!(root.join("locks/push.lock").exists());
        }

        assert!(!root.join("locks/push.lock").exists());
        let _ = fs::remove_dir_all(root);
    }
}
