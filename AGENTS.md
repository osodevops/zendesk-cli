# AGENTS.md

## Repository Purpose

This repository is a Rust workspace that builds `zdk`, a Zendesk CLI for AI agents, scripts and support engineers. It is OAuth-first because Zendesk stops issuing API tokens on 27 October 2026 and disables them on 30 April 2027. It is designed for deterministic subprocess use: plain JSON on stdout, one-line JSON errors on stderr, stable exit codes, and a rate governor that keeps bulk work inside Zendesk's account-wide and per-endpoint limits.

The core contract:

- Commands emit plain JSON (array or object, **no envelope**) when stdout is not a TTY or `-o json` is given; `-o raw` is the untouched Zendesk body; `ndjson`, `csv`, `tsv`, `yaml`, `table` are the other formats.
- Errors never go to stdout. In machine modes they are one stderr line `{"error":{"code","message","help","exit_code","request_id"}}`; in table mode a miette report.
- Exit codes are stable and map to `ZdkError` variants in `crates/zdk-core/src/error.rs`.
- Logs, progress, prompts and the API-token deprecation warning go to stderr.

```bash
cargo build
cargo run -p zendesk-cli -- --help
cargo run -p zendesk-cli -- tickets list --limit 3 -o json
```

## Architecture

Three workspace members (`Cargo.toml`, `resolver = "3"`, `[workspace.package] version` is shared and is the release trigger):

- `crates/zdk-core` (lib `zdk_core`): everything except argv parsing. `config/` (file + env + precedence), `auth/` (hand-rolled OAuth: PKCE authorization code, client credentials, rotating refresh; API-token ramp; scope catalogue), `store/` (keyring / encrypted file / env / memory), `http/` (`ZendeskClient::execute` pipeline, retry, redaction, dry-run, `rate_limit/` governor with header parsing and per-endpoint rules), `pagination/` (cursor, offset, Link header; incremental/audits are typed stubs until v0.4), `api/` (`generated/registry.rs` from the OpenAPI specs + `curated/` hand-written calls), `models/`, `output/`, `error.rs`.
- `crates/zdk-cli` (package `zendesk-cli`, bin `zdk`): `cli.rs` clap tree and global flags, `cmd/*.rs` handlers, `args/` shared parsers (`--field k=v|k:=json`, `--data @file|-`, `me|id|email`), `help_json.rs`, `context.rs` (`AppContext`), `main.rs` (tracing, panic hook, ctrl-c → 130, exit code). `build.rs` reserves an 8 MiB Windows stack.
- `xtask`: `cargo xtask codegen [--check]` (registry from `specs/`), `spec-refresh`, `spec-diff`, `man --out`, `completions --out`. Not published.

Other places you will touch: `specs/` (committed Zendesk OpenAPI snapshots + `SPEC_VERSIONS.toml`), `tests/fixtures/`, `scripts/` (packaging and the release version guard), `.github/workflows/` (see "Release process"), `docs/` (PRD, release process, OAuth migration).

See `CLAUDE.md` for the file-by-file map and invariants.

## Important Runtime Contracts

`OutputFormat::detect` in `crates/zdk-core/src/output/mod.rs`: `-o` flag → `ZENDESK_OUTPUT` → `[default].output` → table if TTY else json. Colour only when TTY and not `NO_COLOR`/`--no-color`. Timestamps are local-time in tables only; RFC 3339 UTC everywhere else.

Exit codes (`ZdkError::exit_code()`):

- `0` success · `1` generic · `2` usage (before any HTTP) · `3` auth (not logged in, expired, revoked, client secret required, `invalid_scope`) · `4` forbidden / missing scope · `5` not found · `6` validation (400/409/422) · `7` rate limited with `--rate-limit-strategy fail` · `8` bulk partial failure (reserved, v0.3) · `9` server error after retries · `10` config or credential-store error · `11` network/TLS · `12` pagination limit · `13` job timeout (reserved, v0.3) · `130` SIGINT.

When adding an error variant, update `exit_code()`, `error_code()`, `help()` and the tests together; `error_code()` strings are part of the public contract.

## Auth and Configuration

Credential resolution (`auth::resolve_provider`) is predictable: `ZENDESK_ACCESS_TOKEN` (optionally with `ZENDESK_REFRESH_TOKEN`) → `ZENDESK_EMAIL` + `ZENDESK_API_TOKEN` → credential store entry for the active profile → `NotLoggedIn` (exit 3).

Settings precedence: CLI flag → environment (`ZENDESK_*`, read once in `config/env.rs` — clippy forbids `std::env::var` elsewhere) → profile → `[default]` → built-in. Config lives at `zendesk-cli/config.toml` under the platform config dir, overridable by `--config` / `ZENDESK_CONFIG`. **No secret is ever a config field.**

