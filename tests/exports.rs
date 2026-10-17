//! Export delivery crosses a real worker process and authenticated HTTP boundary.

#![cfg(feature = "test-engine")]

// Rust guideline compliant 2026-02-21
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use nplank::{
    daemon::{Daemon, router},
    model::{Backend, Command, RunState},
};
use tower::ServiceExt;

#[tokio::test]
async fn console_exports_arrive_as_verified_downloads_and_reset_revokes_them() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Exports".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let session = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Export session".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: Vec::new(),
            },
        })?
        .id;
    daemon.execute(Command::Prompt {
        session_id: session.clone(),
        request_id: "export-request".into(),
        text: "/export md".into(),
        images: Vec::new(),
    })?;
    let (generation, export) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&session), 0)?;
            let current = snapshot
                .sessions
                .iter()
                .find(|row| row.id == session)
                .unwrap();
            if current.state == RunState::Idle
                && let Some(export) = snapshot
                    .telemetry
                    .get(&session)
                    .and_then(|telemetry| telemetry.export.clone())
            {
                break Ok::<_, anyhow::Error>((current.generation, export));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(export.name, "transcript.md");
    let uri = format!(
        "/v1/export?session={session}&generation={generation}&id={}",
        export.id
    );
    let api = router(Arc::clone(&daemon));
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 32)
            .await?
            .as_ref(),
        b"hello"
    );
    let snapshot = daemon.snapshot(Some(&session), 0)?;
    assert!(!snapshot.entries.iter().any(|entry| entry.text == "hello"));
    daemon.execute(Command::Prompt {
        session_id: session.clone(),
        request_id: "preview-request".into(),
        text: "/open report.html".into(),
        images: Vec::new(),
    })?;
    let preview = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&session), 0)?;
            if snapshot
                .sessions
                .iter()
                .any(|row| row.id == session && row.state == RunState::Idle)
                && let Some(preview) = snapshot
                    .telemetry
                    .get(&session)
                    .and_then(|value| value.preview_export.clone())
            {
                assert_eq!(
                    snapshot.telemetry[&session].export.as_ref().unwrap(),
                    &export
                );
                break Ok::<_, anyhow::Error>(preview);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(preview.purpose, nplank::model::ExportPurpose::Preview);
    let preview_uri = format!(
        "/v1/export?session={session}&generation={generation}&id={}",
        preview.id
    );
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri(&preview_uri)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 32)
            .await?
            .as_ref(),
        b"hello"
    );
    daemon.execute(Command::Prompt {
        session_id: session.clone(),
        request_id: "second-export-request".into(),
        text: "/export md".into(),
        images: Vec::new(),
    })?;
    let newest = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&session), 0)?;
            if snapshot
                .sessions
                .iter()
                .any(|row| row.id == session && row.state == RunState::Idle)
                && let Some(telemetry) = snapshot.telemetry.get(&session)
                && let Some(newest) = &telemetry.export
                && newest.id != export.id
            {
                assert_eq!(telemetry.artifacts.len(), 3);
                assert_eq!(telemetry.artifacts[0], *newest);
                assert!(telemetry.artifacts.contains(&export));
                assert!(telemetry.artifacts.contains(&preview));
                break Ok::<_, anyhow::Error>(newest.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_ne!(newest.id, export.id);
    // The old identity remains downloadable after the same category is exported again.
    let old_response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(old_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(old_response.into_body(), 32)
            .await?
            .as_ref(),
        b"hello"
    );
    daemon.execute(Command::Reset {
        session_id: session.clone(),
    })?;
    assert!(
        daemon
            .snapshot(Some(&session), 0)?
            .telemetry
            .get(&session)
            .is_none_or(|telemetry| telemetry.artifacts.is_empty())
    );
    let response = api
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = api
        .oneshot(
            Request::builder()
                .uri(&preview_uri)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}
