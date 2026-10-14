toolkit := env_var_or_default("GPUI_TOOLKIT_CLI", "../gpui-toolkit/target/debug/gpui-toolkit")
wasm_toolchain := env_var_or_default("NPLANK_WASM_TOOLCHAIN", "nightly")
linux_target := env_var_or_default("NPLANK_LINUX_TARGET", "x86_64-unknown-linux-gnu")
windows_target := env_var_or_default("NPLANK_WINDOWS_TARGET", "x86_64-pc-windows-msvc")

build:
    cargo build

# Build the bundled engine with console slash-command dispatch.
build-plank:
    cd plank && cargo build

# Model-free regression for the actual engine's console control path.
check-plank-controls:
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_controls_never_become_model_prompts
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib gui_
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib export_transfer
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --bin plank gui_console_menu_works_without_terminal_but_one_shots_do_not_prompt
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_command_output_tests
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_controls_never_enter_the_model_prompt_queue
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib frame_roundtrip_all_events
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib client_frame_roundtrip
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_questions
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_editors
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib ordinary_commands_and_prompts_are_remote_safe
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib metrics_snapshots
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib activity_snapshots
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib engine_inventory_pages
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib stats_report_renders
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib client_insights_report
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_insights_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib task_controls
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib templates::tests
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib download_wire
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib host_snapshot
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib observer_auth
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib identified_prompts
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib identified_commands
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_mirroring
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_remote_inputs
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_local_
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_confirmations
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_remote_telemetry
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_subagent
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib subagent_output
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib subagent_tool_notices
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib a_sub_agent_edit
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib fanout_roster_labels
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_background_cleanup
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib quiet_remote_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_shell_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_hook_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_mcp_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_web_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib job_reports_bound_output
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_startup_reply
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib ownership_fencing
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_input::tests
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_dialog_pipe
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib readonly_slash_commands_do_not_dirty_or_log

run: build
    cargo run --bin nplank

appearance: build
    cargo run --bin nplank -- --appearance

daemon:
    cargo run --no-default-features --bin nplankd

demo:
    cargo run --bin nplankctl -- demo --directory .

check:
    cargo fmt --check
    cargo clippy --all-targets --features test-engine -- -D warnings
    cargo test --features test-engine

# Uses an installed Muse CLI with its model-free, memory-only echo provider.
smoke-muse:
    cargo test --features test-engine --test agents installed_muse_echo_protocol -- --ignored

components:
    {{toolkit}} component Input Button Sidebar Markdown

themes:
    {{toolkit}} theme list

toolkit-check:
    {{toolkit}} doctor --dir .
    {{toolkit}} layout check 'V[g6] > (Hd"Appearance"[level=2] + Tx.sm"Colors and design rules are independently selectable."[muted])'
    {{toolkit}} layout check 'V[g4] > (Hd"Session preview"[level=3] + Tx"Research · Local Plank" + Tx.sm"Running · 32 tokens/s"[muted])'
    {{toolkit}} layout check 'H[g2] > (B.primary"Send"#send + B.destructive"Stop"#stop + B.ghost"Fresh context"#reset)'

bundle-macos: build
    cargo run --bin nplank-package

bundle-appearance-macos: build
    cargo run --bin nplank-package -- --appearance

# WASM uses nightly only; native recipes retain the default toolchain.
wasm-setup:
    rustup toolchain install {{wasm_toolchain}} --component rust-src --target wasm32-unknown-unknown
    cargo install wasm-bindgen-cli --version 0.2.127 --locked

wasm-check:
    RUSTUP_TOOLCHAIN={{wasm_toolchain}} CARGO_UNSTABLE_BUILD_STD=std,panic_abort cargo check --target wasm32-unknown-unknown --lib

wasm-build:
    RUSTUP_TOOLCHAIN={{wasm_toolchain}} CARGO_UNSTABLE_BUILD_STD=std,panic_abort cargo build --release --target wasm32-unknown-unknown --lib
    cargo run --no-default-features --bin nplank-web-package

# Opens at http://127.0.0.1:47831; stop an existing daemon before starting this one.
wasm-serve: wasm-build
    cargo run --no-default-features --bin nplankd -- --web-dir target/web

check-macos:
    cargo check --all-targets --target aarch64-apple-darwin

build-macos:
    cargo build --release --target aarch64-apple-darwin

# Run on the matching host, or configure its cross-compiler and native SDKs.
check-linux:
    cargo check --all-targets --target {{linux_target}}

build-linux:
    cargo build --release --target {{linux_target}}

check-windows:
    cargo check --all-targets --target {{windows_target}}

build-windows:
    cargo build --release --target {{windows_target}}

# Shared library checks/builds; mobile launch hosts and app packaging are pending.
check-ios:
    cargo check --target aarch64-apple-ios-sim --lib

build-ios:
    cargo build --release --target aarch64-apple-ios --lib

check-android:
    cargo ndk -t arm64-v8a check --lib

build-android:
    cargo ndk -t arm64-v8a build --release --lib
