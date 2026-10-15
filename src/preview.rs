//! Prepares inert HTML previews isolated from app credentials and host resources.

use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

/// Bounds the extra copies needed for an escaped browser preview document.
pub(crate) const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Wraps verified HTML in an opaque-origin, script-free sandbox.
///
/// # Errors
/// Returns an error for invalid metadata, oversized HTML, or changed file bytes.
pub(crate) fn document(export: &crate::model::EngineExport, html: &str) -> Result<String> {
    ensure!(
        export.is_valid() && export.format == "html",
        "Invalid HTML preview"
    );
    ensure!(
        export.bytes <= MAX_BYTES,
        "Preview exceeds 32 MiB; download the file instead"
    );
    ensure!(html.len() as u64 == export.bytes, "Preview length changed");
    let checksum: String = Sha256::digest(html.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(checksum == export.sha256, "Preview checksum mismatch");
    let escaped = html
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    // No scripts, network, forms, popups, or same-origin access. Inline styles and
    // data images allow standalone reports without exposing local files or tokens.
    Ok(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; frame-src about:; base-uri 'none'; form-action 'none'"><title>Plank preview</title><style>html,body,iframe{{margin:0;width:100%;height:100%;border:0}}iframe{{position:fixed;inset:0}}</style></head><body><iframe sandbox="" referrerpolicy="no-referrer" title="Plank HTML preview" srcdoc="{escaped}"></iframe></body></html>"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_preview_preserves_markup_inside_an_inert_sandbox_and_rejects_corruption() {
        let html = "<h1>A & B</h1><script>window.parent.secret</script>\"🦀";
        let mut export = crate::model::EngineExport {
            purpose: crate::model::ExportPurpose::Preview,
            id: uuid::Uuid::new_v4().to_string(),
            name: "report.html".into(),
            format: "html".into(),
            bytes: html.len() as u64,
            sha256: Sha256::digest(html.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        let page = document(&export, html).unwrap();
        assert!(page.contains("sandbox=\"\""));
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("&lt;script&gt;"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("A &amp; B"));
        assert!(document(&export, "changed").is_err());
        export.sha256 = "0".repeat(64);
        assert!(document(&export, html).is_err());
        export.bytes = MAX_BYTES + 1;
        assert!(document(&export, html).is_err());
        export.format = "md".into();
        assert!(!export.is_valid());
    }
}
