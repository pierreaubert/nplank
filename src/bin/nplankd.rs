//! Starts the persistent headless daemon.

// Rust guideline compliant 2026-02-21
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = nplank::daemon::DEFAULT_ADDRESS)]
    listen: SocketAddr,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Serve packaged browser assets on the same origin as the API.
    #[arg(long)]
    web_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let directory = args
        .data_dir
        .unwrap_or_else(nplank::daemon::default_data_dir);
    nplank::daemon::serve_with_web(&directory, args.listen, args.web_dir.as_deref()).await
}
