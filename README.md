# zdk — Zendesk CLI for AI Agents and Support Operations (Rust)

A fast, single-binary CLI that gives AI agents, scripts and support engineers structured, rate-limit-aware access to the [Zendesk](https://www.zendesk.com) Support API — **OAuth-first**, because Zendesk stops issuing API tokens on **27 October 2026** and switches the remaining ones off on **30 April 2027**.

Every command emits plain JSON when piped, uses deterministic exit codes, keeps stdout clean, and never stores a credential in plaintext. Not a chatbot. A tool that agents wield.

[![CI](https://github.com/osodevops/zendesk-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/osodevops/zendesk-cli/actions/workflows/ci.yml)
[![Release](https://github.com/osodevops/zendesk-cli/actions/workflows/release.yml/badge.svg)](https://github.com/osodevops/zendesk-cli/releases)
[![Latest Release](https://img.shields.io/github/v/release/osodevops/zendesk-cli)](https://github.com/osodevops/zendesk-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> **Status:** pre-release. The v0.1.0 milestone described below is being built now; commands marked *roadmap* do not exist yet. See [Roadmap](#roadmap).

## Why This Exists

Support teams and the agents that help them need to read, triage, update and export tickets from scripts, pipelines and LLM tool calls. The existing options either assume a browser, depend on the API tokens Zendesk is retiring, ignore the per-endpoint rate limits that make bulk work fail halfway, or return output no program can parse.

**zdk** is a complete, headless, machine-readable interface to Zendesk that any agent can call as a subprocess: OAuth with PKCE by default, client credentials for CI, a rate governor that learns the account's real quota from response headers, cursor pagination that resumes, and an `api` escape hatch backed by an operation registry generated from Zendesk's own OpenAPI specs (887 operations across Support, Help Center and Talk).

## Agent Integration Contract

**stdout is data.** When stdout is not a TTY (or with `-o json`), a command prints the plain JSON payload — an array for lists, an object for single resources — with **no envelope**, so `jq` works directly:

```bash
zdk tickets list --status open --limit 50 | jq '.[].id'
zdk tickets get 1234 | jq '.status'
zdk tickets list -o ndjson | while IFS= read -r t; do …; done   # one object per line, streamed
zdk tickets list -o raw                                         # untouched Zendesk response body
```

**stderr is everything else**: progress, warnings, prompts, rate-limit notices, logs. In machine output modes an error is a single JSON line on stderr, and the process exits with a stable code:

```json
{"error":{"code":"not_found","message":"ticket 999999 was not found","help":"Check the id, or use `zdk search 'subject:…'` if this looks like a title.","exit_code":5,"request_id":"5f3b…"}}
```

In table mode (a TTY) the same error is a readable [miette](https://github.com/zkat/miette) report. Error `code` strings are stable and listed in `crates/zdk-core/src/error.rs`.

**Exit codes** (the contract agents branch on):

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic error |
| 2 | Usage / invalid arguments (nothing was sent) |
| 3 | Authentication failure (not logged in, token expired or revoked, client secret required) |
| 4 | Authorisation failure / missing OAuth scope |
| 5 | Resource not found |
| 6 | Validation error (400/409/422 from Zendesk) |
| 7 | Rate limited and `--rate-limit-strategy fail` |
| 8 | Bulk operation partial failure (reserved; bulk commands arrive in v0.3) |
| 9 | Zendesk server error after retries |
| 10 | Local configuration or credential-store error |
| 11 | Network / TLS error |
| 12 | Pagination limit reached (offset pagination cap; the message names the alternative) |
| 13 | Job timed out (reserved; jobs arrive in v0.3) |
| 130 | Interrupted (SIGINT); a `--checkpoint` file is written first |

`zdk --help-json` prints the whole command tree as JSON for tool-definition generation.

## Install

```bash
# Homebrew (macOS/Linux) — available once v0.1.0 is released
brew install osodevops/tap/zendesk-cli

# Scoop (Windows) — available once v0.1.0 is released
scoop bucket add osodevops https://github.com/osodevops/scoop-bucket
scoop install zdk

# Pre-built binaries — https://github.com/osodevops/zendesk-cli/releases
# Every archive is attested with GitHub build provenance and listed in checksums-sha256.txt:
gh attestation verify zdk-v0.1.0-aarch64-apple-darwin.tar.gz --owner osodevops
sha256sum -c --ignore-missing checksums-sha256.txt

# From source (Rust 1.88+)
cargo install --git https://github.com/osodevops/zendesk-cli zendesk-cli
```

Release archives contain `bin/zdk`, man pages, shell completions and the docs. Targets: macOS (Apple Silicon, Intel), Linux (x86_64 and aarch64 static musl, x86_64 glibc), Windows (x64, ARM64).

## Documentation

- `zdk --help`, `zdk <command> --help`, and the man pages (`zdk man --to DIR`)
- [docs/oauth-migration.md](docs/oauth-migration.md) — the API-token deadlines and how to move to OAuth
- [docs/release-process.md](docs/release-process.md) — how releases are cut and rolled back
- [docs/zendesk-cli-prd.md](docs/zendesk-cli-prd.md) — the product requirements document (full roadmap)
- [CHANGELOG.md](CHANGELOG.md)

## Quickstart

```bash
# 1. Sign in (opens a browser; PKCE, no client secret needed for a Public OAuth client)
zdk auth login --subdomain acme --client-id zdk_local --preset agent

# 2. Confirm who you are and what you can do
zdk auth whoami
zdk auth status

# 3. Read something
zdk tickets list --limit 3            # table on a TTY
zdk tickets list --limit 3 | jq .     # JSON when piped
```

You need an OAuth client from Admin Center → *Apps and integrations* → *APIs* → *OAuth clients*. `docs/oauth-migration.md` walks through creating one.

## Authentication

OAuth is the default and the documented path. API-token auth exists only as a migration ramp and warns on every use.

```bash
# Authorization code + PKCE (interactive default). Loopback listener on 127.0.0.1, single use.
zdk auth login --subdomain acme --client-id zdk_local --scopes tickets:read,tickets:write,users:read
zdk auth login --port 9876                       # fixed loopback port for clients registered with one
zdk auth login --no-browser                      # print the URL, paste the code back (SSH, containers)
zdk auth login --redirect-uri https://localhost  # copy the code from the address bar

# Client credentials (CI / automation). Confidential clients only; no refresh token — zdk re-mints on expiry.
ZENDESK_CLIENT_SECRET=… zdk auth login --client-credentials --subdomain acme --client-id zdk_ci --scopes tickets:read,users:read

# API token (legacy, deprecated). Warns on every call with the days left until 30 Apr 2027.
zdk auth login --api-token --subdomain acme --email me@acme.com --token "$ZENDESK_API_TOKEN"
```

Also: `zdk auth status`, `zdk auth whoami`, `zdk auth refresh [--force]`, `zdk auth logout [--all] [--no-revoke]`, `zdk auth test`, `zdk auth token`, and scope tooling:

```bash
zdk auth scopes list                    # the granular catalogue
zdk auth scopes check tickets update    # "requires tickets:write — you have tickets:read"
zdk auth scopes preset readonly         # every *:read; also: agent, admin, exporter
```

Tokens are stored in the OS keychain when one is available, otherwise in an encrypted file (`credentials.enc`, XChaCha20-Poly1305 with an Argon2id- or machine-key-derived key, mode 0600) — chosen automatically in Docker, CI and SSH sessions. Force a backend with `--credential-store keyring|file|env|none`; `ZENDESK_ACCESS_TOKEN` bypasses the store entirely. Refresh happens pre-emptively at 80 % of the token lifetime, and refresh-token rotation is persisted before the new token is used.

## Agent Workflow Patterns

```bash
# Read → decide → act
zdk tickets list --status new --unassigned --limit 20 | my-agent-triage | \
  jq -r '.[] | "\(.id) \(.assignee)"' | while read -r id who; do zdk tickets assign "$id" --to "$who"; done

# Preview every write before it happens (consumes no quota)
zdk tickets update 1234 --status solved --dry-run

# Stream a large export without buffering
zdk tickets list --all --updated-after 24h -o ndjson --checkpoint /tmp/tickets.ckpt > tickets.ndjson

# Branch on exit codes
zdk tickets reply 1234 --body "On it."
case $? in
  0) echo sent ;;
  3) zdk auth login && echo "re-auth needed" ;;
  4) echo "missing scope: run zdk auth scopes check tickets update" ;;
  7) sleep 60 ;;   # rate limited with --rate-limit-strategy fail
esac

# Anything the curated commands do not cover yet
zdk api GET /api/v2/views --paginate | jq '.[].title'
```

## Capabilities

Commands below ship in **v0.1.0**. Everything else is [roadmap](#roadmap).

### Authentication (`zdk auth`)

```bash
zdk auth login [--client-credentials | --api-token] [--no-browser] [--port N] [--scopes …] [--preset agent|admin|readonly|exporter]
zdk auth status | whoami | refresh | logout | test | token
zdk auth scopes list | show | check <command…> | preset <name>
```

### Tickets (`zdk tickets`)

```bash
zdk tickets list --status open --priority urgent --assignee me --updated-after 2h
zdk tickets get 1234 --with-comments          # or: zdk tickets show 1234 (transcript)
zdk tickets create --subject "Printer on fire" --comment "It is." --requester jane@acme.com --priority high --tag hardware
zdk tickets update 1234 --status pending --add-tag waiting --custom-field 360001234=eu
zdk tickets reply 1234 --body "Fixed in v2.3" ; zdk tickets note 1234 --body-file notes.md
zdk tickets solve | close | reopen | assign 1234 --to me
zdk tickets count --status open ; zdk tickets recent
zdk tickets delete 1234 --yes ; zdk tickets restore 1234 ; zdk tickets permanently-delete 1234 --yes
```

Filters compile to a Zendesk search query (`zdk search explain --status open --unassigned` shows it). `--all` switches to the export endpoint so results are not capped at 1 000.

### Comments (`zdk comments`)

```bash
zdk comments list 1234 --public-only
zdk comments get 1234 --comment 98765 ; zdk comments count 1234
zdk comments make-private 1234 --comment 98765
zdk comments redact 1234 --comment 98765 --text "4111 1111 1111 1111" --yes
```

### Users (`zdk users`)

```bash
zdk users list --role agent --role admin ; zdk users me ; zdk users get jane@acme.com
zdk users search "jane" ; zdk users autocomplete jan ; zdk users related 4567
zdk users create --field name="Jane Doe" --field email=jane@acme.com --field role=end-user
zdk users create-or-update --file user.json ; zdk users update 4567 --field phone=… ; zdk users delete 4567 --yes
```

### Organizations (`zdk orgs`)

```bash
zdk orgs list ; zdk orgs get 890 ; zdk orgs count ; zdk orgs search --name "Acme"
zdk orgs create --name "Acme Corp" --domain acme.com --tag enterprise
zdk orgs update 890 --field notes="Renewal Q4" ; zdk orgs delete 890 --yes
zdk orgs tickets 890 ; zdk orgs users 890 ; zdk orgs related 890
```

### Search (`zdk search`)

```bash
zdk search 'type:ticket status:open priority:urgent' --sort-by updated_at --sort-order desc
zdk search count 'type:user role:agent'
zdk search export 'type:ticket created>2026-01-01' --filter-type ticket -o ndjson --to tickets.ndjson
zdk search explain --status open --assignee me     # prints the query the ticket filters compile to
```

### API escape hatch (`zdk api`)

Any Zendesk endpoint, with auth, rate limiting, retries, pagination and scope preflight applied. Never refuses an unknown path.

```bash
zdk api GET /api/v2/tickets/1234/audits --paginate --limit 500 -o ndjson
zdk api POST /api/v2/tickets --data @ticket.json ; zdk api PUT /api/v2/users/4567 --field user.name="Jane"
zdk api ops --grep views --method GET ; zdk api describe ListTickets --schema
```

### Configuration, shell integration, diagnostics

```bash
zdk config init | show [--effective] | get <key> | set <key> <value> | edit | validate | path
zdk config profiles list | add | remove | rename | switch
zdk completions bash|zsh|fish|powershell|elvish
zdk man --to ./man
zdk doctor [--json]      # store backend, token expiry, clock skew, connectivity
zdk version              # build and spec-snapshot versions
```

## Resilience & Rate Limiting

Zendesk enforces an account-wide budget (200–2 500 requests/min by plan) **and** per-endpoint limits that are easy to hit in bulk work (100 ticket updates/min, 30 per ticket per 10 min; 5 user updates/min per user; 10 incremental-export calls/min; …). `zdk` handles both:

- **Governor**: a token bucket that starts conservative and learns the real quota from `X-Rate-Limit` / `ratelimit-*` headers, with a separate budget for Help Center paths and static per-endpoint rules keyed by ticket, user or organization. `--rate-limit-strategy wait` (default) sleeps, `fail` exits 7 immediately, `burst` uses the reserve.
- **Retries**: `Retry-After` (seconds or HTTP date) is honoured on 429; 5xx and transport errors back off with full jitter (6 attempts, 1 s base, 60 s cap) — configurable with `--retries`, `--timeout` and `[retry]` in the config file.
- **Idempotency**: every POST/PUT/PATCH carries an `Idempotency-Key` that is stable across retries; set your own with `--idempotency-key`.
- **Pagination**: cursor pagination by default with `--all`, `--limit`, `--page-size` (≤ 100) and `--checkpoint FILE` for resumable exports. Offset-only endpoints stop *before* Zendesk's page-100 / 10 000-record wall with exit 12 and the name of the cursor or export alternative.
- **Dry runs**: `--dry-run` prints the exact request (JSON or a `curl` line) and sends nothing.
- **Concurrency**: `--max-concurrency` caps in-flight requests.

## Multi-Profile Support

```bash
zdk --profile production tickets list
zdk --profile sandbox tickets create --subject "test" --comment "…" --requester me
zdk config profiles list
```

Config file: `~/.config/zendesk-cli/config.toml` (Linux), `~/Library/Application Support/zendesk-cli/config.toml` (macOS), `%APPDATA%\zendesk-cli\config.toml` (Windows); override with `--config` or `ZENDESK_CONFIG`. **No secret is ever a config field.**

```toml
[default]
active_profile = "production"
output = "table"          # table | json | ndjson | csv | tsv | yaml | raw
page_size = 100
confirm_destructive = true

[auth]
refresh_at_percent = 80
credential_store = "auto" # auto | keyring | file | env
suppress_deprecation = false

[rate_limit]
strategy = "wait"         # wait | fail | burst
max_concurrency = 4
reserve_percent = 10

[retry]
max_attempts = 6
base_ms = 1000
max_ms = 60000

[profiles.production]
subdomain = "acme"
client_id = "zdk_production"
grant_type = "authorization_code"
scopes = ["tickets:read", "tickets:write", "users:read", "organizations:read"]

[profiles.ci]
subdomain = "acme"
client_id = "zdk_ci"
grant_type = "client_credentials"
scopes = ["tickets:read", "users:read"]
```

Destructive commands print the active profile in their confirmation prompt so a production instance is never mistaken for a sandbox; `--yes` skips the prompt.

## Environment Variables

Precedence: CLI flag → environment variable → profile → `[default]` → built-in default.

| Variable | Description |
|---|---|
| `ZENDESK_SUBDOMAIN` | Instance subdomain (`acme` for `acme.zendesk.com`) |
| `ZENDESK_CLIENT_ID` | OAuth client identifier |
| `ZENDESK_CLIENT_SECRET` | OAuth client secret (confidential clients / client credentials) |
| `ZENDESK_ACCESS_TOKEN` | Direct access token; bypasses the credential store |
| `ZENDESK_REFRESH_TOKEN` | Refresh token for non-interactive refresh alongside `ZENDESK_ACCESS_TOKEN` |
| `ZENDESK_SCOPES` | Comma-separated granular scopes for `auth login` |
| `ZENDESK_EMAIL` | Legacy API-token auth: agent email |
| `ZENDESK_API_TOKEN` | Legacy API-token auth: the token (deprecated; dead after 30 Apr 2027) |
| `ZENDESK_PROFILE` | Active profile (overridden by `--profile`) |
| `ZENDESK_CONFIG` | Config file path (overridden by `--config`) |
| `ZENDESK_OUTPUT` | Default output format |
| `ZENDESK_PAGE_SIZE` | Cursor page size (max 100) |
| `ZENDESK_MAX_CONCURRENCY` | In-flight request ceiling |
| `ZENDESK_RATE_LIMIT_STRATEGY` | `wait` \| `fail` \| `burst` |
| `ZENDESK_CREDENTIAL_STORE` | `auto` \| `keyring` \| `file` \| `env` \| `none` (`none` is what tests and CI use) |
| `ZENDESK_CREDENTIALS_PASSPHRASE` | Passphrase for the encrypted file store (otherwise a machine key file is used) |
| `ZENDESK_LOG` | Tracing filter, e.g. `zdk=debug,reqwest=info` |
| `NO_COLOR` | Disable colour (also `--no-color`; colour is off whenever stdout is not a TTY) |

## Verbosity

```bash
zdk tickets list              # warnings only (stderr)
zdk -v tickets list           # info
zdk -vv tickets list          # debug
zdk -vvv tickets list         # trace: full request/response with secrets redacted
zdk -q tickets list           # suppress non-error stderr (also silences the API-token warning)
zdk --audit-log audit.ndjson tickets list   # append a structured record of every API call
```

Logs never go to stdout.

## Shell Completions

```bash
zdk completions bash > /etc/bash_completion.d/zdk       # or: >> ~/.bashrc
zdk completions zsh  > ~/.zfunc/_zdk                     # fpath+=(~/.zfunc); autoload -Uz compinit
zdk completions fish > ~/.config/fish/completions/zdk.fish
zdk completions powershell > zdk.ps1
zdk completions elvish > zdk.elv
```

Homebrew and the release archives install completions for bash, zsh and fish automatically.

## Man Pages

Man pages are generated from the clap definitions with `clap_mangen`, so they never drift from `--help`:

```bash
zdk man --to ./man && man ./man/zdk.1
```

Release archives ship them under `share/man/man1/`, and the Homebrew formula installs them.

## Roadmap

v0.1.0 (above) is the OAuth-before-the-deadline milestone. Later milestones from the PRD, in order — **none of these exist yet**:

| Version | Scope |
|---|---|
| v0.2 | views, groups, macros, triggers, automations, SLA, fields, forms, statuses, tags, suspended tickets, requests, CSAT; crates.io and ghcr.io publishing |
| v0.3 | job orchestration, `bulk-*`, attachments, side conversations, schema resolver (`--resolve-names`, name-based `--group`/`--custom-field "Title=…"`) |
| v0.4 | incremental sync engine, `zdk backup`, Help Center and community |
| v0.5 | configuration-as-code: `rules export/plan/apply/diff/promote` |
| v0.6 | Talk, Chat, omnichannel routing, custom objects, webhooks, ZIS |
| v0.7 | GDPR/redaction workflows, audit logs, `zdk auth migrate` |
| v0.8 | generated coverage for every remaining resource group |
| v0.9 | MCP server mode |
| v1.0 | stable command surface |

## Development

```bash
export ZENDESK_CREDENTIAL_STORE=none                 # never touch the OS keyring from tests
cargo build                                          # debug build of the workspace
cargo run -p zendesk-cli -- --help                   # run the zdk binary
cargo test --workspace --all-targets --locked        # unit + wiremock integration + black-box CLI tests
cargo insta review                                   # review changed snapshots
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
rustup run 1.88.0 cargo check --workspace --all-targets --locked   # MSRV
cargo xtask codegen --check                          # generated registry is fresh
cargo xtask spec-refresh && cargo xtask spec-diff    # pull new Zendesk OpenAPI snapshots
cargo deny check bans licenses sources advisories
```

Workspace: `crates/zdk-core` (library: config, auth, credential store, HTTP core, governor, pagination, generated operation registry, curated API, output), `crates/zdk-cli` (the `zdk` binary; package `zendesk-cli`), `xtask` (codegen, spec refresh/diff, man/completions). See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Contributing

We welcome issues and PRs. Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Security

See [SECURITY.md](SECURITY.md) for our security policy and how to report vulnerabilities. Credentials never touch disk in plaintext, secrets are redacted from logs and `--audit-log`, TLS 1.2+ is enforced, and the binary talks to Zendesk and nothing else (no telemetry).

## License

MIT — see [LICENSE](LICENSE).
