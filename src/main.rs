mod config;
mod repo;
mod sync;

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
        path: PathBuf,
    },
    Smudge {
        #[arg(long)]
        path: PathBuf,
    },
    Push {
        name: String,
    },
    Pull {
        name: String,
    },
    Remote {
        #[command(subcommand)]
        command: RemoteCommands,
    },
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
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
        Commands::Pull { name } => pull(&name),
        Commands::Remote { command } => match command {
            RemoteCommands::Add { name, path } => remote_add(&name, &path),
            RemoteCommands::List => remote_list(),
        },
        Commands::Debug { command } => match command {
            DebugCommands::CasTree => debug_cas_tree(),
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

async fn clean(path: PathBuf) -> Result<()> {
    let repo_root = repo::repo_root()?;
    let base = repo_root.join(".xet_ai");
    fs::create_dir_all(&base)?;

    let cfg = Arc::new(TranslatorConfig::local_config(base)?);
    let full_path = repo::resolve_path(&repo_root, &path);

    let (mut reader, size): (Box<dyn Read + Send>, u64) = if full_path.exists() {
        let file = fs::File::open(&full_path)?;
        let size = file.metadata()?.len();
        (Box::new(file), size)
    } else {
        let mut stdin_buf = Vec::new();
        io::stdin().read_to_end(&mut stdin_buf)?;
        let size = stdin_buf.len() as u64;
        (Box::new(io::Cursor::new(stdin_buf)), size)
    };

    let session = FileUploadSession::new(cfg, None).await?;
    let mut cleaner = session.start_clean(None, size, None).await;

    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        cleaner.add_data(&buf[..n]).await?;
    }
    let (file_info, _) = cleaner.finish().await?;
    session.finalize().await?;
    io::stdout().write_all(file_info.as_pointer_file()?.as_bytes())?;
    Ok(())
}

async fn smudge(path: PathBuf) -> Result<()> {
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
    let file_name: Arc<str> = Arc::from(path.to_string_lossy().into_owned());
    let output = DataOutput::writer(io::stdout());

    let hash = xet_file
        .merkle_hash()
        .map_err(|_| anyhow!("Xet hash is corrupted"))?;

    match downloader
        .smudge_file_from_hash(&hash, file_name, output, None, None)
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("not found") || msg.contains("no such") || msg.contains("missing") {
                eprintln!(
                    "warning: missing CAS data; run `xet-ai pull <remote>` then `git checkout -f -- {}`",
                    path.display()
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
    let src = repo_root.join(".xet_ai").join("xet");
    let dst = repo::resolve_path(&repo_root, &remote.path)
        .join(repo_id)
        .join("xet");
    let summary = sync::copy_missing_recursive(&src, &dst)?;
    println!(
        "copied {} files ({} bytes)",
        summary.files_copied, summary.bytes_copied
    );
    Ok(())
}

fn pull(name: &str) -> Result<()> {
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
    let src = repo::resolve_path(&repo_root, &remote.path)
        .join(repo_id)
        .join("xet");
    let dst = repo_root.join(".xet_ai").join("xet");
    let summary = sync::copy_missing_recursive(&src, &dst)?;
    println!(
        "copied {} files ({} bytes)",
        summary.files_copied, summary.bytes_copied
    );
    Ok(())
}
