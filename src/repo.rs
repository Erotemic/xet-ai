use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

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
