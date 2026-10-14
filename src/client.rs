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

    /// Fetches metadata and the selected transcript page.
    ///
    /// # Errors
    /// Returns connection, authentication, or response errors.
    pub async fn snapshot(&self, session: Option<&str>, before: i64) -> Result<Snapshot> {
        let mut request = self
            .http
            .get(format!("{}/v1/snapshot", self.url))
            .timeout(Duration::from_secs(5))
            .bearer_auth(&self.token)
            .query(&[("before", before.to_string())]);
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
        let response = self
            .http
            .post(format!("{}/v1/command", self.url))
            .timeout(Duration::from_secs(5))
            .bearer_auth(&self.token)
            .json(command)
            .send()
            .await?;
        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await?;
            anyhow::bail!("{}", body["error"].as_str().unwrap_or("Command failed"));
        }
        Ok(response.json().await?)
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
    #[cfg(target_family = "wasm")]
    pub(crate) async fn browser_export(
        &self,
        session: &str,
        generation: u64,
        export: &crate::model::EngineExport,
    ) -> Result<String> {
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
        // A browser Blob avoids a complete-file copy into WASM linear memory.
        web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|_| anyhow::anyhow!("Cannot prepare browser file"))
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

#[cfg(test)]
mod tests {
    use super::Client;

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
            html_sha256: Some(export.sha256.clone()),
        };
        assert!(export.matches_insights(&current));
        let different = crate::model::EngineMetrics::Insights {
            markdown: "new report".into(),
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
