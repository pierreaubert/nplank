//! Launches the native Rust workspace client or appearance mini app.

// Rust guideline compliant 2026-02-21
use anyhow::{Context, Result, ensure};
use clap::Parser;
use gpui::AppContext;
#[cfg(not(target_os = "macos"))]
use gpui_miniapp::MiniApp;
use gpui_miniapp::MiniAppConfig;
use nplank::{client::Client, daemon, ui::WorkspaceView};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(about = "Native Plank workspaces; runs continue outside the window")]
struct Args {
    /// Open the theme and design mini app.
    #[arg(long)]
    appearance: bool,
    /// Attach to an existing daemon, usually through SSH.
    #[arg(long)]
    connect: Option<String>,
    /// Environment variable containing the remote daemon's bearer token.
    #[arg(long, default_value = "NPLANK_TOKEN")]
    token_env: String,
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    args.appearance |= std::env::var("NPLANK_APPEARANCE").is_ok_and(|value| value == "1");
    let directory = args.data_dir.unwrap_or_else(daemon::default_data_dir);
    std::fs::create_dir_all(&directory)?;
    let directory = std::fs::canonicalize(directory)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let client = if args.appearance {
        Client::new(
            &format!("http://{}", daemon::DEFAULT_ADDRESS),
            daemon::token_in(&directory)?,
        )?
    } else if let Some(url) = args.connect {
        Client::new(
            &url,
            std::env::var(&args.token_env).context("Remote daemon token is not set")?,
        )?
    } else {
        let client = Client::new(
            &format!("http://{}", daemon::DEFAULT_ADDRESS),
            daemon::token_in(&directory)?,
        )?;
        let status = runtime.block_on(client.snapshot(None, 0));
        if let Err(error) = &status {
            ensure!(
                !error.to_string().contains("Daemon version is incompatible"),
                "{error}"
            );
        }
        if status.is_err() {
            let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
                "nplankd.exe"
            } else {
                "nplankd"
            });
            ensure!(
                executable.is_file(),
                "Build both binaries with cargo build before launching the app"
            );
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("daemon.log"))?;
            Command::new(executable)
                .arg("--data-dir")
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()?;
            let mut connected = false;
            for _ in 0..40 {
                if runtime.block_on(client.snapshot(None, 0)).is_ok() {
                    connected = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            ensure!(
                connected,
                "Daemon did not start; inspect .nplank/daemon.log"
            );
        }
        client
    };
    drop(runtime);
    let appearance = args.appearance;
    let title = if appearance {
        "Plank Appearance"
    } else {
        "nplank"
    };
    let config = MiniAppConfig::new(title)
        .size(1180.0, 800.0)
        .with_theme(true)
        .scrollable(false)
        .state_file(directory.join(if appearance {
            "appearance-window.json"
        } else {
            "window.json"
        }));
    #[cfg(target_os = "macos")]
    nplank::native::run(config, move |cx| {
        cx.new(|cx| WorkspaceView::new(client, directory, appearance, cx))
    })?;
    #[cfg(not(target_os = "macos"))]
    MiniApp::run(config, move |cx| {
        cx.new(|cx| WorkspaceView::new(client, directory, appearance, cx))
    });
    Ok(())
}
