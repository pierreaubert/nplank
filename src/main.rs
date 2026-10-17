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
    path::{Path, PathBuf},
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

/// A connection failure restarts the daemon, except when the running daemon
/// reports an incompatible version: active runs may still be attached to it,
/// so that case stays an explicit error for the user to resolve.
fn should_restart_daemon(error: &anyhow::Error) -> bool {
    !error.to_string().contains("Daemon version is incompatible")
}

/// Spawns the bundled daemon next to this executable and waits until it
/// answers snapshots.
fn spawn_daemon(
    directory: &Path,
    client: &Client,
    runtime: &tokio::runtime::Runtime,
) -> Result<()> {
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
        .arg(directory)
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
    Ok(())
}

/// Watches a local daemon and restarts it if it dies after having answered.
///
/// Only used for daemons this app can spawn (never for `--connect` remotes).
/// A fresh kill is re-probed once before respawning so two app instances do
/// not race, and failed restarts back off instead of hot-looping.
fn supervise_daemon(client: Client, directory: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("nplank-daemon-watch".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            let mut backoff = Duration::from_secs(2);
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if runtime.block_on(client.snapshot(None, 0)).is_ok() {
                    backoff = Duration::from_secs(2);
                    continue;
                }
                // Re-probe: the daemon may be restarting already.
                std::thread::sleep(Duration::from_secs(1));
                let probe = runtime.block_on(client.snapshot(None, 0));
                let restartable = match &probe {
                    Ok(_) => true,
                    Err(error) => should_restart_daemon(error),
                };
                if probe.is_ok()
                    || !restartable
                    || spawn_daemon(&directory, &client, &runtime).is_err()
                {
                    if probe.is_err() && restartable {
                        std::thread::sleep(backoff);
                        backoff = std::cmp::min(backoff * 2, Duration::from_secs(60));
                    }
                    continue;
                }
                backoff = Duration::from_secs(2);
            }
        });
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    args.appearance |= std::env::var("NPLANK_APPEARANCE").is_ok_and(|value| value == "1");
    let directory = args.data_dir.unwrap_or_else(daemon::default_data_dir);
    std::fs::create_dir_all(&directory)?;
    let directory = std::fs::canonicalize(directory)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let client = if args.appearance {
        let client = Client::new(
            &format!("http://{}", daemon::DEFAULT_ADDRESS),
            daemon::token_in(&directory)?,
        )?;
        // A version mismatch stays visible in the UI; only a dead daemon is
        // spawned here, and the watchdog keeps it alive afterwards.
        if let Err(error) = runtime.block_on(client.snapshot(None, 0))
            && should_restart_daemon(&error)
        {
            spawn_daemon(&directory, &client, &runtime)?;
        }
        supervise_daemon(client.clone(), directory.clone());
        client
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
            spawn_daemon(&directory, &client, &runtime)?;
        }
        supervise_daemon(client.clone(), directory.clone());
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

#[cfg(test)]
mod tests {
    use super::should_restart_daemon;

    #[test]
    fn dead_daemon_restarts_but_version_mismatch_stays_explicit() {
        assert!(should_restart_daemon(&anyhow::anyhow!(
            "error sending request for url (http://127.0.0.1:47831/v1/snapshot?before=0)"
        )));
        assert!(!should_restart_daemon(&anyhow::anyhow!(
            "Daemon version is incompatible; finish active runs, then restart the daemon with the updated binary"
        )));
    }
}
