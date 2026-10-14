//! Controls the same persistent sessions as the native workspace app.

// Rust guideline compliant 2026-02-21
use anyhow::Result;
use clap::{Parser, Subcommand};
use nplank::{
    client::Client,
    daemon,
    model::{Backend, Command},
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:47831")]
    url: String,
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Print workspace metadata and an optional transcript page.
    Snapshot {
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 0)]
        before: i64,
    },
    /// Submit a typed JSON command from a file.
    Command { file: PathBuf },
    /// Create two explicitly labeled demo sessions for a model-free smoke check.
    Demo {
        #[arg(long, default_value = ".")]
        directory: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token_file = args
        .token_file
        .unwrap_or_else(|| daemon::default_data_dir().join("token"));
    let client = Client::new(
        &args.url,
        std::fs::read_to_string(token_file)?.trim().into(),
    )?;
    match args.command {
        Action::Snapshot { session, before } => println!(
            "{}",
            serde_json::to_string_pretty(&client.snapshot(session.as_deref(), before).await?)?
        ),
        Action::Command { file } => {
            let command: Command = serde_json::from_slice(&std::fs::read(file)?)?;
            println!(
                "{}",
                serde_json::to_string(&client.execute(&command).await?)?
            );
        }
        Action::Demo { directory } => {
            let directory = std::fs::canonicalize(directory)?
                .to_string_lossy()
                .into_owned();
            let workspace = client
                .execute(&Command::CreateWorkspace {
                    name: "MVP demo".into(),
                    directory,
                })
                .await?
                .id;
            for name in ["Research · demo", "Build · demo"] {
                let session = client
                    .execute(&Command::CreateSession {
                        workspace_id: workspace.clone(),
                        name: name.into(),
                        backend: Backend::Demo,
                    })
                    .await?
                    .id;
                client
                    .execute(&Command::Prompt {
                        session_id: session.clone(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                        text: "Explain how this session survives a window closing.".into(),
                    })
                    .await?;
                println!("{name}: {session}");
            }
        }
    }
    Ok(())
}
