use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Result};
use clap::{Parser, Subcommand};
use data::configurations::TranslatorConfig;
use data::{FileDownloader, FileUploadSession, XetFileInfo};
use file_reconstruction::DataOutput;
use xet_ai_core::commands::{self, PushMode};
use xet_ai_core::{repo, sync};
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
        #[arg(long = "track")]
        track: Vec<String>,
    },
    Status,
    Track {
        patterns: Vec<String>,
    },
    Doctor,
    Version,
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
        plan_only: bool,
        #[arg(long)]
        force_lock: bool,
        #[arg(long)]
        all_cas: bool,
        #[arg(long)]
        minimal_no_validate: bool,
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
    Tx {
        #[command(subcommand)]
        command: RemoteTxCommands,
    },
}

#[derive(Subcommand, Debug)]
enum RemoteTxCommands {
    List {
        remote: Option<String>,
    },
    Gc {
        remote: Option<String>,
        #[arg(long, default_value_t = 60)]
        older_than: u64,
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
        Commands::Init { init_config, track } => commands::init(init_config, &track),
        Commands::Status => commands::status(),
        Commands::Track { patterns } => commands::track(&patterns),
        Commands::Doctor => commands::doctor(),
        Commands::Version => {
            println!("xet-ai {}", env!("CARGO_PKG_VERSION"));
            println!(
                "build_commit: {}",
                option_env!("XET_AI_GIT_COMMIT").unwrap_or("unknown")
            );
            println!("xet_core_rev: a7661a7e63626466561e88e63113001a193a36ee");
            Ok(())
        }
        Commands::Clean { path } => clean(path).await,
        Commands::Smudge { path } => smudge(path).await,
        Commands::Push {
            name,
            refname,
            plan_only,
            force_lock,
            all_cas,
            minimal_no_validate,
        } => {
            let mode = if all_cas {
                PushMode::AllCas
            } else if minimal_no_validate {
                PushMode::MinimalNoValidate
            } else {
                PushMode::MinimalValidate
            };
            commands::push(
                name.as_deref(),
                refname.as_deref(),
                force_lock,
                mode,
                plan_only,
            )
            .await
        }
        Commands::Pull {
            name,
            git_ref,
            all_cas,
        } => commands::pull(name.as_deref(), git_ref.as_deref(), true, all_cas),
        Commands::Remote { command } => match command {
            RemoteCommands::Add { name, path, local } => commands::remote_add(&name, &path, local),
            RemoteCommands::List => commands::remote_list(),
            RemoteCommands::SetDefault { name, local } => {
                commands::remote_set_default(&name, local)
            }
            RemoteCommands::Refs { remote } => commands::remote_refs(remote.as_deref()),
            RemoteCommands::Head { remote } => commands::remote_head(remote.as_deref()),
            RemoteCommands::Tx { command } => match command {
                RemoteTxCommands::List { remote } => commands::remote_tx_list(remote.as_deref()),
                RemoteTxCommands::Gc { remote, older_than } => {
                    commands::remote_tx_gc(remote.as_deref(), older_than)
                }
            },
        },
        Commands::Debug { command } => match command {
            DebugCommands::CasTree => debug_cas_tree(),
        },
        Commands::Manifest { command } => match command {
            ManifestCommands::List => commands::manifest_list(),
            ManifestCommands::Show { sha } => commands::manifest_show(&sha),
            ManifestCommands::Verify { sha } => commands::manifest_verify(&sha),
        },
    }
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
            eprintln!("xet-ai: warning: input is not a xet pointer; passing through unchanged");
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
                if let Some(remote) = commands::should_attempt_smudge_autopull(&repo_root)? {
                    std::env::set_var("XET_AI_SMUDGE_AUTOPULL_ATTEMPT", "1");
                    let pulled = commands::pull(Some(&remote), None, false, false).is_ok();
                    std::env::remove_var("XET_AI_SMUDGE_AUTOPULL_ATTEMPT");
                    if pulled {
                        let cfg2 =
                            Arc::new(TranslatorConfig::local_config(repo_root.join(".xet_ai"))?);
                        let downloader2 = FileDownloader::new(cfg2).await?;
                        let output2 = DataOutput::writer(io::stdout());
                        if downloader2
                            .smudge_file_from_hash(&hash, file_label.clone(), output2, None, None)
                            .await
                            .is_ok()
                        {
                            return Ok(());
                        }
                    }
                }

                eprintln!(
                    "xet-ai: warning: missing CAS data; run `xet-ai pull <remote>` then `git checkout -f -- {}`",
                    file_label
                );
                io::stdout().write_all(&pointer_bytes)?;
                Ok(())
            } else {
                eprintln!("xet-ai: error: failed to hydrate pointer: {e}");
                bail!(e)
            }
        }
    }
}

fn debug_cas_tree() -> Result<()> {
    let repo_root = repo::repo_root()?;
    let cas_root = repo_root.join(".xet_ai").join("xet");
    for (name, included) in sync::describe_cas_tree(&cas_root)? {
        println!("{}\t{}", if included { "include" } else { "exclude" }, name);
    }
    Ok(())
}
