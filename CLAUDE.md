# zendesk-cli (`zdk`)

Rust workspace producing `zdk`, an OAuth-first Zendesk CLI for AI agents and support operations. Package `zendesk-cli`, binary `zdk`, MSRV 1.88, edition 2024. The PRD is `docs/zendesk-cli-prd.md`; v0.1.0 (auth, credential stores, `api` escape hatch, tickets/comments/users/orgs/search, output, governor, pagination, config, doctor, completions, man pages) is complete. The binary and the code are the source of truth; `README.md` is reconciled against `zdk --help-json`.

## Build Commands

```bash
export PATH="$HOME/.cargo/bin:$PATH"                                   # rustup's cargo first (Homebrew's shadows it on this Mac)
export ZENDESK_CREDENTIAL_STORE=none                                   # tests and local runs must never touch the OS keyring
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo insta test --accept && cargo insta review                        # when help text, tables or error reports change
rustup run 1.88.0 cargo check --workspace --all-targets --locked       # MSRV (not `cargo +1.88.0`)
cargo deny check bans licenses sources advisories
cargo audit --deny warnings
typos
actionlint -color .github/workflows/*.yml
cargo xtask codegen --check                                            # or: cargo xtask codegen && git diff --exit-code -- crates/zdk-core/src/api/generated specs/
cargo llvm-cov --workspace --ignore-filename-regex '(api/generated/|^xtask/)' --fail-under-lines 70
python3 scripts/check-release-version.py --base-ref HEAD --mode guard   # "Version unchanged"
cargo run -p zendesk-cli -- man --to /tmp/man && cargo run -p zendesk-cli -- completions zsh | head
target/debug/zdk --help-json > /tmp/zdk-help.json                       # the command tree the docs are checked against
```

CI (`.github/workflows/ci.yml`) runs exactly this list; `CI Complete` is the required check. `RUSTFLAGS=-D warnings` and `--locked` everywhere, so an unused import or a stale `Cargo.lock` fails the build.

## Architecture Map

