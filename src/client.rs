//! Shared asynchronous client for local and SSH-tunneled daemon connections.

// Rust guideline compliant 2026-02-21
use crate::model::{Command, Receipt, Snapshot};
use anyhow::{Result, ensure};
use std::time::Duration;

/// An authenticated connection that never owns session execution.
#[derive(Clone)]
pub struct Client {
    url: String,
    token: String,
    http: reqwest::Client,
    #[cfg(not(target_family = "wasm"))]
    downloads: reqwest::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates an authenticated client with bounded network waits.
    ///
    /// # Errors
    /// Returns errors for invalid endpoints or HTTP initialization.
    pub fn new(url: &str, token: String) -> Result<Self> {
        let parsed = reqwest::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "https"
                || (parsed.scheme() == "http"
                    && matches!(
                        parsed.host_str(),
                        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                    )),
            "Use HTTPS or a loopback HTTP endpoint through SSH"
        );
        let builder = reqwest::Client::builder();
        #[cfg(not(target_family = "wasm"))]
        let builder = builder.timeout(Duration::from_secs(5));
        Ok(Self {
            url: url.trim_end_matches('/').into(),
            token,
            http: builder.build()?,
            #[cfg(not(target_family = "wasm"))]
            downloads: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    /// Builds the pairing payload scanned by the iOS app.
    ///
    /// The JSON shape `{"v":1,"url","token"}` is the shared desktop/iOS
    /// contract; the iOS scanner fills its connect fields from it.
    #[must_use]
    pub fn pairing_payload(&self) -> String {
        serde_json::json!({"v": 1, "url": self.url, "token": self.token}).to_string()
    }

    /// Parses a scanned pairing payload back into URL and token.
    ///
    /// Returns `None` for anything else so stray QR codes never fill
    /// credentials.
    #[must_use]
    pub fn parse_pair_payload(text: &str) -> Option<(String, String)> {
        let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
        if parsed.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return None;
        }
        let url = parsed
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let token = parsed
            .get("token")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if url.trim().is_empty() || token.trim().is_empty() {
            return None;
        }
        Some((url.to_string(), token.to_string()))
    }

    /// Fetches metadata and the selected transcript page.
    ///
    /// # Errors
    /// Returns connection, authentication, or response errors.
    pub async fn snapshot(&self, session: Option<&str>, before: i64) -> Result<Snapshot> {
        self.snapshot_pages(session, before, 0).await
    }

    /// Fetches independently paged main and side-answer histories.
    ///
    /// # Errors
    /// Returns connection, authentication, or response errors.
    pub async fn snapshot_pages(
        &self,
        session: Option<&str>,
        before: i64,
        aside_before: i64,
    ) -> Result<Snapshot> {
        let mut request = self
            .http
            .get(format!("{}/v1/snapshot", self.url))
            .timeout(Duration::from_secs(5))
            .bearer_auth(&self.token)
            .query(&[
                ("before", before.to_string()),
                ("aside_before", aside_before.to_string()),
            ]);
        if let Some(id) = session {
            request = request.query(&[("session", id)]);
        }
        let snapshot: Snapshot = request.send().await?.error_for_status()?.json().await?;
        ensure!(
            snapshot.protocol_version == crate::model::PROTOCOL_VERSION,
            "Daemon version is incompatible; finish active runs, then restart the daemon with the updated binary"
        );
        Ok(snapshot)
    }

    /// Submits a command and returns its durable receipt.
    ///
    /// # Errors
    /// Returns network errors or the daemon's validation message.
    pub async fn execute(&self, command: &Command) -> Result<Receipt> {
        let response = self.command_request(command)?.send().await?;
        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await?;
            anyhow::bail!("{}", body["error"].as_str().unwrap_or("Command failed"));
        }
        Ok(response.json().await?)
    }

