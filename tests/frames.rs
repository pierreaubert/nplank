//! Graphical frame controls cross a real worker without entering model input.

#![cfg(feature = "test-engine")]

use anyhow::Result;
use nplank::{
    daemon::Daemon,
    frame::{Input, Request, Snapshot},
    model::{Backend, Command, RunState},
};
use std::time::Duration;

async fn wait_frame(
    daemon: &Daemon,
    id: &str,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Result<(u64, Snapshot)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(id), 0)?;
            if let Some(frame) = snapshot
                .telemetry
                .get(id)
                .and_then(|value| value.frame.clone())
                && predicate(&frame)
            {
                let generation = snapshot
                    .sessions
                    .iter()
                    .find(|value| value.id == id)
                    .unwrap()
                    .generation;
                return Ok((generation, frame));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn graphical_frames_route_input_fence_ownership_and_retire_after_close_or_crash() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Frames".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let session = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Frame".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec![],
            },
        })?
        .id;
    daemon.execute(Command::Prompt {
        session_id: session.clone(),
        request_id: "open-frame".into(),
        text: "/frame demo".into(),
    })?;
    let (generation, initial) = wait_frame(&daemon, &session, |_| true).await?;
    let command = |owner, frame: &str, input| Command::FrameControl {
        session_id: session.clone(),
        generation: owner,
        request: Request {
            id: frame.into(),
            input,
        },
    };
    assert!(
        daemon
            .execute(command(generation + 1, &initial.id, Input::Close))
            .is_err()
    );
    assert!(
        daemon
            .execute(command(generation, "old-frame", Input::Close))
            .is_err()
    );
    daemon.execute(command(
        generation,
        &initial.id,
        Input::Key {
            code: "a".into(),
            text: Some('A'),
        },
    ))?;
    wait_frame(&daemon, &session, |frame| frame.cells[0].ch == 'A').await?;
    assert!(
        daemon
            .execute(command(
                generation,
                &initial.id,
                Input::Text {
                    text: "invalid\ntext".into()
                }
            ))
            .is_err()
    );
    daemon.execute(command(
        generation,
        &initial.id,
        Input::Text {
            text: "日🦀".into(),
        },
    ))?;
    wait_frame(&daemon, &session, |frame| frame.cells[0].ch == '🦀').await?;
    daemon.execute(command(
        generation,
        &initial.id,
        Input::Mouse {
            kind: "down".into(),
            x: 2,
            y: 0,
        },
    ))?;
    wait_frame(&daemon, &session, |frame| frame.cells[0].ch == 'M').await?;
    daemon.execute(command(
        generation,
        &initial.id,
        Input::Resize { w: 8, h: 2 },
    ))?;
    wait_frame(&daemon, &session, |frame| (frame.w, frame.h) == (8, 2)).await?;
    daemon.execute(command(generation, &initial.id, Input::Close))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&session), 0)?;
            if snapshot
                .sessions
                .iter()
                .any(|row| row.id == session && row.state == RunState::Idle)
                && snapshot.telemetry[&session].frame.is_none()
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(
        daemon
            .execute(command(generation, &initial.id, Input::Close))
            .is_err()
    );
    daemon.execute(Command::Prompt {
        session_id: session.clone(),
        request_id: "reopen-frame".into(),
        text: "/frame demo".into(),
    })?;
    let (_, replacement) = wait_frame(&daemon, &session, |frame| frame.id != initial.id).await?;
    assert!(
        daemon
            .execute(command(generation, &initial.id, Input::Close))
            .is_err()
    );
    daemon.execute(command(
        generation,
        &replacement.id,
        Input::Key {
            code: "crash".into(),
            text: None,
        },
    ))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&session), 0)?;
            if snapshot
                .sessions
                .iter()
                .any(|row| row.id == session && row.state == RunState::Failed)
                && snapshot
                    .telemetry
                    .get(&session)
                    .is_none_or(|value| value.frame.is_none())
            {
                assert!(
                    !snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.text.contains("frame_input"))
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(
        daemon
            .execute(command(generation, &replacement.id, Input::Close))
            .is_err()
    );
    Ok(())
}
