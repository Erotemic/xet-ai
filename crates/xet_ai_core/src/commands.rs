use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use data::configurations::TranslatorConfig;
use data::{FileDownloader, XetFileInfo};
use file_reconstruction::DataOutput;
use serde_json::Value;
use uuid::Uuid;

use crate::config::{self, ConfigFile, EffectiveConfig, RemoteConfig};
use crate::pointers::{self, PointerIndex};
use crate::reachability::{self, PointerHashHydrator};
use crate::remote::{FilesystemRemoteStore, RemoteStore};
use crate::{repo, sync};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushMode {
    MinimalValidate,
    AllCas,
    MinimalNoValidate,
}

fn manifest_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".xet_ai").join("manifests")
}

fn manifest_path(repo_root: &Path, sha: &str) -> PathBuf {
    manifest_dir(repo_root).join(format!("{sha}.json"))
}

fn resolve_remote_name(input: Option<&str>, effective: &EffectiveConfig) -> Result<String> {
    if let Some(name) = input {
        return Ok(name.to_string());
    }
    if let Some(name) = &effective.default_remote {
        return Ok(name.clone());
    }
    let mut names = effective.remotes.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("no remotes configured; run `xet-ai remote add <name> <path>`")
    })
}

fn fs_remote_store(
    repo_root: &Path,
    remote: &RemoteConfig,
    repo_id: &str,
) -> Result<FilesystemRemoteStore> {
    if remote.r#type != "filesystem" {
        bail!("unsupported remote type `{}`", remote.r#type);
    }
    let root = repo::resolve_path(repo_root, &remote.path).join(repo_id);
    Ok(FilesystemRemoteStore::new(root))
}

fn choose_representative_pointer(idx: &PointerIndex) -> Option<XetFileInfo> {
    let mut entries = idx.entries.clone();
    entries.sort_by_key(|e| {
        let sz = serde_json::to_value(&e.file_info)
            .ok()
            .and_then(|v| v.get("file_size").and_then(Value::as_u64))
            .unwrap_or(0);
        (Reverse(sz), e.path.clone())
    });
    entries.first().map(|e| e.file_info.clone())
}

async fn validate_minimal_plan(
    repo_root: &Path,
    sha: &str,
    relpaths: &[String],
    idx: &PointerIndex,
) -> Result<bool> {
    let Some(ptr) = choose_representative_pointer(idx) else {
        return Ok(true);
    };

    let val_base = repo_root.join(".xet_ai").join("validate").join(sha);
    let val_cas = val_base.join("xet");
    let local_cas = repo_root.join(".xet_ai").join("xet");
    fs::create_dir_all(&val_cas)?;

    for relpath in relpaths {
        let src = local_cas.join(relpath);
        if !src.exists() {
            continue;
        }
        let dst = val_cas.join(relpath);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }

    let cfg = Arc::new(TranslatorConfig::local_config(val_base.clone())?);
    let downloader = FileDownloader::new(cfg).await?;
    let output = DataOutput::writer(io::sink());
    let hash = ptr
        .merkle_hash()
        .map_err(|_| anyhow::anyhow!("Xet hash is corrupted"))?;

    let ok = downloader
        .smudge_file_from_hash(&hash, Arc::from("<validate>"), output, None, None)
        .await
        .is_ok();

    let _ = fs::remove_dir_all(val_base);
    Ok(ok)
}