    /// Fetches bounded chunks and validates an original document before publishing it.
    ///
    /// # Errors
    /// Returns authentication, stale revision, transfer, checksum, or UTF-8 errors.
    pub async fn load_document(
        &self,
        session: &str,
        editor: &crate::model::PendingApproval,
    ) -> Result<String> {
        let document = editor
            .document
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Document metadata is unavailable"))?;
        ensure!(
            document.bytes <= crate::documenttransfer::DOCUMENT_BYTES
                && document.sha256.len() == 64,
            "Invalid document metadata"
        );
        let requirement = serde_json::to_string(&editor.requirement)?;
        ensure!(
            session.len() <= 256 && editor.id.len() <= 256 && requirement.len() <= 8192,
            "Invalid editor metadata"
        );
        collect_document(document, |offset| {
            let request = self
                .http
                .get(format!("{}/v1/editor", self.url))
                .timeout(Duration::from_secs(60))
                .bearer_auth(&self.token)
                .query(&[
                    ("session", session),
                    ("document", editor.id.as_str()),
                    ("requirement", requirement.as_str()),
                    ("offset", &offset.to_string()),
                ]);
            let expected = (document.bytes - offset).min(crate::model::EDITOR_FETCH_BYTES);
            async move {
                let response = request.send().await?.error_for_status()?;
                ensure!(
                    response.content_length() == Some(expected as u64),
                    "Invalid document chunk length"
                );
                let bytes = response.bytes().await?;
                ensure!(bytes.len() == expected, "Document chunk was truncated");
                Ok(bytes.to_vec())
            }
        })
        .await
    }

    fn command_request(&self, command: &Command) -> Result<reqwest::RequestBuilder> {
        if let Command::EditDocument {
            session_id,
            approval_id,
            requirement,
            text: Some(text),
        } = command
        {
            ensure!(
                text.len() <= crate::documenttransfer::DOCUMENT_BYTES,
                "Document exceeds the editing limit"
            );
            let requirement = serde_json::to_string(requirement)?;
            let bytes = text.len().to_string();
            let sha256 = crate::documenttransfer::digest(text.as_bytes());
            return Ok(self
                .http
                .post(format!("{}/v1/editor", self.url))
                // Bulk uploads can cross slower remote links independently of polling.
                .timeout(Duration::from_secs(60))
                .bearer_auth(&self.token)
                .query(&[
                    ("session", session_id.as_str()),
                    ("document", approval_id.as_str()),
                    ("requirement", requirement.as_str()),
                    ("bytes", bytes.as_str()),
                    ("sha256", sha256.as_str()),
                ])
                .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(text.clone()));
        }
        Ok(self
            .http
            .post(format!("{}/v1/command", self.url))
            .timeout(Duration::from_secs(5))
            .bearer_auth(&self.token)
            .json(command))
    }

    /// Saves a complete export atomically, checking length and checksum first.
    ///
    /// # Errors
    /// Returns metadata, authentication, transfer, checksum or filesystem errors.
    #[cfg(not(target_family = "wasm"))]
    pub async fn save_export(
        &self,
        session: &str,
        generation: u64,
        export: &crate::model::EngineExport,
        path: &std::path::Path,
    ) -> Result<()> {
        ensure!(export.is_valid(), "Invalid export metadata");
        let response = self
            .downloads
            .get(format!("{}/v1/export", self.url))
            .bearer_auth(&self.token)
            .query(&[
                ("session", session),
                ("generation", &generation.to_string()),
                ("id", &export.id),
            ])
            .send()
            .await?
            .error_for_status()?;
        save_download(response, export, path).await
    }

