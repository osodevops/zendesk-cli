# AGENTS.md

## Repository Purpose

This repository is a Rust workspace that builds `zdk`, a Zendesk CLI for AI agents, scripts and support engineers. It is OAuth-first because Zendesk stops issuing API tokens on 27 October 2026 and disables them on 30 April 2027. It is designed for deterministic subprocess use: plain JSON on stdout, one-line JSON errors on stderr, stable exit codes, and a rate governor that keeps bulk work inside Zendesk's account-wide and per-endpoint limits.

v0.1.0 is complete: `auth`, `config`, `doctor`, `tickets`, `comments`, `users`, `orgs`, `search`, `api`, `completions`, `man`, `version`. The binary is the source of truth — `target/debug/zdk --help-json` dumps the whole tree — and `README.md` must match it.

The core contract:

- Commands emit plain JSON (array or object, **no envelope**) when stdout is not a TTY or `-o json` is given; `-o raw` is the untouched Zendesk body; `ndjson`, `csv`, `tsv`, `yaml`, `table` are the other formats.
- Errors never go to stdout. In machine modes they are one stderr line `{"error":{"code","message","help","exit_code","request_id"}}`; in table mode a miette report.
- Exit codes and `error_code()` strings are stable and map to `ZdkError` variants in `crates/zdk-core/src/error.rs`.
- Logs, progress, prompts and the API-token deprecation warning go to stderr.

```bash
export PATH="$HOME/.cargo/bin:$PATH"          # rustup's cargo, not Homebrew's
export ZENDESK_CREDENTIAL_STORE=none
cargo build
cargo run -p zendesk-cli -- --help
ZENDESK_ACCESS_TOKEN=x ZENDESK_SUBDOMAIN=example target/debug/zdk tickets list --limit 3 --dry-run -o json
```

## Architecture

Three workspace members (`Cargo.toml`, `resolver = "3"`, `[workspace.package] version` is shared and is the release trigger):

- `crates/zdk-core` (lib `zdk_core`): everything except argv parsing. `config/` (file + env + precedence; `env.rs` is the only reader of the process environment), `auth/` (hand-rolled OAuth: PKCE authorization code with a loopback listener, client credentials, rotating refresh, revocation; API-token ramp with the countdown warning; the 52-scope catalogue, presets and `required_for`), `store/` (keyring / encrypted file / env / memory; `auto` probes and caches its decision), `http/` (`ZendeskClient::execute` pipeline, retry, redaction, dry-run, audit observer, `rate_limit/` governor with header parsing, the static `RULES` table and per-profile state), `pagination/` (cursor, offset with the early guard, Link header; `incremental`/`audits` are typed stubs until v0.4), `api/` (`generated/registry.rs` + `detail.json.gz` from the OpenAPI specs; `curated/` hand-written request builders and the `TicketQuery` filter compiler), `models/`, `output/`, `util/`, `error.rs`.
- `crates/zdk-cli` (package `zendesk-cli`, bin `zdk`): `cli.rs` clap tree and global flags, `cmd/*.rs` one handler module per top-level command, `args/` shared parsers (`--field k=v|k:=json`, `--data @file|-`, `--file`/`--from-stdin`, `--body`/`--body-file`/`--editor`, `TicketFilters`, `me|id|email`), `help_json.rs`, `context.rs` (`AppContext`: settings, lazy provider/client, `emit`, confirmations), `main.rs` (tracing, panic hook, ctrl-c → 130, exit code). `build.rs` reserves an 8 MiB Windows stack.
- `xtask`: `cargo xtask codegen [--check]` (registry from `specs/`), `spec-refresh [--spec]`, `spec-diff [--json] [--summary]`. Not published. Inference overrides live in `xtask/overrides.toml` and `xtask/scope_map.toml`.

Other places you will touch: `specs/` (committed Zendesk OpenAPI snapshots + `SPEC_VERSIONS.toml`), `tests/fixtures/{errors,headers,organizations,search,tickets,users}/`, `scripts/` (packaging and the release version guard), `.github/workflows/`, `docs/` (PRD, release process, OAuth migration, recipes).

See `CLAUDE.md` for the file-by-file map and invariants.

## Important Runtime Contracts

