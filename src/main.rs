mod config;
mod repo;
mod sync;

use std::cmp::Reverse;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use config::{ConfigFile, EffectiveConfig};
use data::configurations::TranslatorConfig;
use data::{FileDownloader, FileUploadSession, XetFileInfo};
use file_reconstruction::DataOutput;
use uuid::Uuid;
use xet_ai_core::pointers;
use xet_ai_core::reachability::{self, PointerHashHydrator};
use xet_runtime::XetRuntime;

#[derive(Parser, Debug)]
#[command(name = "xet-ai", about = "Local CAS-first storage using xet-core")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init {
        #[arg(long)]
        init_config: bool,
    },
    Clean {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Smudge {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Push {
        name: Option<String>,
        #[arg(long = "ref")]
        refname: Option<String>,
        #[arg(long)]
        force_lock: bool,
        #[arg(long)]
        all_cas: bool,
    },
    Pull {
        name: Option<String>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        #[arg(long)]
        all_cas: bool,
    },
    Remote {
        #[command(subcommand)]
        command: RemoteCommands,
    },
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
    },
    Manifest {
        #[command(subcommand)]
        command: ManifestCommands,
    },
}

#[derive(Subcommand, Debug)]
enum RemoteCommands {
    Add {
        name: String,
        path: PathBuf,
        #[arg(long)]
        local: bool,
    },
    List,
    SetDefault {
        name: String,
        #[arg(long)]
        local: bool,
    },
    Refs {
        remote: Option<String>,
    },
    Head {
        remote: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DebugCommands {
    CasTree,
}

#[derive(Subcommand, Debug)]
enum ManifestCommands {
    List,
    Show { sha: String },
    Verify { sha: String },
}

fn runtime() -> Arc<XetRuntime> {
    static RUNTIME: OnceLock<Arc<XetRuntime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| XetRuntime::new().expect("failed to initialize runtime"))
        .clone()
}

fn main() {
    match runtime().external_run_async_task(async move { run().await }) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { init_config } => init(init_config),
        Commands::Clean { path } => clean(path).await,
        Commands::Smudge { path } => smudge(path).await,
        Commands::Push {
            name,
            refname,
            force_lock,
            all_cas,
        } => push(name.as_deref(), refname.as_deref(), force_lock, all_cas),
        Commands::Pull {
            name,
            git_ref,
            all_cas,
        } => pull(name.as_deref(), git_ref.as_deref(), true, all_cas),
        Commands::Remote { command } => match command {
            RemoteCommands::Add { name, path, local } => remote_add(&name, &path, local),
            RemoteCommands::List => remote_list(),
            RemoteCommands::SetDefault { name, local } => remote_set_default(&name, local),
            RemoteCommands::Refs { remote } => remote_refs(remote.as_deref()),
            RemoteCommands::Head { remote } => remote_head(remote.as_deref()),
        },
        Commands::Debug { command } => match command {
            DebugCommands::CasTree => debug_cas_tree(),
        },
        Commands::Manifest { command } => match command {
            ManifestCommands::List => manifest_list(),
            ManifestCommands::Show { sha } => manifest_show(&sha),
            ManifestCommands::Verify { sha } => manifest_verify(&sha),
        },
    }
}

