use std::fs;
use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct SyncSummary {
    pub files_copied: u64,
    pub bytes_copied: u64,
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
        let dst_path = dst.join(rel);
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
