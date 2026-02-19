use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct SyncSummary {
    pub files_copied: u64,
    pub bytes_copied: u64,
}

const ALLOWLIST_TOP_LEVEL: &[&str] = &["cas", "shards", "mdb", "xorbs", "merkledb"];

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
    ALLOWLIST_TOP_LEVEL.iter().any(|x| x == &first)
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

pub fn copy_missing_recursive(src: &Path, dst: &Path) -> Result<SyncSummary> {
    let mut summary = SyncSummary::default();
    if !src.exists() {
        return Ok(summary);
    }

    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(src)?;
        if !sync_included(rel) {
            continue;
        }
        let dst_path: PathBuf = dst.join(rel);
        if dst_path.exists() {
            continue;
        }
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &dst_path)?;
        summary.files_copied += 1;
        summary.bytes_copied += entry.metadata()?.len();
    }

    Ok(summary)
}