Grants:

- **Authorization code + PKCE** (default, public clients, `client_secret` optional): loopback on `127.0.0.1` (ephemeral port, `--port` for fixed redirects), single request, state verified, code exchanged immediately (codes live 120 s). `--no-browser` reads the code from stdin.
- **Client credentials** (confidential, CI): no refresh token; re-minted on expiry from `ZENDESK_CLIENT_SECRET` / `--client-secret` / stored secret.
- **API token** (legacy): Basic `{email}/token:{token}`; one stderr warning per process with the countdown; never suggest it in docs as the primary path.
- **Refresh** rotates: persist the new pair, then swap; save failure is exit 10, never retry with the old refresh token. Pre-emptive refresh at 80 % of TTL; `expires_at == None` → refresh only on 401.

Store: `auto` uses the OS keyring when it works and falls back to the encrypted file `credentials.enc` (XChaCha20-Poly1305; key from `ZENDESK_CREDENTIALS_PASSPHRASE` via Argon2id, else `credentials.key`). `ZENDESK_CREDENTIAL_STORE=none` (memory) is the test switch and is set in every CI job.

Never commit or log real tokens, client secrets, subdomains, or ticket contents.

## HTTP Behaviour

All Zendesk calls go through `ZendeskClient` (`crates/zdk-core/src/http/`). Per attempt: scope preflight (once) → `--dry-run` short-circuit (prints the request, exit 0, zero quota) → `AuthProvider::authorization()` → `RateGovernor::acquire()` (`fail` strategy → exit 7 without sleeping) → send with a stable `Idempotency-Key` on POST/PUT/PATCH → `governor.observe(headers)` + observers (`--audit-log`) → classify: 401 → one `invalidate()` + retry; 429 → `Retry-After` (seconds or HTTP date) else full-jitter backoff; 5xx/transport → backoff while attempts remain; otherwise a typed `ZdkError` carrying `request_id`. `retry::decide()` is pure and table-tested.

The governor starts at 200/min and rewrites its account bucket from the first `X-Rate-Limit` / `ratelimit-limit` header (ArcSwap; persisted per profile). Help Center paths use a separate budget. Static `RateRule`s cover the PRD §4.2 per-endpoint limits (ticket update 100/min + 30/10 min per ticket, incremental 10/min, user update 5/min/user, …).

Pagination: `PageDialect` per operation (from the registry or `match_path`); cursor by default; offset stops before page 101 with exit 12 and the alternative; search warns at 1 000 and treats 422-on-`next_page` as end of results; `--limit` truncates mid-page; `--checkpoint` resumes.

Do not call `reqwest` directly outside `http/` and `auth/oauth.rs`.

## Command Implementation Pattern

Two layers: the **generated** operation registry (`api/generated/registry.rs`, 887 ops, reached via `zdk api <METHOD> <path>`, `zdk api ops`, `zdk api describe`) and **curated** commands (`tickets`, `comments`, `users`, `orgs`, `search`) with typed flags, filters, table presets and sideload joins.

A new curated command touches:

1. `crates/zdk-cli/src/cmd/<group>.rs` — clap subcommand + handler that builds the request from `AppContext`.
2. `crates/zdk-core/src/api/curated/<group>.rs` — request builder against the registry operation (`find_op`), returning models.
3. `crates/zdk-core/src/models/` — only if a new resource shape is needed (Option-heavy, `#[serde(flatten)] extra`).
4. `crates/zdk-core/src/auth/scopes.rs` — `required_for(["group","verb"])` mapping (a test asserts every command path maps to at least one scope).
5. `crates/zdk-core/src/output/table.rs` — a column preset if the command lists.
6. `README.md` Capabilities, `CHANGELOG.md` `[Unreleased]`.
7. `crates/zdk-cli/tests/cli.rs` — happy path against wiremock, one error path, `--help` insta snapshot.

Mutating commands must honour `--dry-run`, `--idempotency-key`, and, if destructive, `--yes` with a confirmation naming the active profile. List commands take the global pagination flags and stream in `json`/`ndjson`. Unimplemented PRD flags are **not defined** so they fail with exit 2 rather than silently doing nothing.

Generated code: after editing `specs/`, `xtask/src/openapi/*`, `xtask/src/scope_map.toml` or `overrides.toml`, run `cargo xtask codegen` and commit the result. CI's `codegen-fresh` job diffs it.

## Testing

