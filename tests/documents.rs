//! Original documents stay outside snapshots and require their current revision.

// Rust guideline compliant 2026-02-21
use anyhow::Result;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nplank::{
    daemon::{Daemon, router},
    model::{Backend, EDITOR_FETCH_BYTES, PendingApproval, Snapshot},
    store::Store,
};
use std::sync::Arc;
use tower::ServiceExt;

fn editor(text: String) -> PendingApproval {
    PendingApproval {
        id: "editor-1".into(),
        tool: "File".into(),
        arguments: text,
        requirement: serde_json::json!({"kind":"plank_editor","generation":0,"revision":"revision-1"}),
        document: None,
        choices: Vec::new(),
    }
}

fn read_uri(session: &str, editor: &PendingApproval, offset: usize) -> Result<String> {
    let mut url = reqwest::Url::parse("http://localhost/v1/editor")?;
    url.query_pairs_mut()
        .append_pair("session", session)
        .append_pair("document", &editor.id)
        .append_pair("requirement", &serde_json::to_string(&editor.requirement)?)
        .append_pair("offset", &offset.to_string());
    Ok(format!("{}?{}", url.path(), url.query().unwrap()))
}

#[tokio::test]
async fn document_snapshots_are_compact_and_chunks_are_authenticated_scoped_and_retired()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    // A multibyte scalar crosses the first byte-chunk boundary.
    let text = format!(
        "{}🦀{}",
        "x".repeat(EDITOR_FETCH_BYTES - 1),
        "\u{0}café\r\n".repeat(100_000)
    );
    let (id, pending) = {
        let store = Store::open(&directory.path().join("state.sqlite"))?;
        let workspace = store.create_workspace("Documents", directory.path().to_str().unwrap())?;
        let id = store.create_session(&workspace, "Editor", Backend::Demo)?;
        store.approval(&id, 0, editor(text.clone()))?;
        let pending = store.session(&id)?.approvals.remove(0);
        (id, pending)
    };
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    assert!(pending.arguments.is_empty());
    assert_eq!(pending.document.as_ref().unwrap().bytes, text.len());
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(
        serde_json::to_vec(&snapshot)?.len() < 16 * 1024,
        "Polling metadata must not include document bodies"
    );
    let mut received = Vec::new();
    for offset in (0..text.len()).step_by(EDITOR_FETCH_BYTES) {
        let response = router(Arc::clone(&daemon))
            .oneshot(
                Request::builder()
                    .uri(read_uri(&id, &pending, offset)?)
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let declared: usize = response.headers()["content-length"].to_str()?.parse()?;
        let bytes = to_bytes(response.into_body(), EDITOR_FETCH_BYTES).await?;
        assert_eq!(bytes.len(), declared);
        received.extend_from_slice(&bytes);
    }
    assert!(
        received == text.as_bytes(),
        "Chunks must preserve every byte across UTF-8 boundaries"
    );
    assert_eq!(
        nplank::documenttransfer::digest(&received),
        pending.document.as_ref().unwrap().sha256
    );
    for (token, stale, offset, status) in [
        ("wrong", false, 0, StatusCode::UNAUTHORIZED),
        ("secret", true, 0, StatusCode::CONFLICT),
        ("secret", false, 1, StatusCode::CONFLICT),
        ("secret", false, usize::MAX, StatusCode::CONFLICT),
    ] {
        let mut request = pending.clone();
        if stale {
            request.requirement["revision"] = "old".into();
        }
        let response = router(Arc::clone(&daemon))
            .oneshot(
                Request::builder()
                    .uri(read_uri(&id, &request, offset)?)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), status);
    }
    Store::open(&directory.path().join("state.sqlite"))?.resolve_approval(&id, 0, &pending.id)?;
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .uri(read_uri(&id, &pending, 0)?)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let connection = rusqlite::Connection::open(directory.path().join("state.sqlite"))?;
    let retained: i64 =
        connection.query_row("SELECT count(*) FROM editor_document_chunks", [], |row| {
            row.get(0)
        })?;
    assert_eq!(retained, 0);
    connection.execute(
        "INSERT INTO editor_documents VALUES('missing-session','orphan','{}')",
        [],
    )?;
    connection.execute(
        "INSERT INTO editor_document_chunks VALUES('missing-session','orphan',0,?1)",
        [b"retired".as_slice()],
    )?;
    let _reopened = Store::open(&directory.path().join("state.sqlite"))?;
    let retained: i64 =
        connection.query_row("SELECT count(*) FROM editor_document_chunks", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        retained, 0,
        "Startup must collect originals orphaned by a crash"
    );
    Ok(())
}

#[test]
fn inline_editor_migration_is_atomic_and_preserves_the_revision_and_original_bytes() -> Result<()> {
    for fail in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let store = Store::open(&path)?;
        let workspace = store.create_workspace("Migration", directory.path().to_str().unwrap())?;
        let id = store.create_session(&workspace, "Legacy", Backend::Demo)?;
        let mut session = store.session(&id)?;
        session.approvals.push(editor("old café\r\n🦀".into()));
        drop(store);
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute(
            "UPDATE sessions SET data=?2 WHERE id=?1",
            rusqlite::params![id, serde_json::to_string(&session)?],
        )?;
        connection.pragma_update(None, "user_version", 2)?;
        if fail {
            connection.execute_batch("CREATE TRIGGER fail_document_migration BEFORE INSERT ON editor_document_chunks BEGIN SELECT RAISE(ABORT, 'fixture'); END;")?;
            assert!(Store::open(&path).is_err());
            let saved: String =
                connection.query_row("SELECT data FROM sessions WHERE id=?1", [&id], |row| {
                    row.get(0)
                })?;
            let unchanged: nplank::model::Session = serde_json::from_str(&saved)?;
            assert_eq!(unchanged.approvals[0].arguments, "old café\r\n🦀");
            assert!(unchanged.approvals[0].document.is_none());
            let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            assert_eq!(version, 2);
            connection.execute_batch("DROP TRIGGER fail_document_migration;")?;
        }
        let migrated = Store::open(&path)?;
        let current = migrated.session(&id)?.approvals.remove(0);
        assert!(current.arguments.is_empty());
        assert_eq!(current.requirement, session.approvals[0].requirement);
        assert_eq!(
            migrated.document_chunk(&id, &current.id, &current.requirement, 0)?,
            "old café\r\n🦀".as_bytes()
        );
        let snapshot: Snapshot = migrated.snapshot(Some(&id), 0)?;
        assert!(snapshot.sessions[0].approvals[0].arguments.is_empty());
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, 5);
    }
    Ok(())
}
