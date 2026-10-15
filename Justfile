toolkit := env_var_or_default("GPUI_TOOLKIT_CLI", "../gpui-toolkit/target/debug/gpui-toolkit")
wasm_toolchain := env_var_or_default("NPLANK_WASM_TOOLCHAIN", "nightly")
linux_target := env_var_or_default("NPLANK_LINUX_TARGET", "x86_64-unknown-linux-gnu")
windows_target := env_var_or_default("NPLANK_WINDOWS_TARGET", "x86_64-pc-windows-msvc")

_default:
	just --list

[group('build')]
build:
    cargo build

# Build the bundled engine with console slash-command dispatch.
[group('build')]
build-plank:
    cd plank && cargo build

# Graphical engine with WASM frames, built-in editing and Rust Gemma inference.
[group('build')]
build-plank-gui:
    cd plank && cargo build --no-default-features --features builtin_editor,plugins,gemma

# Real native UI and daemon with disposable state and a model-free Plank engine.
[group('qa')]
qa-ui:
    cd plank && PLANK_NO_DS4=1 cargo build --no-default-features --features builtin_editor,plugins,gemma --bin plank
    cargo build --features test-engine --bin nplank --bin nplankd --bin nplank-ui-qa
    cargo run --features test-engine --bin nplank-ui-qa

# Model-free regression for the actual engine's console control path.
[group('check')]
check-plank-controls:
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_controls_never_become_model_prompts
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib gui_
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib history_rewind
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib branch::tests
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_startup_downloads
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_completion_notifications
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote::control::tests::notify_
    cargo test --lib notification
    cargo test --lib transcript_
    cargo test --lib command_picker_aliases
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib live_controls
    cargo test --features test-engine --lib --test recovery live_
    cargo test --lib gui_focus
    cargo test --features test-engine --test recovery gui_focus
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_buddy
    cargo test --features test-engine --lib --test recovery graphical_buddy
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_buddy
    cargo test --lib remote_buddy
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib subagent_history
    cargo test --lib subagent_history
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib job_output
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib job_reports_bound_output
    cargo test --lib job_output
    cargo test --features test-engine --test recovery job_output_cancel
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib export_transfer
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib html_preview
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_frame
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_arcade
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_frames
    cargo test graphical_frame
    cargo test remote_frames
    cargo test --features test-engine --test frames
    cargo test html_preview
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --bin plank gui_console_menu_works_without_terminal_but_one_shots_do_not_prompt
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_command_output_tests
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_controls_never_enter_the_model_prompt_queue
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib frame_roundtrip_all_events
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib client_frame_roundtrip
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_questions
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_editors
    cargo test --lib document_transfers
    cargo test --lib remote_editor_saves
    cargo test --lib console_writes
    cargo test --lib editor_opening_streams
    cargo test --lib editor_requests_
    cargo test --test documents
    cargo test --lib original_document_
    cargo test --features test-engine --test recovery editor_uploads_
    cargo test --features test-engine --test recovery local_editor_opening_streams
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_editor_opening_
    cargo test --lib document::
    cargo test --lib editor_input
    cargo test --features test-engine --test recovery local_editor_streams
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_editor_
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_dialog_stream_pipe
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib ordinary_commands_and_prompts_are_remote_safe
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib metrics_snapshots
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib activity_snapshots
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib engine_inventory_pages
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib stats_report_renders
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib client_insights_report
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib insights_charts
    cargo test --lib insights_charts
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
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib console_aside
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib replay_instances
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib restart_handover
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib restart_completion
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib restart_notification
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_profile_restart
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib graphical_suggestions
    cargo test --no-default-features --lib prompt_suggestions
    cargo test --no-default-features --lib remote_restart
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib remote_stop
    cargo test --no-default-features --lib remote_stop
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --bin plank restart_handover
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib --bin plank startup_engine_questions
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib startup_download_questions
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib --bin plank startup_trust_questions
    cd plank && PLANK_NO_DS4=1 cargo test --no-default-features --lib foreground_url_download
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

