# zendesk-cli (`zdk`)

Rust workspace producing `zdk`, an OAuth-first Zendesk CLI for AI agents and support operations. Package `zendesk-cli`, binary `zdk`, MSRV 1.88, edition 2024. The PRD is `docs/zendesk-cli-prd.md`; v0.1.0 scope is its §20 roadmap row (auth, credential store, `api` escape hatch, tickets/comments/users/orgs/search, output, governor, pagination, config, completions, man pages).

## Build Commands

```bash
export ZENDESK_CREDENTIAL_STORE=none                                   # tests must never touch the OS keyring
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --locked
rustup run 1.88.0 cargo check --workspace --all-targets --locked       # MSRV (not `cargo +1.88.0`: Homebrew's cargo shadows rustup's on this Mac)
cargo deny check bans licenses sources advisories
cargo audit --deny warnings
typos
actionlint -color .github/workflows/*.yml
cargo xtask codegen && git diff --exit-code -- crates/zdk-core/src/api/generated specs/
cargo llvm-cov --workspace --ignore-filename-regex '(api/generated/|^xtask/)' --fail-under-lines 70
python3 scripts/check-release-version.py --base-ref HEAD --mode guard   # "Version unchanged"
cargo run -p zendesk-cli -- man --to /tmp/man && cargo run -p zendesk-cli -- completions zsh | head
```

CI (`.github/workflows/ci.yml`) runs exactly this list; `CI Complete` is the required check. `RUSTFLAGS=-D warnings` and `--locked` everywhere, so an unused import or a stale `Cargo.lock` fails the build.

## Architecture Map

| Path | Owns |
|---|---|
| `Cargo.toml` | workspace: `resolver = "3"`, `[workspace.package] version` (the release trigger), all dependency versions, lints, release profile |
| `crates/zdk-core/src/config/{mod,env,profiles,settings}.rs` | `ConfigFile` (PRD §13.1), `EnvOverrides` (§13.2, the **only** place `std::env::var` is read), `Settings::resolve` precedence merge |
| `crates/zdk-core/src/auth/{token,oauth,authorization_code,client_credentials,api_token,refresh,revoke,scopes}.rs` | `TokenSet`, `Credential`, `AuthProvider`, hand-rolled OAuth (PKCE, client credentials, rotating refresh), API-token countdown warning, 52-scope catalogue + presets + `preflight` |
| `crates/zdk-core/src/store/{keyring,file,env,memory}.rs` | `CredentialStore`: keyring (`keyring` 4 `v1` API), encrypted file `credentials.enc` (XChaCha20-Poly1305, Argon2id or machine key), env (read-only), memory (tests, `--credential-store none`); `auto` probes and falls back |
| `crates/zdk-core/src/http/{request,response,retry,idempotency,redact,dry_run,observer}.rs` + `http/rate_limit/{headers,endpoint_rules,state}.rs` | `ZendeskClient::execute` pipeline: scope preflight → dry-run → auth → governor → send with `Idempotency-Key` → observe headers → classify/retry; `retry::decide()` is pure |
| `crates/zdk-core/src/pagination/{cursor,offset,link_header,incremental,audits}.rs` | `PageDialect`, `Paginator`, `stream_pages` with `--limit`/`--checkpoint`; offset guard before page 101 → exit 12 |
| `crates/zdk-core/src/api/{mod,template}.rs`, `api/generated/{registry.rs,detail.json.gz}`, `api/curated/*.rs` | `Spec`, `find_op`, `match_path`; **generated** static `OPERATIONS` registry (887 ops) from `specs/`; hand-written curated calls for tickets/comments/users/orgs/search/me |
| `crates/zdk-core/src/models/*.rs` | Option-heavy serde models with `#[serde(flatten)] extra` |
| `crates/zdk-core/src/output/{json,ndjson,yaml,csv,raw,table,project,sideload,jq,progress}.rs` | `OutputFormat` detection, `write_stdout` (BrokenPipe → exit 0), projection, sideload joins, jaq filter, comfy-table presets |
| `crates/zdk-core/src/error.rs` | `ZdkError` (thiserror + miette): `exit_code()`, `error_code()`, `help()`, `request_id` |
| `crates/zdk-cli/src/{main,cli,context,help_json}.rs`, `args/`, `cmd/*.rs` | clap tree (global flags PRD §7.1), `--help-json`, panic hook, ctrl-c → 130, dispatch, exit-code mapping; `build.rs` reserves an 8 MiB Windows stack |
| `crates/zdk-cli/tests/{cli.rs,common/mod.rs,e2e.rs}` | black-box `assert_cmd` + wiremock suite; `e2e.rs` runs only with `ZDK_E2E_*` |
| `xtask/src/{main,specs,diff}.rs`, `xtask/src/openapi/{walk,infer,emit}.rs`, `xtask/src/{scope_map,overrides}.toml` | `cargo xtask codegen [--check] | spec-refresh | spec-diff | man | completions` |
| `specs/{support,help_center,voice}.yaml`, `specs/SPEC_VERSIONS.toml` | committed Zendesk OpenAPI snapshots (Support 645 ops, Help Center 182, Talk 60) and their provenance |
| `scripts/package.sh`, `scripts/check-release-version.py` | release archive layout; version-bump guard used by CI and auto-tag |
| `.github/workflows/{ci,auto-tag,release,spec-refresh,release-preflight,e2e}.yml` | CI gate; tag on version bump; 7-target release + attestations + tap/bucket dispatch; weekly spec PR; PAT/downstream preflight; secret-gated live tests |