pub fn init(init_config: bool) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let xet_ai_dir = repo_root.join(".xet_ai");
    fs::create_dir_all(&xet_ai_dir)?;

    repo::run_git(["config", "filter.xet_ai.clean", "xet-ai clean --path %f"])?;
    repo::run_git(["config", "filter.xet_ai.smudge", "xet-ai smudge --path %f"])?;
    repo::run_git(["config", "filter.xet_ai.required", "true"])?;

    let gitattributes = repo_root.join(".gitattributes");
    if !gitattributes.exists() {
        fs::write(&gitattributes, "*.bin filter=xet_ai diff=xet_ai -text\n")?;
    }

    repo::append_if_missing(&repo_root.join(".gitignore"), ".xet_ai/")?;

    let repo_id_file = repo::repo_id_path(&repo_root);
    if !repo_id_file.exists() {
        let repo_id = Uuid::new_v4().to_string();
        fs::write(&repo_id_file, format!("{repo_id}\n"))?;
        eprintln!(
            "created {} with repo id {repo_id}; please commit this file so clones share the same identity.",
            repo::REPO_ID_FILE
        );
    }

    if init_config {
        let shared = config::shared_config_path(&repo_root);
        if !shared.exists() {
            config::save_shared(&repo_root, &ConfigFile::default())?;
            eprintln!(
                "created {}; commit if you want shared remote config",
                shared.display()
            );
        }
    }

    Ok(())
}

pub fn manifest_list() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let dir = manifest_dir(&repo_root);
    if !dir.exists() {
        println!("no manifests cached");
        return Ok(());
    }

    let mut rows = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let sha = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let manifest = sync::read_manifest(&path)?;
        rows.push((sha, manifest.entries.len(), manifest.total_bytes));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (sha, entries, bytes) in rows {
        println!("{sha}\tentries={entries}\ttotal_bytes={bytes}");
    }
    Ok(())
}

pub fn manifest_show(sha: &str) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let path = manifest_path(&repo_root, sha);
    let manifest = sync::read_manifest(&path)?;
    println!("repo_id: {}", manifest.repo_id);
    println!("git_sha: {}", manifest.git_sha);
    println!("entries: {}", manifest.entries.len());
    println!("total_bytes: {}", manifest.total_bytes);
    let mut entries = manifest.entries.clone();
    entries.sort_by_key(|e| Reverse(e.size));
    for e in entries.into_iter().take(20) {
        println!("{}\t{}\t{}", e.size, e.sha256, e.relpath);
    }
    Ok(())
}

pub fn manifest_verify(sha: &str) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let path = manifest_path(&repo_root, sha);
    let manifest = sync::read_manifest(&path)?;
    let local_cas_root = repo_root.join(".xet_ai").join("xet");
    let summary = sync::verify_manifest_local(&local_cas_root, &manifest)?;
    println!(
        "verified {} files for manifest {}",
        summary.files_verified, manifest.git_sha
    );
    Ok(())
}

pub fn remote_add(name: &str, path: &Path, local: bool) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let mut cfg = if local {
        config::load_local(&repo_root)?
    } else {
        config::load_shared(&repo_root)?
    };
    cfg.remotes.insert(
        name.to_string(),
        RemoteConfig {
            r#type: "filesystem".to_string(),
            path: path.to_path_buf(),
        },
    );
    if local {
        config::save_local(&repo_root, &cfg)?
    } else {
        config::save_shared(&repo_root, &cfg)?
    }
    Ok(())
}

pub fn remote_set_default(name: &str, local: bool) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let mut cfg = if local {
        config::load_local(&repo_root)?
    } else {
        config::load_shared(&repo_root)?
    };
    cfg.default_remote = Some(name.to_string());
    if local {
        config::save_local(&repo_root, &cfg)?
    } else {
        config::save_shared(&repo_root, &cfg)?
    }
    Ok(())
}

pub fn remote_list() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let mut names = cfg.remotes.keys().cloned().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let r = cfg.remotes.get(&name).expect("remote exists");
        let default = cfg.default_remote.as_deref() == Some(name.as_str());
        println!(
            "{}{}\t{}\t{}",
            if default { "*" } else { " " },
            name,
            r.r#type,
            r.path.display()
        );
    }
    Ok(())
}

pub fn remote_refs(remote_name: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let repo_id = repo::load_repo_id(&repo_root)?;
    let store = fs_remote_store(&repo_root, remote, &repo_id)?;
    for p in store.list_prefix("refs")? {
        let sha = String::from_utf8_lossy(&store.read_bytes(&p)?.unwrap_or_default())
            .trim()
            .to_string();
        println!("{}\t{}", p.trim_start_matches("refs/"), sha);
    }
    Ok(())
}