[group('run')]
run: build
    cargo run --bin nplank

[group('run')]
appearance: build
    cargo run --bin nplank -- --appearance

[group('run')]
daemon:
    cargo run --no-default-features --bin nplankd

[group('run')]
demo:
    cargo run --bin nplankctl -- demo --directory .

[group('check')]
check:
    cargo fmt --check
    cargo clippy --all-targets --features test-engine -- -D warnings
    cargo test --features test-engine

# Uses an installed Muse CLI with its model-free, memory-only echo provider.
[group('test')]
smoke-muse:
    cargo test --features test-engine --test agents installed_muse_echo_protocol -- --ignored

[group('test')]
components:
    {{toolkit}} component Input Button Sidebar Markdown

[group('test')]
themes:
    {{toolkit}} theme list

[group('test')]
toolkit-check:
    {{toolkit}} doctor --dir .
    {{toolkit}} layout check 'V[g6] > (Hd"Appearance"[level=2] + Tx.sm"Colors and design rules are independently selectable."[muted])'
    {{toolkit}} layout check 'V[g4] > (Hd"Session preview"[level=3] + Tx"Research · Local Plank" + Tx.sm"Running · 32 tokens/s"[muted])'
    {{toolkit}} layout check 'H[g2] > (B.primary"Send"#send + B.destructive"Stop"#stop + B.ghost"Fresh context"#reset)'

[group('build')]
bundle-macos: build
    cargo run --bin nplank-package

[group('build')]
bundle-appearance-macos: build
    cargo run --bin nplank-package -- --appearance

# macOS DMG via scripts/build-macos.sh (unsigned unless DEVELOPER_ID is set).
[group('build')]
dmg:
    ./scripts/build-macos.sh

# iOS static libraries via scripts/build-ios.sh (packaging/signing happens in Xcode).
[group('build')]
ios:
    ./scripts/build-ios.sh

# Sign dist/ DMGs (and optionally notarize) via scripts/sign-macos.sh.
[group('release')]
sign:
    ./scripts/sign-macos.sh

# WASM uses nightly only; native recipes retain the default toolchain.
[group('wasm')]
wasm-setup:
    rustup toolchain install {{wasm_toolchain}} --component rust-src --target wasm32-unknown-unknown
    cargo install wasm-bindgen-cli --version 0.2.127 --locked

[group('wasm')]
wasm-check:
    RUSTUP_TOOLCHAIN={{wasm_toolchain}} CARGO_UNSTABLE_BUILD_STD=std,panic_abort cargo check --target wasm32-unknown-unknown --lib

[group('wasm')]
wasm-build:
    RUSTUP_TOOLCHAIN={{wasm_toolchain}} CARGO_UNSTABLE_BUILD_STD=std,panic_abort cargo build --release --target wasm32-unknown-unknown --lib
    cargo run --no-default-features --bin nplank-web-package

# Opens at http://127.0.0.1:47831; stop an existing daemon before starting this one.
[group('wasm')]
wasm-serve: wasm-build
    cargo run --no-default-features --bin nplankd -- --web-dir target/web

[group('check')]
check-macos:
    cargo check --all-targets --target aarch64-apple-darwin

[group('build')]
build-macos:
    cargo build --release --target aarch64-apple-darwin

# Run on the matching host, or configure its cross-compiler and native SDKs.
[group('check')]
check-linux:
    cargo check --all-targets --target {{linux_target}}

[group('build')]
build-linux:
    cargo build --release --target {{linux_target}}

[group('check')]
check-windows:
    cargo check --all-targets --target {{windows_target}}

[group('build')]
build-windows:
    cargo build --release --target {{windows_target}}

# Shared library checks/builds; mobile launch hosts and app packaging are pending.
[group('check')]
check-ios:
    cargo check --target aarch64-apple-ios-sim --lib

[group('build')]
build-ios:
    cargo build --release --target aarch64-apple-ios --lib

[group('check')]
check-android:
    cargo ndk -t arm64-v8a check --lib

[group('build')]
build-android:
    cargo ndk -t arm64-v8a build --release --lib