## Non-negotiable invariants

1. **stdout is data.** Plain JSON (no envelope) or the chosen format; never a log line, prompt or progress bar. All diagnostics go to stderr; in machine modes an error is one stderr line `{"error":{"code","message","help","exit_code","request_id"}}`. Only `output::write_stdout` may print to stdout (clippy `print_stdout` is on).
2. **Exit codes are stable** (table below). Update `exit_code()`, `error_code()` and the tests together.
3. **Secrets never touch disk in plaintext and never appear in logs**, `--audit-log`, `config show` (without `--reveal-secrets`), snapshots or fixtures. `SecretString` everywhere; `-vvv` redacts `authorization`, `cookie`, `access_token`, `refresh_token`, `client_secret`, `token`, `password`.
4. **Tests never touch the OS keyring.** The harness sets `ZENDESK_CREDENTIAL_STORE=none`, per-test `HOME`/`XDG_*`, `ZENDESK_BASE_URL=<wiremock>`; every CI `env:` block sets the store to `none`.
5. **Generated code is committed and reproducible.** `cargo xtask codegen` on unchanged specs is byte-identical; CI diffs it. Never hand-edit `api/generated/`; use `xtask/src/overrides.toml`.
6. **Destructive commands need `--yes` or an interactive confirmation that names the active profile**; on a non-TTY without `--yes` they exit 2 before any HTTP.
7. **Never request an empty OAuth scope** (Zendesk grants full read+write); `validate_requested` rejects it.
8. **Refresh-token rotation is persisted before use.** Save failure → exit 10 with "run `zdk auth login`"; never reuse the old refresh token.

## Exit codes (PRD §14.3)

0 success · 1 generic · 2 usage · 3 auth · 4 authorisation/scope · 5 not found · 6 validation (400/409/422) · 7 rate limited with `fail` · 8 bulk partial failure (reserved) · 9 server error after retries · 10 config/credential store · 11 network/TLS · 12 pagination limit · 13 job timeout (reserved) · 130 SIGINT.

## Release gotchas

