# Repository Guidelines

## Project Structure & Module Organization

The Git repository is in `plank/`; run development commands there (`cd plank`). Read its existing `AGENTS.md` for detailed implementation guidance.

- `src/`: agent loop, inference adapters, terminal UI, sessions, and tools.
- `crates/`: shared libraries, local inference, Gemma, GGUF deltas, and tracing.
- `tests/` and `tests/fixtures/`: integration tests and committed parity fixtures.
- `refs/`: reference and vendored Git submodules.
- `guests/` and `spike/`: WASM plugins and experimental guest tooling.
- `docs/`, `user-guide/`, `examples/`, `assets/`, and `site/`: design documentation, usage material, examples, and website resources.

## Build, Test, and Development Commands

Use the toolchain pinned in `rust-toolchain.toml`. Initialize dependencies with `git submodule update --init --recursive`.

- `cargo build`: compile a debug build.
- `cargo build --release`: build the optimized executable.
- `cargo run -- --help`: inspect local launch options.
- `cargo test --lib`: run model-free unit tests.
- `cargo test --lib <name>`: filter unit tests by name.
- `cargo fmt --check`: verify formatting.
- `cargo clippy --all-targets -- -D warnings`: run the main CI lint gate.

DeepSeek inference requires macOS/Metal and model files. `PLANK_NO_DS4=1` skips the native C engine build.

## Coding Style & Naming Conventions

Use idiomatic Rust 2024 with four-space indentation and rustfmt formatting. Use `snake_case` for modules/functions and `PascalCase` for types. Fix Clippy warnings; workspace lint settings include pedantic and performance checks. Gate native-engine code with `#[cfg(ds4_engine)]`. Preserve reference prompt text and wire formats byte-for-byte.

## Testing Guidelines

Use Rust's built-in test framework with descriptive behavior-based names. Add regression tests beside affected logic or in `tests/`. Run `cargo test --release` for integration coverage and `cargo test --release -p local-inference-engine` for engine changes. Build WASM guests before guest-backed tests; CI sets `PLANK_REQUIRE_GUESTS=1` to prevent silent skips. No numeric coverage threshold is specified.

## Commit & Pull Request Guidelines

History predominantly uses short imperative subjects, such as “List --pick-engine in --help”; occasional maintenance commits use `chore:`. Keep commits focused. PRs should explain the behavior change, link relevant issues, and report verification commands. Include screenshots or GIFs for visible terminal UI changes. Update relevant documentation and record newly discovered parity quirks in `FINDINGS.md`.
