# Repository Guidelines

## Project Structure & Module Organization

This repository is a Linux-only, Rust-based distributed agent runtime prototype.

- `crates/protocol` contains shared wire types and run/job/task models.
- `crates/coordinator`, `crates/worker`, and `crates/cli` provide the current binaries.
- `crates/policy`, `crates/sandbox-linux`, `crates/executor`, and `crates/jobstore` implement the launch path and persistence boundary.
- `docs/v0.1.md` is the active architecture/design reference.
- `examples/allow-all.yaml` is the initial development policy profile.
- `LICENSE` contains the project license.

Planned crates such as `mcp-server` and `telemetry` are described in `docs/v0.1.md`; keep new code aligned with those boundaries.

## Build, Test, and Development Commands

- `cargo fmt --all -- --check` verifies Rust formatting.
- `cargo check --workspace` checks all crates.
- `cargo clippy --workspace --all-targets -- -D warnings` runs lint checks.
- `cargo test --workspace` runs all crate tests.
- `cargo build --workspace` verifies the full workspace compiles.
- `cargo run --bin agent-coordinator -- --listen 127.0.0.1:8765` starts a local coordinator.
- `cargo run --bin agent-worker -- --coordinator ws://127.0.0.1:8765 --node-name node-a` starts a local worker.
- `cargo run --bin agentctl -- nodes` lists connected workers.

## Coding Style & Naming Conventions

Use Rust 2021 or newer when code is added, with `rustfmt` defaults. Prefer explicit module names matching runtime concepts, such as `policy`, `sandbox_linux`, `executor`, and `jobstore`. Keep public protocol types in the protocol crate and avoid duplicating wire structs across coordinator and worker crates.

Markdown should use sentence-case headings, fenced code blocks with language hints, and concise lists.

## Testing Guidelines

Place unit tests beside Rust modules with `#[cfg(test)]`. Use integration tests under each crate's `tests/` directory when behavior crosses module boundaries, especially coordinator-worker protocol flows, policy enforcement, sandbox preparation, process lifecycle, and log streaming. Name tests by behavior, for example `rejects_disallowed_command` or `streams_stdout_until_exit`.

## Commit & Pull Request Guidelines

Git history currently only establishes `Initial commit`; there is no detailed convention yet. Use short, imperative commit subjects, for example `Add worker heartbeat design`. Keep PRs focused, describe the behavioral or design change, link related issues, and include command output for any build, lint, or test checks run.

## Security & Configuration Tips

Do not commit tokens, `.env` files, node credentials, logs with secrets, or machine-specific policy files. Design and code changes must preserve the core flow: `PolicyEngine -> SandboxBackend -> Executor`, with worker-side enforcement as the final authority.