- A release is `[workspace.package] version` bump on `main` + `## [X.Y.Z]` in `CHANGELOG.md` + `cargo update --workspace` (the guard checks every member's `Cargo.lock` entry). Members use `version.workspace = true`; never set a member version.
- `auto-tag.yml` pushes the tag with the `HOMEBREW_TAP_TOKEN` PAT. Never `gh workflow run release.yml` to *create* a release; `-f tag=vX.Y.Z` only re-publishes an existing tag.
- The `homebrew`/`scoop` polls can fail a release after the GitHub Release exists. Re-run the downstream workflow (`gh workflow run update-zendesk-cli-formula.yml -R osodevops/homebrew-tap -f tag=…` / `gh workflow run update-manifest.yml -R osodevops/scoop-bucket -f tag=… -f product=zdk`) then `gh run rerun <id> --failed`. Never delete a release users may have installed; ship a patch version.
- Runner labels are explicit (`macos-15`, `macos-15-intel`, `ubuntu-24.04-arm`, `windows-11-arm`, …). Check GitHub's deprecations quarterly; `macos-14` retires 2 Nov 2026.
- musl targets must stay fully static (`ring` TLS provider, pure-Rust keyring backends); the build job `file`-checks it. Adding a crate with a C dependency can break this silently until release day — check with `cross build --target x86_64-unknown-linux-musl` locally.
- Windows: `crates/zdk-cli/build.rs` reserves an 8 MiB main-thread stack; completions/man run on a 16 MiB thread; `panic = "unwind"` so a panic exits 1 instead of aborting.
- Third-party actions are SHA-pinned with a version comment; resolve with `gh api repos/<o>/<r>/git/ref/tags/<tag>` (dereference annotated tags). Dependabot keeps them fresh.
- Full runbook and rollback table: `docs/release-process.md`.

## Command implementation pattern

A new curated command touches: `crates/zdk-cli/src/cmd/<group>.rs` (clap args + handler), `crates/zdk-core/src/api/curated/<group>.rs` (request builder against the registry op), `models/` if a new shape, `auth/scopes.rs` `required_for` (every command path maps to ≥ 1 scope — there is a test), `output/table.rs` preset if it lists, `README.md` Capabilities, `tests/cli.rs` (happy path, one error path, `--help` insta snapshot). Read-only endpoints not worth curating are reachable through `zdk api` — do not add a curated command without a table preset and filters that earn it.

## Gotchas

- `cargo +1.88.0` does not work on this Mac (Homebrew cargo first on PATH); use `rustup run 1.88.0 cargo …`.
- macOS keychain grants are bound to the exact binary signature, so every unsigned local build prompts (or fails) when it touches stored tokens. Keep `ZENDESK_CREDENTIAL_STORE=none` (or `file`) for local runs unless you are testing the keyring on purpose.
- `RUSTFLAGS=-D warnings` and `--locked` in CI: dead code, unused imports, and a `Cargo.lock` that needs updating all fail.
- `cargo xtask codegen --check` fails if the registry is stale; run `cargo xtask codegen` after touching `specs/`, `xtask/`, `scope_map.toml` or `overrides.toml`.
- `openapiv3` is deliberately not used: the Zendesk specs have quirks (undefined `oauth2` scheme, `deepObject` on `oneOf`, a bare `=` scalar) that a strict parser rejects. The walker is `serde_yaml_ng::Value` → `serde_json::Value`.
- `oauth2` crate is deliberately not used (it pins reqwest 0.12; we are on 0.13). OAuth token POSTs are hand-rolled in `auth/oauth.rs` with `_at(base_url)` seams for wiremock.
- `operationId`s collide across specs (`ListLocales`, `ShowComment`); registry identity is `(spec, id)`, displayed as `support.ListTickets`.
- Search (`GET /api/v2/search`) is offset-only and capped at 1 000 results; ticket filters that need more must route through `search/export` (`--all` does this).
- Coverage floor is 70 % lines (generated code and xtask excluded); it rises 5 points per minor toward the PRD's 80 %.
