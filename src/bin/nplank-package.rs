//! Creates a local macOS app bundle from the two compiled Rust executables.

// Rust guideline compliant 2026-02-21
use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
struct Args {
    /// Package a standalone appearance mini app that needs no engine daemon.
    #[arg(long)]
    appearance: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let name = if args.appearance {
        "nplank-appearance"
    } else {
        "nplank"
    };
    let executable = std::env::current_exe()?;
    let binaries = executable.parent().context("Executable has no parent")?;
    let bundle: PathBuf = binaries.join(format!("{name}.app/Contents"));
    std::fs::create_dir_all(bundle.join("MacOS"))?;
    for name in ["nplank", "nplankd"] {
        let destination = bundle.join("MacOS").join(name);
        let temporary = destination.with_extension("next");
        std::fs::copy(binaries.join(name), &temporary)
            .context("Run cargo build before bundling")?;
        std::fs::rename(temporary, destination)?;
    }
    std::fs::write(
        bundle.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>nplank</string>
<key>CFBundleIdentifier</key><string>org.spinorama.{name}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
{environment}
</dict></plist>"#,
            environment = if args.appearance {
                "<key>LSEnvironment</key><dict><key>NPLANK_APPEARANCE</key><string>1</string></dict>"
            } else {
                ""
            }
        ),
    )?;
    println!(
        "{}",
        bundle.parent().context("Bundle has no parent")?.display()
    );
    Ok(())
}