    /// Fetches a complete export as a browser object URL without copying into WASM.
    ///
    /// # Errors
    /// Returns metadata, network, HTTP or browser file preparation errors.
    #[cfg(all(target_family = "wasm", feature = "gui"))]
    pub(crate) async fn browser_export(
        &self,
        session: &str,
        generation: u64,
        export: &crate::model::EngineExport,
    ) -> Result<String> {
        let blob = self
            .browser_export_blob(session, generation, export)
            .await?;
        web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|_| anyhow::anyhow!("Cannot prepare browser file"))
    }

    /// Prepares a bounded sandboxed preview after explicit user selection.
    ///
    /// # Errors
    /// Returns metadata, network, integrity, or browser preparation errors.
    #[cfg(all(target_family = "wasm", feature = "gui"))]
    pub(crate) async fn browser_preview(
        &self,
        session: &str,
        generation: u64,
        export: &crate::model::EngineExport,
    ) -> Result<String> {
        use wasm_bindgen_futures::JsFuture;
        ensure!(
            export.format == "html" && export.bytes <= crate::preview::MAX_BYTES,
            "Preview requires HTML of at most 32 MiB; download the file instead"
        );
        let blob = self
            .browser_export_blob(session, generation, export)
            .await?;
        let html = JsFuture::from(blob.text())
            .await
            .map_err(|_| anyhow::anyhow!("Cannot read preview"))?
            .as_string()
            .ok_or_else(|| anyhow::anyhow!("Invalid preview text"))?;
        let page = crate::preview::document(export, &html)?;
        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(&page));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("text/html;charset=utf-8");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)
            .map_err(|_| anyhow::anyhow!("Cannot prepare HTML preview"))?;
        web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|_| anyhow::anyhow!("Cannot prepare preview URL"))
    }

    #[cfg(all(target_family = "wasm", feature = "gui"))]
    async fn browser_export_blob(
        &self,
        session: &str,
        generation: u64,
        export: &crate::model::EngineExport,
    ) -> Result<web_sys::Blob> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        ensure!(export.is_valid(), "Invalid export metadata");
        // Blob.size uses JavaScript numbers; larger byte counts cannot be checked exactly.
        ensure!(
            export.bytes <= 9_007_199_254_740_991,
            "Export exceeds browser file-size limits"
        );
        let mut url = reqwest::Url::parse(&format!("{}/v1/export", self.url))?;
        url.query_pairs_mut()
            .append_pair("session", session)
            .append_pair("generation", &generation.to_string())
            .append_pair("id", &export.id);
        let request = web_sys::Request::new_with_str(url.as_str())
            .map_err(|_| anyhow::anyhow!("Cannot prepare download"))?;
        request
            .headers()
            .set("authorization", &format!("Bearer {}", self.token))
            .map_err(|_| anyhow::anyhow!("Cannot authorize download"))?;
        let window =
            web_sys::window().ok_or_else(|| anyhow::anyhow!("Browser window unavailable"))?;
        let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|_| anyhow::anyhow!("Export download failed"))?
            .dyn_into()
            .map_err(|_| anyhow::anyhow!("Invalid download response"))?;
        ensure!(
            response.ok(),
            "Export is unavailable (HTTP {})",
            response.status()
        );
        let blob: web_sys::Blob = JsFuture::from(
            response
                .blob()
                .map_err(|_| anyhow::anyhow!("Cannot read export"))?,
        )
        .await
        .map_err(|_| anyhow::anyhow!("Export download failed"))?
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("Invalid export file"))?;
        ensure!(blob.size() == export.bytes as f64, "Export length changed");
        Ok(blob)
    }
}

#[cfg(not(target_family = "wasm"))]
async fn save_download(
    response: reqwest::Response,
    export: &crate::model::EngineExport,
    path: &std::path::Path,
) -> Result<()> {
    ensure!(export.is_valid(), "Invalid export metadata");
    let mut response = response.error_for_status()?;
    if let Some(length) = response.headers().get("content-length") {
        let declared: u64 = length.to_str()?.parse()?;
        ensure!(declared == export.bytes, "Export length changed");
    }
    if let Some(bytes) = response.content_length() {
        ensure!(bytes == export.bytes, "Export length changed");
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use sha2::{Digest, Sha256};
    use std::io::Write as _;
    let mut hash = Sha256::new();
    let mut received = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("Export length overflow"))?;
        ensure!(
            received <= export.bytes,
            "Export exceeds its expected length"
        );
        temporary.write_all(&chunk)?;
        hash.update(&chunk);
    }
    ensure!(received == export.bytes, "Export was truncated");
    let digest = hash.finalize();
    let mut checksum = String::new();
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(checksum, "{byte:02x}");
    }
    ensure!(checksum == export.sha256, "Export checksum mismatch");
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

