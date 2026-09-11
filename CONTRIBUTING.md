# Contributing to zendesk-cli

Thanks for your interest in contributing! This project provides `zdk`, a secure, scriptable Zendesk CLI for support operators, scripts, and AI agents.

- Code: Rust 2024 edition, MSRV 1.88, `clap` v4, async via `tokio`, HTTP via `reqwest` (rustls with the `ring` provider). Cargo workspace: `crates/zdk-core` (library), `crates/zdk-cli` (the `zdk` binary, package `zendesk-cli`), `xtask` (codegen and spec tooling).
- Style: run `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` before pushing. CI builds with `RUSTFLAGS=-D warnings` and `--locked`. `std::env::var` is only allowed in `crates/zdk-core/src/config/env.rs`; stdout only through `output::write_stdout*`.
- Tests: add unit tests beside changed code. For HTTP behaviour prefer `wiremock` integration tests in `crates/zdk-core/tests/`; black-box CLI tests live in `crates/zdk-cli/tests/{cli,auth_cli,resources_cli}.rs` and use the `common::Harness`. `--help` output, tables and the ticket transcript are `insta` snapshots under `crates/zdk-cli/tests/snapshots/`: after changing wording run `cargo insta test --accept`, review with `cargo insta review`, and commit the `.snap` files. Tests must never touch the OS keyring (`ZENDESK_CREDENTIAL_STORE=none`, which the harness sets).
- Generated code: `crates/zdk-core/src/api/generated/` is produced by `cargo xtask codegen` from `specs/`. Never hand-edit it; regenerate and commit. CI fails if it is stale (`cargo xtask codegen --check`).
- Docs: every `zdk …` invocation in `README.md` must resolve against `target/debug/zdk --help-json`; `typos` runs in CI.
- Commits: [Conventional Commits](https://www.conventionalcommits.org/) (`feat(tickets): …`, `fix(auth): …`, `chore(release): v0.2.0`). PRs are squash-merged, so the PR title is the commit message. Small, focused PRs are easier to review.
- Security: never include secrets, tokens, real subdomains, or ticket contents in tests, fixtures, examples, or logs.

## Dev setup

```bash
rustup toolchain install stable 1.88.0
rustup component add rustfmt clippy llvm-tools-preview
cargo install --locked cargo-deny cargo-llvm-cov cargo-insta typos-cli
export PATH="$HOME/.cargo/bin:$PATH"        # on macOS with Homebrew: rustup's cargo must come first (cross / --target builds)
export ZENDESK_CREDENTIAL_STORE=none

cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --locked
rustup run 1.88.0 cargo check --workspace --all-targets --locked   # MSRV; `cargo +1.88.0` fails when Homebrew's cargo shadows rustup's
cargo deny check bans licenses sources advisories
typos
cargo xtask codegen --check
```

## Pull Requests

- Write a descriptive title (Conventional Commits) and summary; fill in the PR template.
- Link related issues.
- Include usage notes and sample JSON output if useful.
- Update `README.md`, `CHANGELOG.md` (`## [Unreleased]`), tests, and snapshots where applicable.
- A release is a separate PR that bumps `[workspace.package] version`, runs `cargo update --workspace`, and adds a `## [X.Y.Z]` section to `CHANGELOG.md`. See [docs/release-process.md](docs/release-process.md).

## Code of Conduct

This project follows a standard Code of Conduct. Be respectful and inclusive.
