//! Serves trusted browser build output with shared-memory isolation headers.

// Rust guideline compliant 2026-02-21
use std::path::{Component, Path, PathBuf};

use axum::{
    Extension,
    body::Body,
    extract::Request,
    http::{HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) async fn isolate(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
        ("cross-origin-resource-policy", "same-origin"),
        ("x-content-type-options", "nosniff"),
        ("cache-control", "no-store"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

pub(crate) async fn asset(
    Extension(directory): Extension<PathBuf>,
    method: Method,
    uri: Uri,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let relative = match uri.path() {
        "/" => Path::new("index.html"),
        path => Path::new(path.trim_start_matches('/')),
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type = match relative
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    // Canonicalization also rejects symlinks that escape the asset directory.
    let Ok(root) = std::fs::canonicalize(directory) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(path) = std::fs::canonicalize(root.join(relative)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !path.starts_with(&root) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(content_type));
    response
}