fn init(init_config: bool) -> Result<()> {
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

async fn clean(path: Option<PathBuf>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let base = repo_root.join(".xet_ai");
    fs::create_dir_all(&base)?;

    let cfg = Arc::new(TranslatorConfig::local_config(base.clone())?);

    let source_path = prepare_clean_source(&repo_root, &base, path)?;
    let source_size = fs::metadata(&source_path)?.len();

    let session = FileUploadSession::new(cfg, None).await?;
    let mut cleaner = session.start_clean(None, source_size, None).await;

    let mut file = fs::File::open(&source_path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        cleaner.add_data(&buf[..n]).await?;
    }

    let (file_info, _) = cleaner.finish().await?;
    session.finalize().await?;

    io::stdout().write_all(file_info.as_pointer_file()?.as_bytes())?;

    if source_path.starts_with(base.join("tmp")) {
        let _ = fs::remove_file(source_path);
    }

    Ok(())
}

fn prepare_clean_source(repo_root: &Path, base: &Path, path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = path {
        let full_path = repo::resolve_path(repo_root, &path);
        if full_path.exists() {
            return Ok(full_path);
        }
    }

    let tmp_dir = base.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_path = tmp_dir.join(format!("clean-stdin-{}.tmp", std::process::id()));

    let mut temp = fs::File::create(&tmp_path)?;
    let mut stdin = io::stdin().lock();
    io::copy(&mut stdin, &mut temp)?;
    temp.flush()?;

    Ok(tmp_path)
}

async fn smudge(path: Option<PathBuf>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let base = repo_root.join(".xet_ai");
    fs::create_dir_all(&base)?;

    let mut pointer_bytes = Vec::new();
    io::stdin().read_to_end(&mut pointer_bytes)?;

    let xet_file: XetFileInfo = match serde_json::from_slice(&pointer_bytes) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("warning: input is not a xet pointer; passing through unchanged");
            io::stdout().write_all(&pointer_bytes)?;
            return Ok(());
        }
    };

    let cfg = Arc::new(TranslatorConfig::local_config(base)?);
    let downloader = FileDownloader::new(cfg).await?;
    let file_label: Arc<str> = Arc::from(
        path.as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "<unknown>".to_string()),
    );
    let output = DataOutput::writer(io::stdout());

    let hash = xet_file
        .merkle_hash()
        .map_err(|_| anyhow!("Xet hash is corrupted"))?;

    match downloader
        .smudge_file_from_hash(&hash, file_label.clone(), output, None, None)
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("not found") || msg.contains("no such") || msg.contains("missing") {
                let effective = EffectiveConfig::load(&repo_root)?;
                let already_attempted = std::env::var("XET_AI_SMUDGE_AUTOPULL_ATTEMPT")
                    .ok()
                    .as_deref()
                    == Some("1");

                if effective.auto_pull_on_smudge && !already_attempted {
                    if let Some(remote) = effective.default_remote {
                        std::env::set_var("XET_AI_SMUDGE_AUTOPULL_ATTEMPT", "1");
                        let pulled = pull(Some(&remote), None, false, false).is_ok();
                        std::env::remove_var("XET_AI_SMUDGE_AUTOPULL_ATTEMPT");

                        if pulled {
                            let cfg2 = Arc::new(TranslatorConfig::local_config(
                                repo_root.join(".xet_ai"),
                            )?);
                            let downloader2 = FileDownloader::new(cfg2).await?;
                            let output2 = DataOutput::writer(io::stdout());
                            if downloader2
                                .smudge_file_from_hash(
                                    &hash,
                                    file_label.clone(),
                                    output2,
                                    None,
                                    None,
                                )
                                .await
                                .is_ok()
                            {
                                return Ok(());
                            }
                        }
                    }
                }

                eprintln!(
                    "warning: missing CAS data; run `xet-ai pull <remote>` then `git checkout -f -- {}`",
                    file_label
                );
                io::stdout().write_all(&pointer_bytes)?;
                Ok(())
            } else {
                eprintln!("error: failed to hydrate pointer: {e}");
                bail!(e)
            }
        }
    }
}

fn debug_cas_tree() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cas_root = repo_root.join(".xet_ai").join("xet");
    eprintln!("CAS root: {}", cas_root.display());
    for (dir, included) in sync::describe_cas_tree(&cas_root)? {
        let mark = if included { "INCLUDED" } else { "excluded" };
        eprintln!(" - {dir}: {mark}");
    }
    Ok(())
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
    effective
        .default_remote
        .clone()
        .ok_or_else(|| anyhow!("no remote specified and no default_remote configured"))
}

fn remote_repo_root(repo_root: &Path, remote: &config::RemoteConfig) -> Result<PathBuf> {
    let repo_id = repo::load_repo_id(repo_root)?;
    Ok(repo::resolve_path(repo_root, &remote.path).join(repo_id))
}

fn manifest_list() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let dir = manifest_dir(&repo_root);
    if !dir.exists() {
        return Ok(());
    }

    let mut rows: Vec<(String, usize, u64)> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let manifest = sync::read_manifest(&path)?;
        let sha = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        rows.push((sha, manifest.entries.len(), manifest.total_bytes));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (sha, count, total) in rows {
        println!("{sha}\tentries={count}\ttotal_bytes={total}");
    }
    Ok(())
}

fn manifest_show(sha: &str) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let path = manifest_path(&repo_root, sha);
    let manifest = sync::read_manifest(&path)?;

    println!("repo_id: {}", manifest.repo_id);
    println!("git_sha: {}", manifest.git_sha);
    println!("entries: {}", manifest.entries.len());
    println!("total_bytes: {}", manifest.total_bytes);

    let mut entries = manifest.entries.clone();
    entries.sort_by_key(|e| Reverse(e.size));
    println!("largest_entries:");
    for e in entries.into_iter().take(10) {
        println!("  {}\t{}", e.size, e.relpath);
    }
    Ok(())
}

fn manifest_verify(sha: &str) -> Result<()> {
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

fn remote_add(name: &str, path: &Path, local: bool) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let mut cfg = if local {
        config::load_local(&repo_root)?
    } else {
        config::load_shared(&repo_root)?
    };
    cfg.remotes.insert(
        name.to_string(),
        config::RemoteConfig {
            r#type: "filesystem".to_string(),
            path: path.to_path_buf(),
        },
    );

    if local {
        config::save_local(&repo_root, &cfg)
    } else {
        config::save_shared(&repo_root, &cfg)
    }
}