async fn collect_document<F, Fut>(
    document: &crate::model::EditorDocument,
    mut fetch: F,
) -> Result<String>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>>>,
{
    ensure!(
        document.bytes <= crate::documenttransfer::DOCUMENT_BYTES && document.sha256.len() == 64,
        "Invalid document metadata"
    );
    let mut text = Vec::with_capacity(document.bytes.min(crate::model::EDITOR_FETCH_BYTES));
    for offset in (0..document.bytes.max(1)).step_by(crate::model::EDITOR_FETCH_BYTES) {
        let bytes = fetch(offset).await?;
        ensure!(
            bytes.len() == (document.bytes - offset).min(crate::model::EDITOR_FETCH_BYTES),
            "Invalid document chunk length"
        );
        text.extend_from_slice(&bytes);
    }
    ensure!(
        crate::documenttransfer::digest(&text) == document.sha256,
        "Original document checksum mismatch"
    );
    Ok(String::from_utf8(text)?)
}

#[cfg(test)]
mod tests {
    use super::Client;

    #[test]
    fn pairing_payload_round_trips_and_rejects_stray_codes() {
        let client = Client::new("http://127.0.0.1:47831", "secret".into())
            .expect("loopback client must build");
        let payload = client.pairing_payload();
        assert_eq!(
            Client::parse_pair_payload(&payload),
            Some(("http://127.0.0.1:47831".into(), "secret".into()))
        );
        assert_eq!(Client::parse_pair_payload("hello"), None);
        assert_eq!(
            Client::parse_pair_payload(r#"{"v":2,"url":"u","token":"t"}"#),
            None
        );
        assert_eq!(
            Client::parse_pair_payload(r#"{"v":1,"url":"","token":"t"}"#),
            None
        );
        assert_eq!(
            Client::parse_pair_payload(r#"{"v":1,"url":"u","token":""}"#),
            None
        );
    }

    #[tokio::test]
    async fn original_document_collection_validates_chunks_before_publishing_utf8()
    -> anyhow::Result<()> {
        let text = format!(
            "{}🦀 café",
            "x".repeat(crate::model::EDITOR_FETCH_BYTES - 1)
        );
        let document = crate::model::EditorDocument {
            bytes: text.len(),
            sha256: crate::documenttransfer::digest(text.as_bytes()),
        };
        for mode in 0..5 {
            let result = super::collect_document(&document, |offset| {
                let mut bytes = text.as_bytes()
                    [offset..text.len().min(offset + crate::model::EDITOR_FETCH_BYTES)]
                    .to_vec();
                if mode == 1 {
                    bytes[0] ^= 1;
                }
                if mode == 2 {
                    bytes.pop();
                }
                if mode == 3 {
                    bytes.push(b'x');
                }
                async move {
                    anyhow::ensure!(mode != 4 || offset == 0, "Editor closed during transfer");
                    Ok(bytes)
                }
            })
            .await;
            if mode == 0 {
                assert!(
                    result? == text,
                    "UTF-8 split across byte chunks must reassemble exactly"
                );
            } else {
                assert!(result.is_err());
            }
        }
        let empty = crate::model::EditorDocument {
            bytes: 0,
            sha256: crate::documenttransfer::digest(b""),
        };
        assert_eq!(
            super::collect_document(&empty, |_| async { Ok(Vec::new()) }).await?,
            ""
        );
        let invalid_utf8 = crate::model::EditorDocument {
            bytes: 1,
            sha256: crate::documenttransfer::digest(&[0xff]),
        };
        assert!(
            super::collect_document(&invalid_utf8, |_| async { Ok(vec![0xff]) })
                .await
                .is_err()
        );
        let oversized = crate::model::EditorDocument {
            bytes: crate::documenttransfer::DOCUMENT_BYTES + 1,
            sha256: "0".repeat(64),
        };
        assert!(
            super::collect_document(&oversized, |_| async {
                panic!("Invalid metadata must not initiate a fetch")
            })
            .await
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn editor_requests_keep_document_text_out_of_json_and_preserve_exact_requirements()
    -> anyhow::Result<()> {
        let client = Client::new("http://127.0.0.1:47831", "fixture-token".into())?;
        let text = "\u{0}".repeat(1024 * 1024);
        let requirement =
            serde_json::json!({"kind":"plank_editor","generation":4,"revision":"rev-1"});
        let command = crate::model::Command::EditDocument {
            session_id: "session-1".into(),
            approval_id: "editor-1".into(),
            requirement: requirement.clone(),
            text: Some(text.clone()),
        };
        let request = client.command_request(&command)?.build()?;
        assert_eq!(request.url().path(), "/v1/editor");
        assert_eq!(
            request.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        assert!(
            request
                .body()
                .and_then(|body| body.as_bytes())
                .is_some_and(|body| body == text.as_bytes())
        );
        let encoded = request
            .url()
            .query_pairs()
            .find(|(key, _)| key == "requirement")
            .unwrap()
            .1;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded)?,
            requirement
        );
        let query: std::collections::HashMap<_, _> = request.url().query_pairs().collect();
        assert_eq!(query["session"], "session-1");
        assert_eq!(query["document"], "editor-1");
        assert_eq!(query["bytes"], text.len().to_string());
        assert_eq!(
            query["sha256"],
            crate::documenttransfer::digest(text.as_bytes())
        );
        let cancel = crate::model::Command::EditDocument {
            session_id: "session-1".into(),
            approval_id: "editor-1".into(),
            requirement,
            text: None,
        };
        assert_eq!(
            client.command_request(&cancel)?.build()?.url().path(),
            "/v1/command"
        );
        Ok(())
    }

    #[tokio::test]
    async fn export_download_replaces_only_complete_verified_files_and_removes_partials()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("conversation.md");
        std::fs::write(&path, "previous file")?;
        let export = crate::model::EngineExport {
            purpose: crate::model::ExportPurpose::Conversation,
            id: uuid::Uuid::new_v4().to_string(),
            name: "conversation.md".into(),
            format: "md".into(),
            bytes: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into(),
        };
        for (body, declared, status) in [
            ("jello", 5, 200),
            ("he", 5, 200),
            ("hello!", 5, 200),
            ("hello", 4, 200),
            ("hello", 5, 404),
        ] {
            let response: reqwest::Response = axum::http::Response::builder()
                .status(status)
                .header("content-length", declared)
                .body(body.to_owned())?
                .into();
            assert!(
                super::save_download(response, &export, &path)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path)?, "previous file");
            assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
        }
        let response: reqwest::Response = axum::http::Response::builder()
            .header("content-length", 5)
            .body("hello".to_owned())?
            .into();
        super::save_download(response, &export, &path).await?;
        assert_eq!(std::fs::read_to_string(&path)?, "hello");
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn export_metadata_rejects_paths_and_suggests_portable_download_names() {
        let mut export = crate::model::EngineExport {
            purpose: crate::model::ExportPurpose::Conversation,
            id: uuid::Uuid::new_v4().to_string(),
            name: "quoted\"name 🦀.html".into(),
            format: "html".into(),
            bytes: 0,
            sha256: "0".repeat(64),
        };
        assert!(export.is_valid());
        assert_eq!(export.suggested_name(), "quoted_name 🦀.html");
        export.purpose = crate::model::ExportPurpose::Insights;
        let current = crate::model::EngineMetrics::Insights {
            markdown: "report".into(),
            charts: Vec::new(),
            html_sha256: Some(export.sha256.clone()),
        };
        assert!(export.matches_insights(&current));
        let different = crate::model::EngineMetrics::Insights {
            markdown: "new report".into(),
            charts: Vec::new(),
            html_sha256: Some("1".repeat(64)),
        };
        assert!(!export.matches_insights(&different));
        export.purpose = crate::model::ExportPurpose::Conversation;
        assert!(!export.matches_insights(&current));
        export.name = "NUL.txt".into();
        assert_eq!(export.suggested_name(), "session.html");
        export.name = "../secret".into();
        assert!(!export.is_valid());
        export.name = "valid.html".into();
        export.sha256 = "G".repeat(64);
        assert!(!export.is_valid());
    }

    #[tokio::test]
    async fn client_debug_does_not_disclose_bearer_tokens() -> anyhow::Result<()> {
        let token = "private-test-token";
        let client = Client::new("http://127.0.0.1:47831", token.into())?;
        let debug = format!("{client:?}");
        assert!(!debug.contains(token));
        assert!(debug.contains("127.0.0.1"));
        Ok(())
    }
}
