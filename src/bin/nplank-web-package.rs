//! Packages the compiled Rust browser client with wasm-bindgen bootstrap glue.

// Rust guideline compliant 2026-02-21
use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};

fn main() -> Result<()> {
    let output = PathBuf::from("target/web");
    std::fs::create_dir_all(&output)?;
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", "nplank", "--out-dir"])
        .arg(&output)
        .arg("target/wasm32-unknown-unknown/release/nplank.wasm")
        .status()
        .context("Install wasm-bindgen-cli 0.2.127 before packaging")?;
    ensure!(
        status.success(),
        "wasm-bindgen packaging failed; use CLI version 0.2.127"
    );
    // Only toolchain bootstrap glue is JavaScript; views and behavior are Rust.
    std::fs::write(
        output.join("index.html"),
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>nplank</title>
<style>html,body{margin:0;width:100%;height:100%;overflow:hidden}#boot{padding:24px;font-family:system-ui}</style></head>
<body><div id="boot">Starting nplank…</div><script type="module">
import init from './nplank.js';
try {
  if (!globalThis.crossOriginIsolated) throw new Error('Shared-memory isolation is missing. Serve this page through nplankd --web-dir.');
  if (!navigator.gpu) throw new Error('This browser needs WebGPU support.');
  await init();
  document.getElementById('boot')?.remove();
} catch (error) {
  document.getElementById('boot').textContent = 'nplank could not start: ' + error.message;
  console.error(error);
}
</script></body></html>"#,
    )?;
    println!("Browser assets: {}", output.display());
    Ok(())
}
