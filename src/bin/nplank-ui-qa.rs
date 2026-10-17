//! Launches the real native client against an isolated, disposable QA daemon.

// Rust guideline compliant 2026-02-21
use anyhow::{Context, Result, ensure};
use clap::Parser;
use nplank::{
    client::Client,
    model::{Backend, Command},
    store::Store,
};
use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Parser)]
#[command(about = "Isolated native UI verification; no model weights are loaded")]
struct Args {
    /// Bundled Plank executable; defaults to plank/target/debug/plank.
    #[arg(long)]
    plank: Option<PathBuf>,
    /// Prepare and verify fixture files without opening a socket or window.
    #[arg(long)]
    prepare_only: bool,
    /// Retain temporary QA files after closing the window.
    #[arg(long)]
    keep_data: bool,
    /// Start with the static native question layout instead of the Plank controls tab.
    #[arg(long)]
    questions: bool,
}

#[derive(Debug)]
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
    workspace: PathBuf,
    engine: PathBuf,
    session: String,
}

impl Fixture {
    fn prepare(root: &Path, engine: &Path) -> Result<Self> {
        let home = root.join("home");
        let state = root.join("state");
        let workspace = root.join("workspace");
        for path in [&home, &state, &workspace] {
            std::fs::create_dir_all(path)?;
        }
        std::fs::create_dir_all(home.join(".plank"))?;
        std::fs::write(
            home.join(".plank/settings.json"),
            "{\"ui\":{\"notifications\":\"never\"}}\n",
        )?;
        std::fs::write(
            home.join(".plank/MEMORY.md"),
            "# Memory\n\n- [project] qa-confirmation: A disposable note for testing /forget approval.\n",
        )?;
        std::fs::write(
            workspace.join("example.rs"),
            "fn main() {\n    println!(\"UI verification fixture\");\n}\n",
        )?;
        std::fs::write(
            workspace.join("large-editor.rs"),
            "// 🦀 Large editor verification fixture\n".repeat(4096),
        )?;
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "example.rs"],
            vec![
                "-c",
                "user.name=QA",
                "-c",
                "user.email=qa@example.invalid",
                "commit",
                "-m",
                "QA fixture",
            ],
        ] {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&workspace)
                .args(args)
                .output()?;
            ensure!(
                output.status.success(),
                "QA Git setup failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["worktree", "add", "-b", "feature/quiet-ui"])
            .arg(root.join("linked-checkout"))
            .output()?;
        ensure!(
            output.status.success(),
            "QA worktree setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let store = Store::open(&state.join("state.sqlite"))?;
        let workspace_id =
            store.create_workspace("UI verification", &workspace.to_string_lossy())?;
        let session = store.create_session(
            &workspace_id,
            "Plank controls · no model",
            Backend::Local {
                executable: engine.to_string_lossy().into(),
                args: engine_args(&workspace),
            },
        )?;
        let sample = store.create_session(&workspace_id, "Transcript sample", Backend::Demo)?;
        store.append(&sample, "user", "Fixture: earlier context")?;
        store.append(
            &sample,
            "assistant",
            "This reference history should be collapsed in the live view.",
        )?;
        store.reset(&sample)?;
        store.append(&sample, "user", "Fixture: current context")?;
        store.append(&sample, "assistant", "Expand Earlier context, inspect Older messages when available, and return to the Plank tab to exercise /command and the controls.")?;
        for multi in [false, true] {
            store.approval(&sample, store.session(&sample)?.generation, nplank::model::PendingApproval {
                id: format!("qa-question-{multi}"),
                requirement: serde_json::json!({"kind":"plank_question", "revision": format!("qa-{multi}"), "request": {"multi":multi}}),
                tool: if multi { "Choose checks" } else { "Choose an approach" }.into(),
                arguments: "**Layout fixture:** select options and verify explicit confirmation. This static fixture has no engine waiting for an answer.".into(),
                document: None,
                choices: vec![
                    nplank::model::ApprovalChoice { id:"1".into(), label:"Keep the conversation in focus and open project navigation only when it is needed, with enough room for a long description on small screens.".into() },
                    nplank::model::ApprovalChoice { id:"2".into(), label:"Pin project navigation beside the conversation while reviewing several worktrees.".into() },
                    nplank::model::ApprovalChoice { id:"0".into(), label:"Decline".into() },
                ],
            })?;
        }
        Ok(Self {
            root: root.into(),
            home,
            state,
            workspace,
            engine: engine.into(),
            session,
        })
    }

    async fn probe_engine(&self) -> Result<usize> {
        let mut child = tokio::process::Command::new(&self.engine)
            .args(engine_args(&self.workspace))
            .args(["--ui", "console"])
            .env("HOME", &self.home)
            .env("PLANK_GUI_PROTOCOL", "1")
            .env("PLANK_GUI_NOTIFICATIONS", "1")
            .current_dir(&self.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child.stdin.take().context("QA engine stdin unavailable")?;
        input
            .write_all(b"{\"type\":\"prompt\",\"text\":\"/command\"}\n")
            .await?;
        drop(input);
        let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
            .await
            .context("QA engine did not finish its /command probe")??;
        ensure!(
            output.status.success(),
            "QA engine failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let count = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("+PLANK_CONTROLS "))
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| value["commands"].as_array().map(Vec::len))
            .max()
            .unwrap_or(0);
        ensure!(
            count > 0 && String::from_utf8_lossy(&output.stdout).contains("engine controls"),
            "QA engine did not dispatch /command through its console catalog"
        );
        Ok(count)
    }
}

fn engine_args(workspace: &Path) -> Vec<String> {
    vec![
        "--fake-gpu".into(),
        "--minimal-prompt".into(),
        "--model".into(),
        workspace.join("not-loaded.gguf").to_string_lossy().into(),
    ]
}

async fn run_ui(fixture: &Fixture) -> Result<()> {
    let binaries = std::env::current_exe()?
        .parent()
        .context("QA executable has no parent")?
        .to_path_buf();
    let app = binaries.join(format!("nplank{}", std::env::consts::EXE_SUFFIX));
    let daemon = binaries.join(format!("nplankd{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        app.is_file() && daemon.is_file(),
        "Build nplank and nplankd before launching QA"
    );
    // A distinct bundle lets accessibility tools select this disposable process
    // without launching the user's installed NPlank application by name.
    #[cfg(target_os = "macos")]
    let app = {
        let contents = fixture.root.join("NPlank QA.app/Contents");
        let executable = contents.join("MacOS/nplank-qa");
        std::fs::create_dir_all(contents.join("MacOS"))?;
        std::fs::copy(&app, &executable)?;
        std::fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>nplank-qa</string>
<key>CFBundleIdentifier</key><string>org.spinorama.nplank.qa</string>
<key>CFBundleName</key><string>NPlank QA</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>"#,
        )?;
        executable
    };
    let address = TcpListener::bind("127.0.0.1:0")
        .context("QA needs a loopback listener; run just qa-ui outside the execution sandbox")?
        .local_addr()?;
    let token = nplank::daemon::token_in(&fixture.state)?;
    let client = Client::new(&format!("http://{address}"), token.clone())?;
    let log = std::fs::File::create(fixture.root.join("daemon.log"))?;
    let mut daemon = tokio::process::Command::new(daemon)
        .arg("--data-dir")
        .arg(&fixture.state)
        .arg("--listen")
        .arg(address.to_string())
        .env("HOME", &fixture.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(status) = daemon.try_wait()? {
                anyhow::bail!("QA daemon exited: {status}; inspect daemon.log");
            }
            if let Ok(snapshot) = client.snapshot(None, 0).await {
                ensure!(
                    snapshot
                        .sessions
                        .iter()
                        .any(|session| session.id == fixture.session),
                    "QA daemon fixture identity mismatch"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("QA daemon startup timed out; inspect daemon.log")??;
    let mut app = tokio::process::Command::new(app)
        .arg("--connect")
        .arg(format!("http://{address}"))
        .arg("--token-env")
        .arg("NPLANK_QA_TOKEN")
        .env("NPLANK_QA_TOKEN", token)
        .arg("--data-dir")
        .arg(&fixture.state)
        .env("HOME", &fixture.home)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let result = tokio::select! {
        status = app.wait() => status.context("QA app wait failed").and_then(|status| {
            ensure!(status.success(), "QA app exited: {status}");
            Ok(())
        }),
        signal = tokio::signal::ctrl_c() => signal.context("QA interrupt handler failed"),
    };
    // Retire only this launcher's sessions before stopping its owned daemon.
    if let Ok(snapshot) = client.snapshot(None, 0).await {
        for session in snapshot.sessions {
            let _ = client
                .execute(&Command::Reset {
                    session_id: session.id,
                })
                .await;
        }
    }
    if app.try_wait()?.is_none() {
        app.kill().await?;
    }
    if daemon.try_wait()?.is_none() {
        daemon.kill().await?;
    }
    result
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let engine = args.plank.unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plank/target/debug")
            .join(format!("plank{}", std::env::consts::EXE_SUFFIX))
    });
    ensure!(
        engine.is_file(),
        "Build the bundled Plank binary or pass --plank <executable>"
    );
    let engine = std::fs::canonicalize(engine)?;
    let directory = tempfile::Builder::new().prefix("nplank-ui-qa-").tempdir()?;
    let fixture = Fixture::prepare(directory.path(), &engine)?;
    if args.questions {
        let store = Store::open(&fixture.state.join("state.sqlite"))?;
        let sample = store
            .snapshot(None, 0)?
            .sessions
            .into_iter()
            .find(|session| matches!(session.backend, Backend::Demo))
            .context("Question fixture missing")?;
        std::fs::write(
            fixture.state.join("preferences.json"),
            serde_json::to_vec(&serde_json::json!({"theme":0,"design":0,"selected":sample.id}))?,
        )?;
    }
    println!("QA files: {}", fixture.root.display());
    let result = async {
        let count = fixture.probe_engine().await?;
        println!("Plank /command verified: {count} controls; no model weights loaded.");
        if args.prepare_only {
            Ok(())
        } else {
            run_ui(&fixture).await
        }
    }
    .await;
    if args.prepare_only || args.keep_data {
        println!("Retained QA files: {}", directory.keep().display());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::Fixture;
    use nplank::{model::Backend, store::Store};

    #[test]
    fn qa_fixture_keeps_state_home_and_workspace_inside_its_disposable_root() -> anyhow::Result<()>
    {
        let root = tempfile::tempdir()?;
        let engine = root.path().join("plank-fixture");
        std::fs::write(&engine, b"fixture")?;
        let fixture = Fixture::prepare(root.path(), &engine)?;
        for path in [&fixture.home, &fixture.state, &fixture.workspace] {
            assert!(path.starts_with(root.path()));
        }
        let document = std::fs::read_to_string(fixture.workspace.join("large-editor.rs"))?;
        assert!(document.len() > nplank::documenttransfer::CHUNK_BYTES);
        assert!(document.len() <= 256 * 1024);
        let store = Store::open(&fixture.state.join("state.sqlite"))?;
        let snapshot = store.snapshot(None, 0)?;
        assert_eq!(snapshot.sessions.len(), 2);
        assert!(
            matches!(&store.session(&fixture.session)?.backend, Backend::Local { executable, args }
            if executable == &engine.to_string_lossy() && args.iter().any(|arg| arg == "--fake-gpu"))
        );
        let sample = snapshot
            .sessions
            .iter()
            .find(|session| matches!(session.backend, Backend::Demo))
            .unwrap();
        assert!(
            store
                .snapshot(Some(&sample.id), 0)?
                .entries
                .iter()
                .any(|entry| entry.is_context_boundary())
        );
        Ok(())
    }
}
