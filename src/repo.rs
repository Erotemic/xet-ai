use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub const REPO_ID_FILE: &str = ".xet_ai_repo_id";

pub fn repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!(
            "not inside a git repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

pub fn run_git<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = Command::new("git").args(args).status()?;
    if !status.success() {
        bail!("git command failed");
    }
    Ok(())
}

pub fn run_git_capture_stdout(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn git_head_sha(repo_root: &Path) -> Result<String> {
    run_git_capture_stdout(repo_root, &["rev-parse", "HEAD"])
}

pub fn git_current_branch_short(repo_root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .context("failed to execute git")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    Ok(None)
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
