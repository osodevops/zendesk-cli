## Summary

Explain the change, motivation, and user impact.

## Changes

- 

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] `rustup run 1.88.0 cargo check --workspace --all-targets --locked` (MSRV)
- [ ] `cargo deny check`
- [ ] `cargo audit`
- [ ] `typos`
- [ ] `cargo xtask codegen --check`
- [ ] `cargo run -p zendesk-cli -- man --to /tmp/man`

## Release impact

- [ ] No release impact
- [ ] User-facing behavior changed
- [ ] New command, flag, output field, exit code, or error code
- [ ] Docs, README, examples, completions, or man pages updated
- [ ] Packaging, Homebrew, Scoop, or GitHub Actions changed
- [ ] This PR is a release: `[workspace.package] version` bumped, `cargo update --workspace` run, `## [X.Y.Z]` added to CHANGELOG.md

## Safety

- [ ] No secrets, access or refresh tokens, client secrets, Zendesk subdomains, or customer data are committed.
- [ ] Mutating commands support `--yes` and `--dry-run` where appropriate, and destructive ones name the active profile.
- [ ] Machine-readable output (JSON on stdout, error line on stderr) remains stable or the change is documented in CHANGELOG.md.