`OutputFormat::detect` in `crates/zdk-core/src/output/mod.rs`: `-o` flag → `ZENDESK_OUTPUT` → `[default].output` → table if TTY else json. Colour only when the stream is a TTY and not `NO_COLOR`/`--no-color`. `write_stdout` maps `BrokenPipe` to exit 0.

Exit codes (`ZdkError::exit_code()`) and codes (`error_code()`):

- `0` success (also `--dry-run`, code `DRY_RUN`) · `1` generic (`ERROR`, `IO`) · `2` usage (`USAGE`, before any HTTP) · `3` auth (`AUTH_NOT_LOGGED_IN`, `AUTH_EXPIRED`, `AUTH_REVOKED`, `AUTH_CLIENT_SECRET_REQUIRED`, `AUTH_INVALID_SCOPE`, `AUTH_TOKEN_ENDPOINT`, `AUTH_CODE_EXPIRED`, `AUTH_FLOW_ABORTED`) · `4` `SCOPE_MISSING` (grants known, checked locally) / `FORBIDDEN` (403 or grants unknown) · `5` `NOT_FOUND` · `6` `VALIDATION` (400/409/422) · `7` `RATE_LIMITED` (with `--rate-limit-strategy fail`) · `8` `PARTIAL_FAILURE` (reserved) · `9` `SERVER_ERROR` · `10` `CONFIG` / `CREDENTIAL_STORE` · `11` `NETWORK` · `12` `PAGINATION_LIMIT` · `13` `JOB_TIMEOUT` (reserved) · `130` `INTERRUPTED` (prints nothing).

When adding an error variant, update `exit_code()`, `error_code()`, `help_text()`, `help_json::EXIT_CODES` and the tests together; `error_code()` strings are part of the public contract.

## Auth and Configuration

Credential resolution (`auth::resolve_provider`) is predictable: `ZENDESK_ACCESS_TOKEN` (static bearer, never refreshed) → `ZENDESK_EMAIL` + `ZENDESK_API_TOKEN` → credential store entry for the active profile → `NotLoggedIn` (exit 3). `ZENDESK_REFRESH_TOKEN` is read into `EnvOverrides` but nothing consumes it yet.

Settings precedence: CLI flag → environment (`ZENDESK_*`, read once in `config/env.rs` — clippy forbids `std::env::var` elsewhere) → profile → `[default]` → built-in. Config lives at `zendesk-cli/config.toml` under the platform config dir (XDG honoured on every OS), overridable by `--config` / `ZENDESK_CONFIG`. **No secret is ever a config field.** Unknown keys warn on stderr; `config validate` reports them.

Grants:

- **Authorization code + PKCE** (default, public clients, `client_secret` optional): loopback on `127.0.0.1` (ephemeral port; `--port` / `auth.callback_port` for fixed redirects), single request, state verified, code exchanged immediately (codes live 120 s). `--no-browser` prints the URL and reads the code or full redirect URL from stdin, with redirect `http://127.0.0.1:8484/callback` unless a port is configured; `--redirect-uri` uses a registered URL instead.
- **Client credentials** (confidential, CI): no refresh token; re-minted on expiry from `ZENDESK_CLIENT_SECRET` / `--client-secret` / the secret kept with `--store-secret`.
- **API token** (legacy): Basic `{email}/token:{token}`; one stderr warning per process with the countdown (`api_token::deprecation_warning`); never suggest it in docs as the primary path.
- **Refresh** rotates: persist the new pair, then swap; save failure is reported as exit 10 after the command completes, never retry with the old refresh token. Pre-emptive refresh at 80 % of TTL; `expires_at == None` → refresh only on 401.

Store: `auto` uses the OS keyring (service `zendesk-cli`) when it works and falls back to the encrypted file `credentials.enc` in the config dir (XChaCha20-Poly1305; key from `ZENDESK_CREDENTIALS_PASSPHRASE` via Argon2id, else `credentials.key`); the decision is cached in `state/store-decision.json`. `ZENDESK_CREDENTIAL_STORE=none` (memory) is the test switch and is set in every CI job.

Never commit or log real tokens, client secrets, subdomains, or ticket contents.

## HTTP Behaviour

