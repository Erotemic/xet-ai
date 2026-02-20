//! Repository bootstrap and path helpers.
//!
//! This module owns creation of local `.xet_ai` state and small helpers for
//! working with repository-relative paths and git-side setup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::git;

pub const REPO_ID_FILE: &str = ".xet_ai_repo_id";

pub fn repo_root() -> Result<PathBuf> {
    git::repo_root(Path::new("."))
}

pub fn run_git<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = Command::new("git").args(args).status()?;
    if !status.success() {
        bail!("git command failed");
    }
    Ok(())
}

pub fn append_if_missing(path: &Path, line: &str) -> Result<()> {
    let mut content = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    if !content.lines().any(|l| l.trim() == line) {
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }
        content.push_str(line);
        content.push('\n');
        fs::write(path, content)?;
    }
    Ok(())
}

pub fn resolve_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

pub fn repo_id_path(repo_root: &Path) -> PathBuf {
    repo_root.join(REPO_ID_FILE)
}

pub fn load_repo_id(repo_root: &Path) -> Result<String> {
    let id = fs::read_to_string(repo_id_path(repo_root))
        .with_context(|| format!("missing {}; run `xet-ai init`", REPO_ID_FILE))?;
    let trimmed = id.trim();
    if trimmed.is_empty() {
        bail!("{} is empty", REPO_ID_FILE);
    }
    Ok(trimmed.to_string())
}

pub fn git_head_sha(repo_root: &Path) -> Result<String> {
    let repo = git::open_repo(repo_root)?;
    git::head_sha(&repo)
}

pub fn git_current_branch_short(repo_root: &Path) -> Result<Option<String>> {
    let repo = git::open_repo(repo_root)?;
    let head = repo.head().map_err(|e| anyhow!(e))?;
    if head.is_branch() {
        Ok(head.shorthand().map(str::to_string))
    } else {
        Ok(None)
    }
}