fn remote_set_default(name: &str, local: bool) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let eff = EffectiveConfig::load(&repo_root)?;
    if !eff.remotes.contains_key(name) {
        bail!("remote `{name}` does not exist in effective config");
    }

    let mut cfg = if local {
        config::load_local(&repo_root)?
    } else {
        config::load_shared(&repo_root)?
    };
    cfg.default_remote = Some(name.to_string());

    if local {
        config::save_local(&repo_root, &cfg)
    } else {
        config::save_shared(&repo_root, &cfg)
    }
}

fn remote_list() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    for (name, remote) in cfg.remotes {
        println!("{name}\t{}\t{}", remote.r#type, remote.path.display());
    }
    Ok(())
}

fn remote_refs(remote_name: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let root = remote_repo_root(&repo_root, remote)?;
    for (refname, sha) in sync::list_remote_refs(&root)? {
        println!("{refname}\t{sha}");
    }
    Ok(())
}

fn remote_head(remote_name: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    let root = remote_repo_root(&repo_root, remote)?;
    let sha = sync::read_head(&root.join("manifests").join("HEAD"))?;
    println!("{sha}");
    Ok(())
}

fn push(
    remote_name: Option<&str>,
    refname_opt: Option<&str>,
    force_lock: bool,
    all_cas: bool,
) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = EffectiveConfig::load(&repo_root)?;
    let name = resolve_remote_name(remote_name, &cfg)?;
    let remote = cfg
        .remotes
        .get(&name)
        .with_context(|| format!("remote `{name}` not found"))?;
    if remote.r#type != "filesystem" {
        bail!("unsupported remote type `{}`", remote.r#type);
    }

    let repo_id = repo::load_repo_id(&repo_root)?;
    let git_sha = repo::git_head_sha(&repo_root)?;
    let xet_ai_root = repo_root.join(".xet_ai");
    let local_cas_root = xet_ai_root.join("xet");

    let remote_repo_root = repo::resolve_path(&repo_root, &remote.path).join(&repo_id);
    let _lock_guard = sync::acquire_push_lock(&remote_repo_root, force_lock)?;

    let remote_cas_root = remote_repo_root.join("xet");
    let remote_manifest_dir = remote_repo_root.join("manifests");

    let hash_cache_path = xet_ai_root.join("hash_cache.json");
    let pointer_index = reachability::load_or_build_pointer_index(&repo_root, &git_sha, &repo_id)?;
    let mut hydrator = PointerHashHydrator::new(&local_cas_root)?;
    let plan = reachability::plan_reachable_cas(
        &repo_root,
        &local_cas_root,
        &repo_id,
        &git_sha,
        &mut hydrator,
    )?;

    let manifest = if all_cas {
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
    let summary = sync::push_with_manifest(&local_cas_root, &remote_cas_root, &manifest)?;

    let local_manifest_path = manifest_path(&repo_root, &git_sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;

    let manifest_path = remote_manifest_dir.join(format!("{git_sha}.json"));
    sync::write_manifest_atomic(&manifest_path, &manifest)?;
    let head_path = remote_manifest_dir.join("HEAD");
    sync::write_head_atomic(&head_path, &git_sha)?;

    pointers::cache_pointer_index(&repo_root, &pointer_index)?;
    let remote_pointer_dir = remote_repo_root.join("pointers");
    sync::atomic_write_string(
        &remote_pointer_dir.join(format!("{git_sha}.json")),
        &serde_json::to_string_pretty(&pointer_index)?,
    )?;
    sync::write_head_atomic(&remote_pointer_dir.join("HEAD"), &git_sha)?;

    let refname = refname_opt
        .map(|s| s.to_string())
        .or_else(|| repo::git_current_branch_short(&repo_root).ok().flatten())
        .unwrap_or_else(|| "HEAD".to_string());
    sync::atomic_write_string(
        &remote_repo_root.join("refs").join(&refname),
        &format!("{git_sha}\n"),
    )?;

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
    if !all_cas {
        println!(
            "reachability: {} pointer files, {} required CAS files",
            plan.pointer_paths.len(),
            plan.required_cas_relpaths.len()
        );
    }
    println!("updated HEAD -> {}", git_sha);
    Ok(())
}

fn pull(
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
    if remote.r#type != "filesystem" {
        bail!("unsupported remote type `{}`", remote.r#type);
    }

    let remote_repo_root = remote_repo_root(&repo_root, remote)?;
    let sha = sync::resolve_ref_or_sha(&remote_repo_root, git_ref)?;

    let remote_manifest_path = remote_repo_root
        .join("manifests")
        .join(format!("{sha}.json"));
    let remote_cas_root = remote_repo_root.join("xet");
    let local_cas_root = repo_root.join(".xet_ai").join("xet");
    let hash_cache_path = repo_root.join(".xet_ai").join("hash_cache.json");

    let manifest = if all_cas {
        sync::build_manifest("all-cas", &sha, &remote_cas_root, &hash_cache_path)?
    } else {
        sync::read_manifest(&remote_manifest_path)?
    };

    let local_manifest_path = manifest_path(&repo_root, &sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;

    let summary = sync::pull_from_manifest(&remote_cas_root, &local_cas_root, &manifest)?;

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