All Zendesk calls go through `ZendeskClient` (`crates/zdk-core/src/http/`). Per attempt: scope preflight (once, when grants are known) → `--dry-run` short-circuit (prints the request, exit 0, zero quota; `me`/email lookups skipped) → `AuthProvider::authorization()` → `RateGovernor::acquire()` (`fail` strategy → exit 7 without sleeping; `burst` skips the buckets) → send with a stable `Idempotency-Key` on POST/PUT/PATCH → `governor.observe(headers)` + observers (`--audit-log`) → classify: 401 → one `invalidate()` + retry; 429 → `Retry-After` (seconds or HTTP date) else full-jitter backoff; 5xx/transport → backoff while attempts remain; otherwise a typed `ZdkError` carrying `request_id`. `retry::decide()` is pure and table-tested.

The governor starts at 200/min and rewrites its account bucket from the first `X-Rate-Limit` / `ratelimit-limit` header (ArcSwap; persisted per profile under `state/rate_limit/<profile>.json`). Help Center paths use a separate budget. Static `RateRule`s in `rate_limit/endpoint_rules.rs` cover the documented per-endpoint limits (ticket update 100/min + 30/10 min per ticket, incremental 10/min, view execute 5/min per view, user update 5/min per user, create_or_update 5/min per email, org update 5/min per org, search export 100/min, side conversations, agent availabilities, tickets index beyond page 500).

Pagination: `PageDialect` per operation (from the registry or `match_path`); cursor by default; offset stops before page 101 / 10,000 records (and before the first page when `--all` and `count` say it cannot finish) with exit 12 and the alternatives; search warns at 1,000 and treats 422-on-`next_page` as end of results; `--limit` truncates mid-page; `--checkpoint` resumes.

Do not call `reqwest` directly outside `http/` and `auth/`.

## Command Implementation Pattern

Two layers: the **generated** operation registry (`api/generated/registry.rs`, 887 ops, reached via `zdk api <METHOD> <path>`, `zdk api ops`, `zdk api describe`) and **curated** commands (`tickets`, `comments`, `users`, `orgs`, `search`) with typed flags, filters, table presets and sideload joins.

A new curated command touches:

1. `crates/zdk-core/src/api/curated/<resource>.rs` — request builder against the registry operation (`spec(Method, path, "OperationId")`), returning `RequestSpec`s; helpers for envelopes.
2. `crates/zdk-cli/src/cmd/<resource>.rs` — clap subcommand + handler that builds the request from `AppContext`, reusing `args/` helpers.
3. `crates/zdk-core/src/models/` — only if a new resource shape is needed (Option-heavy, `#[serde(flatten)] extra`).
4. `crates/zdk-core/src/auth/scopes.rs` — `COMMAND_SCOPES` entry (a test asserts every entry names catalogue scopes).
5. `crates/zdk-core/src/output/table.rs` — a column preset if the command lists.
6. `crates/zdk-cli/tests/resources_cli.rs` — happy path against wiremock, one error path, `--help` snapshot (`cargo insta test --accept`, then review the `.snap`).
7. `README.md` Capabilities, `CHANGELOG.md` `[Unreleased]`.

Mutating commands must honour `--dry-run`, `--idempotency-key`, and, if destructive, `--yes` with a confirmation naming the active profile and subdomain. List commands take the global pagination flags and stream in `json`/`ndjson`. Unimplemented PRD flags are **not defined** so they fail with exit 2 rather than silently doing nothing.

Generated code: after editing `specs/`, `xtask/src/openapi/*`, `xtask/scope_map.toml` or `xtask/overrides.toml`, run `cargo xtask codegen` and commit the result. CI's `codegen-fresh` job diffs it.