pub fn remote_head(remote_name: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let repo_id = repo::load_repo_id(&repo_root)?;
    let store = fs_remote_store(&repo_root, remote, &repo_id)?;
    let sha = String::from_utf8_lossy(
        &store
            .read_bytes("manifests/HEAD")?
            .ok_or_else(|| anyhow::anyhow!("remote has no data HEAD; run `xet-ai push` first"))?,
    )
    .trim()
    .to_string();
    println!("{sha}");
    Ok(())
}

pub fn remote_tx_list(remote_name: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let repo_id = repo::load_repo_id(&repo_root)?;
    let store = fs_remote_store(&repo_root, remote, &repo_id)?;
    let mut txids = store
        .list_prefix("tx")?
        .into_iter()
        .filter_map(|p| p.strip_prefix("tx/").map(str::to_string))
        .filter(|p| !p.starts_with("COMMITTED/"))
        .filter_map(|p| p.split('/').next().map(str::to_string))
        .collect::<Vec<_>>();
    txids.sort();
    txids.dedup();
    for txid in txids {
        let committed = store.exists(&format!("tx/COMMITTED/{txid}"))?;
        println!(
            "{}\t{}",
            txid,
            if committed { "committed" } else { "staged" }
        );
    }
    Ok(())
}

pub async fn push(
    remote_name: Option<&str>,
    refname_opt: Option<&str>,
    force_lock: bool,
    mode: PushMode,
) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;

    let repo_id = repo::load_repo_id(&repo_root)?;
    let git_sha = repo::git_head_sha(&repo_root)?;
    let xet_ai_root = repo_root.join(".xet_ai");
    let local_cas_root = xet_ai_root.join("xet");
    let hash_cache_path = xet_ai_root.join("hash_cache.json");

    let store = fs_remote_store(&repo_root, remote, &repo_id)?;
    let _lock_guard = sync::acquire_push_lock(store.root(), force_lock)?;

    let pointer_index = reachability::load_or_build_pointer_index(&repo_root, &git_sha, &repo_id)?;
    let mut hydrator = PointerHashHydrator::new(&local_cas_root)?;
    let plan = reachability::plan_reachable_cas(
        &repo_root,
        &local_cas_root,
        &repo_id,
        &git_sha,
        &mut hydrator,
    )?;

    let mut used_all_cas = mode == PushMode::AllCas;
    if mode == PushMode::MinimalValidate {
        let ok = validate_minimal_plan(
            &repo_root,
            &git_sha,
            &plan.required_cas_relpaths,
            &pointer_index,
        )
        .await?;
        if !ok {
            eprintln!("warning: minimal reachability validation failed; falling back to --all-cas for this push");
            used_all_cas = true;
        }
    }
    if mode == PushMode::MinimalNoValidate {
        eprintln!("warning: running minimal push without validation (--minimal-no-validate)");
    }

    let manifest = if used_all_cas {
        sync::build_manifest(&repo_id, &git_sha, &local_cas_root, &hash_cache_path)?
    } else {
        sync::build_manifest_for_relpaths(
            &repo_id,
            &git_sha,
            &local_cas_root,
            &hash_cache_path,
            &plan.required_cas_relpaths,
        )?
    };

    let summary = sync::push_with_manifest_store(&local_cas_root, &store, &manifest)?;

    let local_manifest_path = manifest_path(&repo_root, &git_sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;
    pointers::cache_pointer_index(&repo_root, &pointer_index)?;

    let refname = refname_opt
        .map(|s| s.to_string())
        .or_else(|| repo::git_current_branch_short(&repo_root).ok().flatten())
        .unwrap_or_else(|| "HEAD".to_string());

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let txid = format!("{}.{}.{}", git_sha, ts, Uuid::new_v4());
    let tx_base = format!("tx/{txid}");

    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    let pointer_json = serde_json::to_vec_pretty(&pointer_index)?;
    store.write_bytes_atomic(
        &format!("{tx_base}/manifests/{git_sha}.json"),
        &manifest_json,
    )?;
    store.write_bytes_atomic(&format!("{tx_base}/pointers/{git_sha}.json"), &pointer_json)?;
    store.write_bytes_atomic(
        &format!("{tx_base}/refs/{refname}"),
        format!("{git_sha}\n").as_bytes(),
    )?;
    store.write_bytes_atomic(
        &format!("{tx_base}/manifests/HEAD"),
        format!("{git_sha}\n").as_bytes(),
    )?;
    store.write_bytes_atomic(
        &format!("{tx_base}/pointers/HEAD"),
        format!("{git_sha}\n").as_bytes(),
    )?;
    store.write_bytes_atomic(&format!("tx/COMMITTED/{txid}"), b"committed\n")?;

    store.write_bytes_atomic(&format!("manifests/{git_sha}.json"), &manifest_json)?;
    store.write_bytes_atomic(&format!("pointers/{git_sha}.json"), &pointer_json)?;
    store.write_bytes_atomic(
        &format!("refs/{refname}"),
        format!("{git_sha}\n").as_bytes(),
    )?;
    store.write_bytes_atomic("manifests/HEAD", format!("{git_sha}\n").as_bytes())?;
    store.write_bytes_atomic("pointers/HEAD", format!("{git_sha}\n").as_bytes())?;

    println!(
        "copied {} files ({} bytes)",
        summary.files_copied, summary.bytes_copied
    );
    println!(
        "wrote manifest {} with {} entries (total bytes {})",
        git_sha,
        manifest.entries.len(),
        manifest.total_bytes
    );
    if !used_all_cas {
        println!(
            "reachability: {} pointer files, {} required CAS files",
            plan.pointer_paths.len(),
            plan.required_cas_relpaths.len()
        );
    }
    println!("updated HEAD -> {}", git_sha);
    Ok(())
}