```bash
export ZENDESK_CREDENTIAL_STORE=none
cargo test --workspace --all-targets --locked          # everything (unit, wiremock integration, black-box CLI)
cargo test -p zdk-core --test oauth --test client       # one integration suite
cargo test -p zendesk-cli --test cli                    # black-box CLI suite
cargo insta test --accept && cargo insta review         # snapshots (tables, --help, errors, api describe)
cargo test -p zendesk-cli --test e2e -- --test-threads 1   # live; needs ZDK_E2E_SUBDOMAIN/CLIENT_ID/CLIENT_SECRET
cargo llvm-cov --workspace --ignore-filename-regex '(api/generated/|^xtask/)' --fail-under-lines 70
```

- Unit tests beside code; wiremock integration tests in `crates/zdk-core/tests/`; the CLI harness (`crates/zdk-cli/tests/common/mod.rs`) sets per-test `HOME`/`XDG_*`, `ZENDESK_CREDENTIAL_STORE=none`, `ZENDESK_ACCESS_TOKEN=test-token`, `ZENDESK_SUBDOMAIN=test`, `ZENDESK_BASE_URL=<mock>`, `NO_COLOR=1`, zero retry backoff, and strips other `ZENDESK_*`.
- `wiremock` is the preferred way to test HTTP behaviour; never require live credentials in the default suite.
- Fixtures in `tests/fixtures/{tickets,users,organizations,search,errors,headers,help_center,oauth}/`; no real subdomains or data.
- `proptest` covers cursor sequences, projection idempotence, the filter compiler round trip and the `k:=json` parser.

CI (`ci.yml`): check, fmt, clippy `-D warnings`, tests on Ubuntu/macOS/Windows, MSRV 1.88, cargo-deny, cargo-audit, typos, actionlint, generated-code freshness, man/completions smoke, coverage ≥ 70 %, release version guard, and a `CI Complete` gate.

## Gotchas

- `RUSTFLAGS=-D warnings` and `--locked` in CI: dead code, unused imports and a stale `Cargo.lock` fail the build.
- `cargo +1.88.0` fails on the maintainer's Mac (Homebrew cargo shadows rustup); use `rustup run 1.88.0 cargo check --workspace --all-targets --locked`.
- macOS binds keychain grants to the exact binary signature: every unsigned local build prompts (or is denied) when it touches stored tokens. Use `ZENDESK_CREDENTIAL_STORE=none` or `file` for local development; the test harness already does.
- Keep stdout clean. Only `output::write_stdout` prints to stdout; clippy's `print_stdout`/`print_stderr` lints enforce it. `write_stdout` maps `BrokenPipe` to exit 0 (`| head` must not error).
- Preserve exit codes and `error_code()` strings; agents branch on them.
- `std::env::var` only in `config/env.rs` (clippy `disallowed_methods`).
- Generated registry freshness: `cargo xtask codegen --check` must pass; it is byte-deterministic.
- `operationId`s collide across the three specs; always address an operation as `(spec, id)`.
- Zendesk YAML quirks mean `openapiv3` cannot parse the specs; the xtask walker works on `serde_yaml_ng::Value` and `overrides.toml` patches inference. `spec-refresh` refuses to overwrite a snapshot it cannot parse.
- Search is offset-only and capped at 1 000 results; `search export` (cursor) is the escape.
- The API-token warning must be one line, once per process, stderr only, and suppressible by `--quiet`/`auth.suppress_deprecation`.
- Windows: the main thread has an 8 MiB stack via `build.rs`; completions/man render on a 16 MiB thread. Keep `panic = "unwind"` in the release profile.
- Global flag names are reserved (`-o/--output` is the format selector; never reuse it for file paths).
- Keep `README.md`, `--help` text and `tests/cli.rs` in sync: a documented flag form gets a regression test.

## Release Process

Version-bump driven: bump `[workspace.package] version`, run `cargo update --workspace`, add `## [X.Y.Z]` to `CHANGELOG.md`, merge; `auto-tag.yml` pushes the tag and `release.yml` builds 7 targets, attests, publishes, and updates the Homebrew tap and Scoop bucket. Full runbook, secrets and rollback table: `docs/release-process.md`. Never create a release by running `release.yml` manually; `-f tag=` only re-publishes an existing tag.

## Before Finishing Changes

For most changes:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
```

For command, model, API, error or shared-core changes, also:

```bash
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo insta review
```

For anything under `specs/` or `xtask/`: `cargo xtask codegen --check`. For workflow changes: `actionlint -color .github/workflows/*.yml`.

If you change CLI help text, command names, output shapes, error codes or exit codes, update `README.md`, `CHANGELOG.md`, `CLAUDE.md`, the insta snapshots and `crates/zdk-cli/tests/cli.rs`.
