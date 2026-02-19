use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub r#type: String,
    pub path: PathBuf,
}

impl AppConfig {
    pub fn load(repo_root: &Path) -> Result<Self> {
        let cfg_path = config_path(repo_root);
        if !cfg_path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(cfg_path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let cfg_dir = repo_root.join(".xet_ai");
        fs::create_dir_all(&cfg_dir)?;
        let content = toml::to_string_pretty(self)?;
        fs::write(config_path(repo_root), content)?;
        Ok(())
    }
}

fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".xet_ai").join("config.toml")
}