## Testing

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export ZENDESK_CREDENTIAL_STORE=none
cargo test --workspace --all-targets --locked          # everything (unit, wiremock integration, black-box CLI); ~385 tests
cargo test -p zdk-core --test oauth --test client       # one integration suite
cargo test -p zendesk-cli --test resources_cli          # black-box curated-command suite (also: cli, auth_cli)
cargo insta test --accept && cargo insta review         # snapshots (tables, --help, transcript) after wording changes
cargo test -p zendesk-cli --test e2e -- --test-threads 1   # live; needs ZDK_E2E_SUBDOMAIN/CLIENT_ID/CLIENT_SECRET
cargo xtask codegen --check                             # registry freshness
cargo llvm-cov --workspace --ignore-filename-regex '(api/generated/|^xtask/)' --fail-under-lines 70
```

- Unit tests beside code; wiremock integration tests in `crates/zdk-core/tests/{client,governor,oauth,pagination,store}.rs`; the CLI harness (`crates/zdk-cli/tests/common/mod.rs`) sets per-test `HOME`/`XDG_*`, `ZENDESK_CREDENTIAL_STORE=none`, `ZENDESK_SUBDOMAIN=test`, `NO_COLOR=1`, `COLUMNS=100`, strips other `ZENDESK_*`, and `zdk_api()` adds `ZENDESK_BASE_URL=<mock>`, `ZENDESK_ACCESS_TOKEN=test-token` and a zero-backoff, `burst` config.
- `wiremock` is the preferred way to test HTTP behaviour; never require live credentials in the default suite.
- Fixtures in `tests/fixtures/{errors,headers,organizations,search,tickets,users}/`; no real subdomains or data.
- `proptest` covers cursor sequences, projection idempotence, the filter compiler round trip and the `k:=json` parser.

CI (`ci.yml`): check, fmt, clippy `-D warnings`, tests on Ubuntu/macOS/Windows, MSRV 1.88, cargo-deny, cargo-audit, typos, actionlint, generated-code freshness, man/completions smoke, coverage ≥ 70 %, release version guard, and a `CI Complete` gate.

## Gotchas

- `RUSTFLAGS=-D warnings` and `--locked` in CI: dead code, unused imports and a stale `Cargo.lock` fail the build.
- Homebrew's `cargo` shadows rustup's on the maintainer's Mac: `cargo +1.88.0` fails, `cross` and `--target` builds pick the wrong toolchain. `export PATH="$HOME/.cargo/bin:$PATH"` first; use `rustup run 1.88.0 cargo check --workspace --all-targets --locked` for MSRV.
- macOS binds keychain grants to the exact binary signature: every unsigned local build prompts (or is denied) when it touches stored tokens. Use `ZENDESK_CREDENTIAL_STORE=none` or `file` for local development; the test harness already does.
- Keep stdout clean. Only `output::write_stdout*` prints to stdout; clippy's `print_stdout`/`print_stderr` lints enforce it. `write_stdout` maps `BrokenPipe` to exit 0 (`| head` must not error).
- Preserve exit codes and `error_code()` strings; agents branch on them.
- `std::env::var` only in `config/env.rs` (clippy `disallowed_methods`); tests may use it in `tests/` only.
- Snapshots (`crates/zdk-cli/tests/snapshots/*.snap`) cover `--help` text, tables and the transcript; any wording change needs `cargo insta test --accept` or CI fails.
- `typos` is a CI gate (config in `.typos.toml`; British spellings in help text are accepted, generated code and fixtures are excluded).
- Generated registry freshness: `cargo xtask codegen --check` must pass; it is byte-deterministic. xtask has no `man`/`completions` commands — use `zdk man --to DIR` / `zdk completions <shell>`.
- `operationId`s collide across the three specs; always address an operation as `(spec, id)`.
- Zendesk YAML quirks mean `openapiv3` cannot parse the specs; the xtask walker works on `serde_yaml_ng::Value` and `overrides.toml` patches inference. `spec-refresh` refuses to overwrite a snapshot it cannot parse.
- Search is offset-only and capped at 1,000 results; `search export` (cursor) is the escape, and `tickets list --all` with filters takes it automatically.
- The API-token warning must be one line, once per process, stderr only, and suppressible by `--quiet`/`auth.suppress_deprecation`. It does not fire under `--dry-run` (the provider is never asked for a header), and dry-run output always shows `Bearer [redacted]`.
- Windows: the main thread has an 8 MiB stack via `build.rs`; completions/man render on a 16 MiB thread. Keep `panic = "unwind"` in the release profile.
- Global flag names are reserved (`-o/--output` is the format selector, `--all`/`--paginate` is auto-paginate; never reuse them for file paths or "all profiles").
- Keep `README.md`, `--help` text and the CLI tests in sync: a documented flag form gets a regression test, and every `zdk …` in the README must resolve against `--help-json`.

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
cargo insta test --accept && cargo insta review
typos
```

For anything under `specs/` or `xtask/`: `cargo xtask codegen --check`. For workflow changes: `actionlint -color .github/workflows/*.yml`.

If you change CLI help text, command names, output shapes, error codes or exit codes, update `README.md`, `CHANGELOG.md`, `CLAUDE.md`, the insta snapshots and the tests under `crates/zdk-cli/tests/`.
