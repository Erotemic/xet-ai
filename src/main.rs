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
use config::AppConfig;
use data::configurations::TranslatorConfig;
use data::{FileDownloader, FileUploadSession, XetFileInfo};
use file_reconstruction::DataOutput;
use uuid::Uuid;
use xet_runtime::XetRuntime;

#[derive(Parser, Debug)]
#[command(name = "xet-ai", about = "Local CAS-first storage using xet-core")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init,
    Clean {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Smudge {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Push {
        name: String,
    },
    Pull {
        name: String,
        #[arg(long = "ref")]
        git_ref: Option<String>,
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
    Add { name: String, path: PathBuf },
    List,
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
        Commands::Init => init(),
        Commands::Clean { path } => clean(path).await,
        Commands::Smudge { path } => smudge(path).await,
        Commands::Push { name } => push(&name),
        Commands::Pull { name, git_ref } => pull(&name, git_ref.as_deref()),
        Commands::Remote { command } => match command {
            RemoteCommands::Add { name, path } => remote_add(&name, &path),
            RemoteCommands::List => remote_list(),
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

fn init() -> Result<()> {
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

fn remote_add(name: &str, path: &Path) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let mut cfg = AppConfig::load(&repo_root)?;
    cfg.remotes.insert(
        name.to_string(),
        config::RemoteConfig {
            r#type: "filesystem".to_string(),
            path: path.to_path_buf(),
        },
    );
    cfg.save(&repo_root)
}

fn remote_list() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = AppConfig::load(&repo_root)?;
    for (name, remote) in cfg.remotes {
        println!("{name}\t{}\t{}", remote.r#type, remote.path.display());
    }
    Ok(())
}

fn push(name: &str) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = AppConfig::load(&repo_root)?;
    let remote = cfg
        .remotes
        .get(name)
        .with_context(|| format!("remote `{name}` not found"))?;
    if remote.r#type != "filesystem" {
        bail!("unsupported remote type `{}`", remote.r#type);
    }

    let repo_id = repo::load_repo_id(&repo_root)?;
    let git_sha = repo::git_head_sha(&repo_root)?;
    let xet_ai_root = repo_root.join(".xet_ai");
    let local_cas_root = xet_ai_root.join("xet");

    let remote_repo_root = repo::resolve_path(&repo_root, &remote.path).join(&repo_id);
    let remote_cas_root = remote_repo_root.join("xet");
    let remote_manifest_dir = remote_repo_root.join("manifests");

    let hash_cache_path = xet_ai_root.join("hash_cache.json");
    let manifest = sync::build_manifest(&repo_id, &git_sha, &local_cas_root, &hash_cache_path)?;
    let summary = sync::push_with_manifest(&local_cas_root, &remote_cas_root, &manifest)?;

    let local_manifest_path = manifest_path(&repo_root, &git_sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;

    let manifest_path = remote_manifest_dir.join(format!("{git_sha}.json"));
    sync::write_manifest_atomic(&manifest_path, &manifest)?;
    let head_path = remote_manifest_dir.join("HEAD");
    sync::write_head_atomic(&head_path, &git_sha)?;

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
    println!("updated HEAD -> {}", git_sha);
    Ok(())
}

fn pull(name: &str, git_ref: Option<&str>) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cfg = AppConfig::load(&repo_root)?;
    let remote = cfg
        .remotes
        .get(name)
        .with_context(|| format!("remote `{name}` not found"))?;
    if remote.r#type != "filesystem" {
        bail!("unsupported remote type `{}`", remote.r#type);
    }

    let repo_id = repo::load_repo_id(&repo_root)?;
    let remote_repo_root = repo::resolve_path(&repo_root, &remote.path).join(&repo_id);
    let remote_manifest_dir = remote_repo_root.join("manifests");

    let sha = if let Some(r) = git_ref {
        r.to_string()
    } else {
        sync::read_head(&remote_manifest_dir.join("HEAD"))?
    };

    let remote_manifest_path = remote_manifest_dir.join(format!("{sha}.json"));
    let manifest = sync::read_manifest(&remote_manifest_path)?;

    let local_manifest_path = manifest_path(&repo_root, &sha);
    sync::cache_manifest(&local_manifest_path, &manifest)?;

    let local_cas_root = repo_root.join(".xet_ai").join("xet");
    let remote_cas_root = remote_repo_root.join("xet");
    let summary = sync::pull_from_manifest(&remote_cas_root, &local_cas_root, &manifest)?;

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
    Ok(())
}