pub fn pull(
    remote_name: Option<&str>,
    git_ref: Option<&str>,
    verbose: bool,
    all_cas: bool,
) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let repo_id = repo::load_repo_id(&repo_root)?;
    let store = fs_remote_store(&repo_root, remote, &repo_id)?;

    let sha = if let Some(v) = git_ref {
        if sync::is_sha1_hex(v) {
            v.to_string()
        } else {
            let b = store
                .read_bytes(&format!("refs/{v}"))?
                .ok_or_else(|| anyhow::anyhow!("remote ref not found: refs/{v}"))?;
            String::from_utf8_lossy(&b).trim().to_string()
        }
    } else {
        let b = store
            .read_bytes("manifests/HEAD")?
            .ok_or_else(|| anyhow::anyhow!("remote has no data HEAD; run `xet-ai push` first"))?;
        String::from_utf8_lossy(&b).trim().to_string()
    };

    let local_cas_root = repo_root.join(".xet_ai").join("xet");
    let hash_cache_path = repo_root.join(".xet_ai").join("hash_cache.json");

    let manifest = if all_cas {
        let remote_cas_root = store.root().join("xet");
        sync::build_manifest("all-cas", &sha, &remote_cas_root, &hash_cache_path)?
    } else {
        let bytes = store
            .read_bytes(&format!("manifests/{sha}.json"))?
            .ok_or_else(|| anyhow::anyhow!("manifest missing: {sha}"))?;
        serde_json::from_slice(&bytes)?
    };

    let local_manifest_path = manifest_path(&repo_root, &sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;
    let summary = sync::pull_from_manifest_store(&store, &local_cas_root, &manifest)?;

    if let Some(ptr_bytes) = store.read_bytes(&format!("pointers/{sha}.json"))? {
        let path = pointers::pointer_cache_path(&repo_root, &sha);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, ptr_bytes)?;
    }

    if verbose {
        println!(
            "pulled manifest {} ({})",
            sha,
            local_manifest_path.display()
        );
        println!(
            "copied {} files ({} bytes)",
            summary.files_copied, summary.bytes_copied
        );
        println!("verified {} files", summary.files_verified);
    }
    Ok(())
}