| Path | Owns |
|---|---|
| `Cargo.toml` | workspace: `resolver = "3"`, `[workspace.package] version` (the release trigger), all dependency versions, lints (`print_stdout`/`print_stderr` warn, pedantic), release profile (`panic = "unwind"`) |
| `clippy.toml` | `msrv`, `disallowed-methods` for `std::env::var` / `var_os` (read the environment once in `config/env.rs`) |
| `crates/zdk-core/src/lib.rs`, `error.rs` | `ZdkError` (thiserror + miette): `exit_code()`, `error_code()`, `help_text()`, `request_id()`; `AuthFailure`, `RateBudget`, `ValidationDetail` |
| `crates/zdk-core/src/config/{mod,env,profiles,settings}.rs` | `ConfigFile` (every section typed, unknown keys collected), `Paths` (config/state/cache dirs, XDG on every OS), `starter_toml`; `EnvOverrides` (the **only** reader of the process environment); `profiles.rs` (comment-preserving `toml_edit` edits); `Settings::resolve` precedence merge |
| `crates/zdk-core/src/auth/{mod,token,oauth,authorization_code,client_credentials,api_token,refresh,revoke,scopes}.rs` | `resolve_provider` (env token → env API token → store), `TokenSet`/`Credential`, `AuthProvider` (`StaticBearer`, `OAuthProvider`, `ApiTokenProvider`), hand-rolled OAuth (`oauth/tokens`, PKCE + loopback listener, client credentials, rotating refresh persisted before use, revocation), the API-token countdown warning, the 52-scope catalogue, 4 presets, `required_for` command→scope map and `preflight` |
| `crates/zdk-core/src/store/{mod,keyring,file,env,memory}.rs` | `CredentialStore` + `open`/`open_with`: keyring (`keyring` 4 `v1` API, service `zendesk-cli`), encrypted file `credentials.enc` (XChaCha20-Poly1305; Argon2id passphrase or `credentials.key` machine key), env (read-only), memory (`none`); `auto` probes once and caches `store-decision.json` in the state dir |
| `crates/zdk-core/src/http/{mod,request,response,retry,idempotency,redact,dry_run,observer}.rs` + `http/rate_limit/{mod,headers,endpoint_rules,state}.rs` | `ZendeskClient::execute` pipeline: scope preflight → dry-run → auth → `RateGovernor::acquire` → send with `Idempotency-Key` → `observe` headers + observers (`--audit-log`) → classify/retry (`retry::decide()` is pure); the governor (GCRA per family, learned account limit persisted per profile, static `RULES` table keyed by path param or body field, reserve holds, concurrency semaphore) |
| `crates/zdk-core/src/pagination/{mod,cursor,offset,link_header,incremental,audits}.rs` | `PageDialect`, `Paginator`, `stream_pages` (`--all`, `--limit`, `--checkpoint`, ctrl-c, progress); offset `guard` before page 101 / 10,000 records → exit 12; search cap warning; `incremental`/`audits` are typed stubs until v0.4 |
| `crates/zdk-core/src/api/{mod,template}.rs`, `api/generated/{mod,registry}.rs` + `detail.json.gz`, `api/curated/{mod,tickets,comments,users,organizations,search,me}.rs` | `Spec`, `Method`, `Operation`, `find_operations`, `match_path`, `path_params`; the **generated** `OPERATIONS` registry (887 ops) and gzip'd parameter/schema detail from `specs/`; curated request builders (`RequestSpec`s against registry ops), the ticket filter compiler `TicketQuery` (compile ↔ parse, proptested) |
| `crates/zdk-core/src/models/{comment,common,organization,search,ticket,user}.rs` | Option-heavy serde models with `#[serde(flatten)] extra` |
| `crates/zdk-core/src/output/{mod,json,ndjson,yaml,csv,raw,table,project,sideload,jq,progress}.rs` | `OutputFormat::detect`, `OutputSink` streaming, `write_stdout` (BrokenPipe → exit 0), `warn`/`error_line` (the only stderr paths besides tracing), projection, sideload joins, jaq filter, comfy-table presets (`tickets`, `users`, `organizations`, `comments`, `search`) |
| `crates/zdk-core/src/util/{fs,time}.rs` | `atomic_write_0600`, `parse_human_time` (`24h`, `"2 hours ago"`, `yesterday`, dates, RFC 3339) |
| `crates/zdk-cli/src/{main,cli,context,help_json}.rs` | `main` (tracing, panic hook → one stderr line + exit 1, ctrl-c → 130, `--help-json` handled before clap), the clap tree and global flags, `AppContext` (settings, lazy provider/client, `emit`, confirmations), the `--help-json` shape |
| `crates/zdk-cli/src/args/{body,field,filters,ids,list}.rs` | `--data @file|-|json`, `--file`/`--from-stdin`, `--body`/`--body-file`/`--editor`, `k=v|k:=json` dotted fields, `TicketFilters` → `TicketQuery`, `me|id|email` user refs, list-flag plumbing |
| `crates/zdk-cli/src/cmd/{mod,auth,tickets,comments,users,orgs,search,api,config,doctor,completions,man,version}.rs` | one handler module per top-level command; `cmd::dispatch` |
| `crates/zdk-cli/build.rs` | reserves an 8 MiB main-thread stack on Windows |
| `crates/zdk-cli/tests/{cli,auth_cli,resources_cli,e2e}.rs`, `tests/common/mod.rs`, `tests/snapshots/*.snap` | black-box `assert_cmd` + wiremock suites; `common::Harness` isolates HOME/XDG and strips `ZENDESK_*`; insta snapshots of `--help`, tables and the transcript; `e2e.rs` runs only with `ZDK_E2E_SUBDOMAIN/CLIENT_ID/CLIENT_SECRET` |
| `crates/zdk-core/tests/{client,governor,oauth,pagination,store}.rs` | wiremock integration suites for the HTTP pipeline, governor, OAuth engine, pagination and credential stores |
| `tests/fixtures/{errors,headers,organizations,search,tickets,users}/*.json` | mock response bodies (no real data) |
| `xtask/src/{main,specs,diff}.rs`, `xtask/src/openapi/{mod,walk,infer,emit}.rs`, `xtask/{scope_map,overrides}.toml` | `cargo xtask codegen [--check] | spec-refresh [--spec] | spec-diff [--json] [--summary]` |
| `specs/{support,help_center,voice}.yaml`, `specs/SPEC_VERSIONS.toml` | committed Zendesk OpenAPI snapshots (Support 645 ops, Help Center 182, Talk 60) and their provenance |
| `scripts/package.sh`, `scripts/check-release-version.py` | release archive layout; version-bump guard used by CI and auto-tag |
| `.github/workflows/{ci,auto-tag,release,spec-refresh,release-preflight,e2e}.yml` | CI gate; tag on version bump; 7-target release + attestations + tap/bucket dispatch; weekly spec PR; PAT/downstream preflight; secret-gated live tests |

