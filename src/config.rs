use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

const SHARED_CONFIG_FILE: &str = ".xet_ai.toml";
const LOCAL_CONFIG_FILE: &str = "config.local.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteConfig>,
    pub default_remote: Option<String>,
    pub auto_pull_on_smudge: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub remotes: BTreeMap<String, RemoteConfig>,
    pub default_remote: Option<String>,
    pub auto_pull_on_smudge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub r#type: String,
    pub path: PathBuf,
}

impl EffectiveConfig {
    pub fn load(repo_root: &Path) -> Result<Self> {
        let shared = load_shared(repo_root)?;
        let local = load_local(repo_root)?;

        let mut remotes = shared.remotes;
        for (k, v) in local.remotes {
            remotes.insert(k, v);
        }

        let default_remote = local.default_remote.or(shared.default_remote);
        let auto_pull_on_smudge = local
            .auto_pull_on_smudge
            .or(shared.auto_pull_on_smudge)
            .unwrap_or(false);

        Ok(Self {
            remotes,
            default_remote,
            auto_pull_on_smudge,
        })
    }
}

pub fn load_shared(repo_root: &Path) -> Result<ConfigFile> {
    let path = shared_config_path(repo_root);
    if !path.exists() {
        return Ok(ConfigFile::default());
    }
    let content = fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

pub fn save_shared(repo_root: &Path, cfg: &ConfigFile) -> Result<()> {
    let content = toml::to_string_pretty(cfg)?;
    fs::write(shared_config_path(repo_root), content)?;
    Ok(())
}

pub fn load_local(repo_root: &Path) -> Result<ConfigFile> {
    let path = local_config_path(repo_root);
    if !path.exists() {
        return Ok(ConfigFile::default());
    }
    let content = fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

pub fn save_local(repo_root: &Path, cfg: &ConfigFile) -> Result<()> {
    let cfg_dir = repo_root.join(".xet_ai");
    fs::create_dir_all(&cfg_dir)?;
    let content = toml::to_string_pretty(cfg)?;
    fs::write(local_config_path(repo_root), content)?;
    Ok(())
}

pub fn shared_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(SHARED_CONFIG_FILE)
}

pub fn local_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".xet_ai").join(LOCAL_CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn config_merge_local_overrides_shared() {
        let root = std::env::temp_dir().join(format!("xet-ai-config-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create root");

        let mut shared = ConfigFile::default();
        shared.default_remote = Some("origin".to_string());
        shared.auto_pull_on_smudge = Some(false);
        shared.remotes.insert(
            "origin".to_string(),
            RemoteConfig {
                r#type: "filesystem".to_string(),
                path: PathBuf::from("/shared"),
            },
        );
        save_shared(&root, &shared).expect("save shared");

        let mut local = ConfigFile::default();
        local.default_remote = Some("local-remote".to_string());
        local.auto_pull_on_smudge = Some(true);
        local.remotes.insert(
            "origin".to_string(),
            RemoteConfig {
                r#type: "filesystem".to_string(),
                path: PathBuf::from("/local-override"),
            },
        );
        local.remotes.insert(
            "local-remote".to_string(),
            RemoteConfig {
                r#type: "filesystem".to_string(),
                path: PathBuf::from("/local-only"),
            },
        );
        save_local(&root, &local).expect("save local");

        let eff = EffectiveConfig::load(&root).expect("load effective");
        assert_eq!(eff.default_remote.as_deref(), Some("local-remote"));
        assert!(eff.auto_pull_on_smudge);
        assert_eq!(
            eff.remotes.get("origin").unwrap().path,
            PathBuf::from("/local-override")
        );
        assert!(eff.remotes.contains_key("local-remote"));

        let _ = fs::remove_dir_all(root);
    }
}
