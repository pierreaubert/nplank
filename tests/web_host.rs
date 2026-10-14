//! Browser asset delivery preserves API authentication and filesystem boundaries.

// Rust guideline compliant 2026-02-21
use anyhow::Result;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use nplank::daemon::{Daemon, router_with_web};
use tower::ServiceExt;

#[tokio::test]
async fn browser_assets_have_isolation_headers_and_api_still_requires_authentication() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let web = directory.path().join("web");
    std::fs::create_dir_all(web.join("snippets/worker"))?;
    std::fs::write(web.join("index.html"), "browser")?;
    std::fs::write(web.join("nplank_bg.wasm"), b"\0asm")?;
    std::fs::write(web.join("snippets/worker/worker.js"), "worker")?;
    std::fs::write(web.join("token"), "private")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let router = router_with_web(daemon, web);
    for (path, status, mime) in [
        ("/", StatusCode::OK, Some("text/html; charset=utf-8")),
        ("/nplank_bg.wasm", StatusCode::OK, Some("application/wasm")),
        (
            "/snippets/worker/worker.js",
            StatusCode::OK,
            Some("text/javascript; charset=utf-8"),
        ),
        ("/token", StatusCode::NOT_FOUND, None),
        ("/../state.sqlite", StatusCode::NOT_FOUND, None),
        ("/v1/snapshot", StatusCode::UNAUTHORIZED, None),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), status, "{path}");
        assert_eq!(
            response.headers()["cross-origin-opener-policy"],
            "same-origin"
        );
        assert_eq!(
            response.headers()["cross-origin-embedder-policy"],
            "require-corp"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        if let Some(mime) = mime {
            assert_eq!(response.headers()["content-type"], mime);
        }
    }
    let response = router
        .oneshot(
            Request::builder()
                .uri("/v1/snapshot")
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn browser_assets_cannot_follow_symlinks_outside_the_build_directory() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let web = directory.path().join("web");
    std::fs::create_dir(&web)?;
    let outside = directory.path().join("private.js");
    std::fs::write(&outside, "private")?;
    std::os::unix::fs::symlink(outside, web.join("escape.js"))?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let response = router_with_web(daemon, web)
        .oneshot(Request::builder().uri("/escape.js").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}