## Non-negotiable invariants

1. **stdout is data.** Plain JSON (no envelope) or the chosen format; never a log line, prompt or progress bar. All diagnostics go to stderr; in machine modes an error is one stderr line `{"error":{"code","message","help","exit_code","request_id"}}`. Only `output::write_stdout*` may print to stdout (clippy `print_stdout` is on); only `output::warn` / `output::error_line` / tracing may print to stderr.
2. **Exit codes and `error_code()` strings are stable** (table below). Update `exit_code()`, `error_code()`, `help_text()`, `help_json::EXIT_CODES` and the tests together.
3. **Secrets never touch disk in plaintext and never appear in logs**, `--audit-log`, `config show` (without `--reveal-secrets`), snapshots or fixtures. `SecretString` everywhere; `Debug` impls print `[redacted]`; `redact.rs` scrubs `authorization`, `proxy-authorization`, `cookie`, `set-cookie` and `access_token` / `refresh_token` / `client_secret` / `api_token` / `token` / `password` values.
4. **Tests never touch the OS keyring.** The harness sets `ZENDESK_CREDENTIAL_STORE=none`, per-test `HOME`/`XDG_*`, `ZENDESK_BASE_URL=<wiremock>`; every CI `env:` block sets the store to `none`.
5. **Generated code is committed and reproducible.** `cargo xtask codegen` on unchanged specs is byte-identical; CI diffs it. Never hand-edit `api/generated/`; patch inference in `xtask/overrides.toml` or `xtask/scope_map.toml`.
6. **Destructive commands need `--yes` or an interactive confirmation that names the active profile and subdomain**; on a non-TTY without `--yes` they exit 2 before any HTTP.
7. **Never request an empty OAuth scope** (Zendesk grants full read+write); `scopes::validate_requested` rejects it and `auth login` falls back to the `agent` preset when nothing is configured.
8. **Refresh-token rotation is persisted before use.** Save failure → the command completes, then exit 10 with "run `zdk auth login`"; never reuse the old refresh token.
9. **`std::env::var` only in `config/env.rs`** (clippy `disallowed_methods`); everything else receives `EnvOverrides`.

## Exit codes (PRD §14.3, `error.rs`)

0 success (also `--dry-run`) · 1 generic (`ERROR`, `IO`) · 2 usage · 3 auth (`AUTH_*`) · 4 `SCOPE_MISSING` / `FORBIDDEN` · 5 not found · 6 validation (400/409/422) · 7 rate limited with `fail` · 8 bulk partial failure (reserved) · 9 server error after retries · 10 `CONFIG` / `CREDENTIAL_STORE` · 11 network/TLS · 12 pagination limit · 13 job timeout (reserved) · 130 SIGINT (nothing printed).

## Adding a curated command

1. `crates/zdk-core/src/api/curated/<resource>.rs` — a `RequestSpec` builder against the registry op (`spec(Method, path, "OperationId")`), plus any envelope/unwrap helpers. New resource shape → `models/<resource>.rs`.
2. `crates/zdk-cli/src/cmd/<resource>.rs` — clap subcommand + handler building the request from `AppContext`; reuse `args/` helpers (`resource_body`, `TicketFilters`, `UserRef`, `read_text_source`). Mutating commands honour `--dry-run` and `--idempotency-key`; destructive ones go through the confirmation helper.
3. `crates/zdk-core/src/auth/scopes.rs` — add the `COMMAND_SCOPES` entry (a test asserts every entry names catalogue scopes).
4. `crates/zdk-core/src/output/table.rs` — a column preset if the command lists.
5. Tests in `crates/zdk-cli/tests/resources_cli.rs` (happy path against wiremock, one error path, `--help` snapshot via `resource_help_snapshots`), then `cargo insta test --accept` and review the `.snap`.
6. `README.md` Capabilities (the docs check extracts every backticked `zdk …` and resolves it against `--help-json`), `CHANGELOG.md` `[Unreleased]`.

Read-only endpoints not worth curating are reachable through `zdk api`; do not add a curated command without filters or a table preset that earn it. Unimplemented PRD flags are **not defined**, so they fail with exit 2 (a test in `cli.rs` pins `--all-profiles`, `--resolve-names`, `--no-cache` as absent).

## Release gotchas

- A release is `[workspace.package] version` bump on `main` + `## [X.Y.Z]` in `CHANGELOG.md` + `cargo update --workspace` (the guard checks every member's `Cargo.lock` entry). Members use `version.workspace = true`; never set a member version.
- `auto-tag.yml` pushes the tag with the `HOMEBREW_TAP_TOKEN` PAT. Never `gh workflow run release.yml` to *create* a release; `-f tag=vX.Y.Z` only re-publishes an existing tag.
- The `homebrew`/`scoop` polls can fail a release after the GitHub Release exists. Re-run the downstream workflow (`gh workflow run update-zendesk-cli-formula.yml -R osodevops/homebrew-tap -f tag=…` / `gh workflow run update-manifest.yml -R osodevops/scoop-bucket -f tag=… -f product=zdk`) then `gh run rerun <id> --failed`. Never delete a release users may have installed; ship a patch version.
- Runner labels are explicit (`macos-15`, `macos-15-intel`, `ubuntu-24.04-arm`, `windows-11-arm`, …). Check GitHub's deprecations quarterly; `macos-14` retires 2 Nov 2026.
- musl targets must stay fully static (`ring` TLS provider, pure-Rust keyring backends); the build job `file`-checks it. Adding a crate with a C dependency can break this silently until release day — check with `cross build --target x86_64-unknown-linux-musl` locally (with rustup's cargo first on PATH).
- Windows: `crates/zdk-cli/build.rs` reserves an 8 MiB main-thread stack; completions/man run on a 16 MiB thread; `panic = "unwind"` so a panic exits 1 instead of aborting.
- Third-party actions are SHA-pinned with a version comment; resolve with `gh api repos/<o>/<r>/git/ref/tags/<tag>` (dereference annotated tags). Dependabot keeps them fresh.
- Full runbook and rollback table: `docs/release-process.md`.

## Gotchas

- `cargo +1.88.0` does not work on this Mac (Homebrew cargo first on PATH); use `rustup run 1.88.0 cargo …`, and `export PATH="$HOME/.cargo/bin:$PATH"` before `cross` or any `--target` build.
- macOS keychain grants are bound to the exact binary signature, so every unsigned local build prompts (or fails) when it touches stored tokens. Keep `ZENDESK_CREDENTIAL_STORE=none` (or `file`) for local runs unless you are testing the keyring on purpose.
- `RUSTFLAGS=-D warnings` and `--locked` in CI: dead code, unused imports, and a `Cargo.lock` that needs updating all fail.
- Snapshots: `--help` text, tables, the transcript and `api describe` are insta snapshots under `crates/zdk-cli/tests/snapshots/`. Any wording change needs `cargo insta test --accept` (then review the diff) or CI fails. The harness pins `COLUMNS=100` and `NO_COLOR=1` so snapshots are stable.
- `typos` runs in CI over everything except `specs/`, `api/generated/`, `tests/fixtures/`, `Cargo.lock` and the PRD (`.typos.toml`); British spellings used in help text (`colour`, `honoured`) are fine, real typos are not.
- `cargo xtask codegen --check` fails if the registry is stale; run `cargo xtask codegen` after touching `specs/`, `xtask/`, `scope_map.toml` or `overrides.toml`. xtask has no `man`/`completions` subcommands — those are `zdk man --to DIR` and `zdk completions <shell>` on the built binary.
- `openapiv3` is deliberately not used: the Zendesk specs have quirks (undefined `oauth2` scheme, `deepObject` on `oneOf`, a bare `=` scalar) that a strict parser rejects. The walker is `serde_yaml_ng::Value` → `serde_json::Value`.
- `oauth2` crate is deliberately not used (it pins reqwest 0.12; we are on 0.13). OAuth token POSTs are hand-rolled in `auth/oauth.rs` with `_at(base_url)` seams for wiremock.
- `operationId`s collide across specs (`ListLocales`, `ShowComment`); registry identity is `(spec, id)`, displayed as `support.ListTickets`.
- Search (`GET /api/v2/search`) is offset-only and capped at 1,000 results; ticket filters that need more route through `search/export` (`tickets list --all` does this).
- `--dry-run` short-circuits before the auth provider runs: `me`/email lookups are skipped (a stderr note says so) and the rendered `Authorization` header is always `Bearer [redacted]`, even for API-token auth, whose deprecation warning therefore does not fire on a dry run.
- Global flag names are reserved (`-o/--output` is the format selector, `--all` is auto-paginate); never reuse them for command-specific meanings.
- Coverage floor is 70 % lines (generated code and xtask excluded); it rises 5 points per minor toward the PRD's 80 %.
