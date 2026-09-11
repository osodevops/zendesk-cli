# zdk — Zendesk CLI for AI Agents and Support Operations (Rust)

A fast, single-binary CLI that gives AI agents, scripts and support engineers structured, rate-limit-aware access to the [Zendesk](https://www.zendesk.com) Support API — **OAuth-first**, because Zendesk stops issuing API tokens on **27 October 2026** and switches the remaining ones off on **30 April 2027**.

Every command emits plain JSON when piped, uses deterministic exit codes, keeps stdout clean, and never stores a credential in plaintext. Not a chatbot. A tool that agents wield.

[![CI](https://github.com/osodevops/zendesk-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/osodevops/zendesk-cli/actions/workflows/ci.yml)
[![Release](https://github.com/osodevops/zendesk-cli/actions/workflows/release.yml/badge.svg)](https://github.com/osodevops/zendesk-cli/releases)
[![Latest Release](https://img.shields.io/github/v/release/osodevops/zendesk-cli)](https://github.com/osodevops/zendesk-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> **Status:** v0.1.0 is the first release: OAuth login, credential stores, the rate governor, cursor pagination, the `api` escape hatch and curated `tickets` / `comments` / `users` / `orgs` / `search` commands. Everything under [Roadmap](#roadmap) does not exist yet.

## Why This Exists

Support teams and the agents that help them need to read, triage, update and export tickets from scripts, pipelines and LLM tool calls. The existing options either assume a browser, depend on the API tokens Zendesk is retiring, ignore the per-endpoint rate limits that make bulk work fail halfway, or return output no program can parse.

**zdk** is a headless, machine-readable interface to Zendesk that any agent can call as a subprocess: OAuth with PKCE by default, client credentials for CI, a rate governor that learns the account's real quota from response headers, cursor pagination that resumes, and an `api` escape hatch backed by an operation registry generated from Zendesk's own OpenAPI specs (887 operations: Support 645, Help Center 182, Talk 60).

## Agent Integration Contract

**stdout is data.** When stdout is not a TTY (or with `-o json`), a command prints the plain JSON payload — an array for lists, an object for single resources — with **no envelope**, so `jq` works directly:

```bash
zdk tickets list --status open --limit 50 | jq '.[].id'
zdk tickets get 1234 | jq '.status'
zdk tickets list -o ndjson | while IFS= read -r t; do …; done   # one object per line, streamed
zdk tickets list -o raw                                         # untouched Zendesk response body
zdk tickets list --fields id,subject,status --jq '.[] | .id'    # projection and an embedded jq filter
```

Formats: `table` (default on a terminal), `json` (default when piped), `ndjson`, `csv`, `tsv`, `yaml`, `raw`. Selection order: `-o` → `ZENDESK_OUTPUT` → `[default].output` in the config file → TTY detection.

**stderr is everything else**: progress, warnings, prompts, rate-limit notices, logs. In machine output modes an error is a single JSON line on stderr, and the process exits with a stable code. This is exactly what `zdk tickets get 1234 -o json` prints before logging in:

```json
{"error":{"code":"AUTH_NOT_LOGGED_IN","message":"not authenticated for profile 'default'","help":"Run `zdk auth login` (browser), `zdk auth login --no-browser` (SSH), or `zdk auth login --client-credentials` (CI).","exit_code":3,"request_id":null}}
```

`help` and `request_id` are `null` when there is nothing to say; `request_id` is Zendesk's correlation id whenever the failure came from an HTTP response. In table mode (a TTY) the same error is a readable [miette](https://github.com/zkat/miette) report. The `code` strings are stable (`crates/zdk-core/src/error.rs`):

| `code` | Exit | When |
|---|---|---|
| `USAGE` | 2 | Bad flag or value; a destructive command without `--yes` on a non-TTY (nothing was sent) |
| `AUTH_NOT_LOGGED_IN`, `AUTH_EXPIRED`, `AUTH_REVOKED`, `AUTH_CLIENT_SECRET_REQUIRED`, `AUTH_INVALID_SCOPE`, `AUTH_TOKEN_ENDPOINT`, `AUTH_CODE_EXPIRED`, `AUTH_FLOW_ABORTED` | 3 | No usable credential; refresh failed; the OAuth client rejected a scope; the login flow failed |
| `SCOPE_MISSING` | 4 | The token's granted scopes are known and lack the one the command needs (checked locally, before any HTTP) |
| `FORBIDDEN` | 4 | Zendesk answered 403 (or grants are unknown, e.g. a static or API token) |
| `NOT_FOUND` | 5 | 404, or an email that matches no user |
| `VALIDATION` | 6 | Zendesk rejected the request (400 / 409 / 422); field details are in `message` |
| `RATE_LIMITED` | 7 | A budget is exhausted and `--rate-limit-strategy fail` is set |
| `PARTIAL_FAILURE` | 8 | Reserved for bulk commands (none ship yet) |
| `SERVER_ERROR` | 9 | 5xx after every retry |
| `CONFIG`, `CREDENTIAL_STORE` | 10 | Unparsable config, bad `ZENDESK_*` value, keyring / encrypted-file failure, refresh-token rotation that could not be saved |
| `NETWORK` | 11 | Transport or TLS failure |
| `PAGINATION_LIMIT` | 12 | An offset walk would cross Zendesk's 100-page / 10,000-record wall; `help` names the alternatives |
| `JOB_TIMEOUT` | 13 | Reserved for job orchestration (none ships yet) |
| `INTERRUPTED` | 130 | Ctrl-C (nothing is printed; a `--checkpoint` file, if given, holds the last completed page) |
| `IO`, `ERROR` | 1 | Anything else (including a failed `zdk doctor` run) |

`DRY_RUN` exists as a code but maps to exit 0: `--dry-run` prints the request instead of sending it.

**Exit codes** (the contract agents branch on), from `zdk --help-json`:

| Code | Name | Meaning |
|---|---|---|
| 0 | `OK` | success (also `--dry-run`) |
| 1 | `ERROR` | unexpected error |
| 2 | `USAGE` | usage / argument error |
| 3 | `AUTH` | authentication failed or missing |
| 4 | `FORBIDDEN` | forbidden or missing scope |
| 5 | `NOT_FOUND` | resource not found |
| 6 | `VALIDATION` | Zendesk rejected the request (400/409/422) |
| 7 | `RATE_LIMITED` | rate limit exhausted (or `--rate-limit-strategy fail`) |
| 8 | `PARTIAL_FAILURE` | bulk operation completed with failures |
| 9 | `SERVER_ERROR` | Zendesk 5xx after retries |
| 10 | `CONFIG` | configuration or credential-store error |
| 11 | `NETWORK` | network / TLS error |
| 12 | `PAGINATION_LIMIT` | offset pagination cap reached |
| 13 | `JOB_TIMEOUT` | job did not finish in time |
| 130 | `INTERRUPTED` | interrupted (ctrl-c) |

`zdk --help-json` prints the whole command tree — every command's `path`, `summary`, `usage` and `flags`, plus `global_flags` and `exit_codes` once at the root — as JSON for tool-definition generation. A closed stdout pipe (`| head`) is normal termination (exit 0).

## Install

```bash
# Homebrew (macOS/Linux)
brew install osodevops/tap/zendesk-cli

# Scoop (Windows)
scoop bucket add osodevops https://github.com/osodevops/scoop-bucket
scoop install zdk

# Pre-built binaries — https://github.com/osodevops/zendesk-cli/releases
# Every archive is attested with GitHub build provenance and listed in checksums-sha256.txt:
gh attestation verify zdk-v0.1.0-aarch64-apple-darwin.tar.gz --owner osodevops
sha256sum -c --ignore-missing checksums-sha256.txt

# From source (Rust 1.88+)
cargo install --git https://github.com/osodevops/zendesk-cli zendesk-cli
```

Release archives (`zdk-<tag>-<target>.tar.gz`, `.zip` on Windows) contain `bin/zdk`, `share/man/man1/*.1`, `share/completions/*` and `share/doc/zendesk-cli/`. Targets: macOS (Apple Silicon, Intel), Linux (x86_64 and aarch64 static musl, x86_64 glibc), Windows (x64, ARM64).

## Documentation

- `zdk --help`, `zdk <command> --help`, `zdk --help-json`, and the man pages (`zdk man --to DIR`)
- [docs/oauth-migration.md](docs/oauth-migration.md) — the API-token deadlines, creating an OAuth client, and how to move
- [docs/recipes.md](docs/recipes.md) — short, copy-pasteable agent and shell recipes
- [docs/release-process.md](docs/release-process.md) — how releases are cut and rolled back
- [docs/zendesk-cli-prd.md](docs/zendesk-cli-prd.md) — the product requirements document (full roadmap)
- [CHANGELOG.md](CHANGELOG.md)

## Quickstart

You need an OAuth client from Admin Center → *Apps and integrations* → *APIs* → *OAuth clients* (a **Public** client with redirect URL `http://127.0.0.1/callback` for interactive use; [docs/oauth-migration.md](docs/oauth-migration.md) walks through it).

```bash
# 1. Write a config file for your instance (prompts for anything missing; --non-interactive in scripts)
zdk config init --subdomain acme --client-id zdk_local

# 2. Sign in (opens a browser; PKCE, no client secret needed for a Public client)
zdk auth login

# 3. Confirm who you are
zdk auth whoami

# 4. Read something
zdk tickets list --limit 3            # table on a TTY
zdk tickets list --limit 3 | jq .     # JSON when piped
```

`config init` writes a profile named `default` whose scopes are `tickets:read`, `tickets:write`, `users:read`, `organizations:read`; `auth login` requests the profile's scopes (falling back to the `agent` preset when a profile has none).

## Authentication

OAuth is the default and the documented path. API-token auth exists only as a migration ramp and warns on every use.

```bash
# 1. Authorization code + PKCE (interactive default). A single-use loopback listener on
#    http://127.0.0.1:<ephemeral port>/callback; `state` is verified; the code is exchanged at once.
zdk auth login --client-id zdk_local --scopes tickets:read,tickets:write,users:read
zdk auth login --preset agent                    # a named scope bundle (repeatable; combines with --scopes)
zdk auth login --port 9876                       # fixed loopback port for clients registered with one
zdk auth login --no-browser                      # print the URL; paste the code or the full redirect URL back
                                                 #   (redirect http://127.0.0.1:8484/callback unless --port / auth.callback_port)
zdk auth login --redirect-uri https://localhost  # paste-from-address-bar flow with a registered redirect
zdk auth login --expires-in 3600                 # requested access-token lifetime (300–172800 s)

# 2. Client credentials (CI / automation). Confidential clients only; no refresh token — zdk re-mints on expiry.
ZENDESK_CLIENT_SECRET=… zdk auth login --client-credentials --client-id zdk_ci --scopes tickets:read,users:read
zdk auth login --client-credentials --client-secret "$SECRET" --store-secret   # keep the secret in the credential store

# 3. API token (legacy, deprecated).
zdk auth login --api-token --email me@acme.com --token "$ZENDESK_API_TOKEN"    # defaults: ZENDESK_EMAIL, ZENDESK_API_TOKEN
```

`--subdomain` and `--profile` are global flags (`zdk --profile ci auth login …`). Authorization codes are valid for 120 seconds; a late exchange is `AUTH_CODE_EXPIRED`. Requesting a scope outside the client's *Allowed scopes* is `AUTH_INVALID_SCOPE` and the help text names the Admin Center screen to fix it. An empty scope list is refused (Zendesk would grant full read+write).

Every API-token call prints one line on stderr, once per process, with the live countdown (today it reads: `warning: API token auth is deprecated. New tokens cannot be created after 27 Oct 2026 (46 days); all tokens stop working on 30 Apr 2027 (231 days). Run `zdk auth login` to switch to OAuth.`). After 27 October 2026 the wording becomes `Zendesk no longer issues new API tokens; all tokens stop working on 30 Apr 2027 (N days)`, and after 30 April 2027 `all API tokens stopped working on 30 Apr 2027 (N days ago)`. `--quiet` or `auth.suppress_deprecation = true` silences it.

After login:

```bash
zdk auth status                         # grant, scopes, expiry countdown, store backend (exit 3 when not logged in)
zdk auth whoami                         # GET /api/v2/users/me
zdk auth test                           # one cheap call per product API the credential can reach
zdk auth refresh --force                # refresh (authorization code) or re-mint (client credentials) now
zdk auth token --format bearer          # raw | bearer | json — the only command that prints a secret
zdk auth logout --no-revoke             # remove the local credential; without the flag the token is revoked server-side first
```

### Scopes

`zdk` ships Zendesk's 52-scope catalogue and four presets:

| Preset | Scopes |
|---|---|
| `agent` | `tickets:read`, `tickets:write`, `users:read`, `organizations:read`, `hc:read` |
| `admin` | `agent` plus `triggers:write`, `automations:write`, `macros:write`, `webhooks:write` |
| `readonly` | every granular `*:read` scope (25 of them; never the legacy blanket `read`) |
| `exporter` | `tickets:read`, `users:read`, `organizations:read`, `auditlogs:read` |

```bash
zdk auth scopes list                    # the catalogue: scope, family, access, description
zdk auth scopes preset agent            # a preset's scopes; --login re-authenticates with exactly that preset
zdk auth scopes show --preset admin     # granted scopes versus what a preset needs
zdk auth scopes check tickets update    # {"required":["tickets:write"],"granted":[…],"satisfied":…,"missing":[…]}
```

Every curated command maps to the scope it needs (`tickets update` → `tickets:write`, `orgs users` → `users:read`, `search` → the legacy `read` because unified search has no granular scope). When the token's granted scopes are known (OAuth), a missing one fails locally with exit 4 / `SCOPE_MISSING` before any request. With a static `ZENDESK_ACCESS_TOKEN` or an API token the grants are unknown, so the server decides and a 403 is `FORBIDDEN`. `zdk api` skips the check unless the registry knows the operation's scope; `--no-preflight` disables it.

### Where credentials live

Resolution order for every command: `ZENDESK_ACCESS_TOKEN` (used as-is, never refreshed) → `ZENDESK_EMAIL` + `ZENDESK_API_TOKEN` → the credential store entry for the active profile → exit 3.

The store is chosen by `--credential-store`, `ZENDESK_CREDENTIAL_STORE` or `[auth].credential_store`:

- `auto` (default): probe the OS keyring once (service `zendesk-cli`, one entry per profile); if it is unusable — Docker, CI, SSH sessions, a headless Linux with no Secret Service — fall back to the encrypted file. The decision is cached in `store-decision.json` under the state directory; `zdk auth login` and `zdk doctor` re-probe.
- `keyring`, `file`, `env` (read-only: the environment variables above), `none` (in-memory; what the tests and CI use).

The encrypted file is `credentials.enc` next to `config.toml`, XChaCha20-Poly1305, mode `0600`. Its key comes from `ZENDESK_CREDENTIALS_PASSPHRASE` (stretched with Argon2id, 64 MiB / 3 passes / 1 lane) when that variable is set, otherwise from a machine key file `credentials.key` (32 random bytes, `0600`) created beside it on first use. **Headless hosts** therefore either export `ZENDESK_CREDENTIALS_PASSPHRASE` in the job environment, or keep `credentials.key` alongside `credentials.enc` on the host that logged in. A file written in one mode refuses to open in the other and says which it needs.

Access tokens are refreshed pre-emptively once 80 % of their lifetime has elapsed (`[auth].refresh_at_percent`); Zendesk rotates refresh tokens, so the new pair is persisted before it is used and the old refresh token is never retried. If the new pair cannot be saved the running command completes and then exits 10 with `run zdk auth login`.

## Agent Workflow Patterns

```bash
# Read → decide → act
zdk tickets list --status new --unassigned --limit 20 | my-agent-triage | \
  jq -r '.[] | "\(.id) \(.assignee)"' | while read -r id who; do zdk tickets assign "$id" --to "$who"; done

# Preview every write before it happens (consumes no quota)
zdk tickets update 1234 --status solved --dry-run

# Stream a large export without buffering; resume after an interruption
zdk tickets list --all --updated-after 24h -o ndjson --checkpoint /tmp/tickets.ckpt > tickets.ndjson

# Branch on exit codes
zdk tickets reply 1234 --body "On it."
case $? in
  0) echo sent ;;
  3) zdk auth login && echo "re-auth needed" ;;
  4) echo "missing scope: run zdk auth scopes check tickets reply" ;;
  7) sleep 60 ;;   # rate limited with --rate-limit-strategy fail
esac

# Anything the curated commands do not cover
zdk api GET /api/v2/views --paginate | jq '.[].title'
```

`--dry-run` skips the lookups that turn `me` or an email into a user id (a stderr note says so); the real request carries the id.

## Capabilities

Commands below ship in **v0.1.0**. Everything else is [roadmap](#roadmap).

### Authentication (`zdk auth`)

```bash
zdk auth login [--client-credentials | --api-token] [--no-browser] [--port N] [--redirect-uri URI] [--scopes A,B] [--preset NAME]
zdk auth status ; zdk auth whoami ; zdk auth refresh [--force] ; zdk auth logout [--no-revoke] ; zdk auth test ; zdk auth token [--format raw|bearer|json]
zdk auth scopes list ; zdk auth scopes show [--preset NAME] ; zdk auth scopes check <command…> ; zdk auth scopes preset <name> [--login]
```

### Tickets (`zdk tickets`)

```bash
zdk tickets list --status open,pending --priority urgent --assignee me --updated-after 2h
zdk tickets list --unassigned --older-than 7d --tag vip --custom-field 360001234=eu
zdk tickets get 1234 --with-comments          # or: zdk tickets show 1234 (transcript on a TTY, ticket+comments as JSON)
zdk tickets create --subject "Printer on fire" --comment "It is." --requester jane@acme.com --priority high --tag hardware
zdk tickets create --file ticket.json --field priority=low --field custom_fields:='[{"id":360001234,"value":"eu"}]'   # --file/--from-stdin < typed flags < --field
zdk tickets update 1234 --status pending --add-tag waiting --custom-field 360001234=eu
zdk tickets update 1234 --status solved --safe-update --updated-stamp "$STAMP"   # 409 → exit 6 if it changed
zdk tickets reply 1234 --body "Fixed in v2.3" ; zdk tickets note 1234 --body-file notes.md
zdk tickets solve 1234 --body "Closing." ; zdk tickets close 1234 ; zdk tickets reopen 1234 ; zdk tickets assign 1234 --to me --group-id 501
zdk tickets count --status open ; zdk tickets recent
zdk tickets delete 1234 --yes ; zdk tickets restore 1234 ; zdk tickets permanently-delete 1234 --yes
```

`--file` accepts a bare object or `{"ticket": {…}}`; typed flags merge on top and `--field k=v|k:=json` (dots nest) is applied last, with keys relative to the resource object (`--field priority=low`, not `ticket.priority`). An unfiltered `tickets list` walks `GET /api/v2/tickets` with cursor pages. Any filter compiles to a Zendesk search query (`zdk search explain --status open --unassigned` prints it) and runs through `GET /api/v2/search`, which is offset-paginated and capped at 1,000 results; `--all` switches to `GET /api/v2/search/export` (cursor, uncapped). Time filters accept RFC 3339, `YYYY-MM-DD`, `24h`, `"2 hours ago"` and `yesterday`. Tag updates go through the dedicated `PUT …/tags` endpoint so other tags are untouched.

### Comments (`zdk comments`)

```bash
zdk comments list 1234 --public-only          # authors joined from the users sideload
zdk comments get 1234 --comment 98765 ; zdk comments count 1234
zdk comments make-private 1234 --comment 98765
zdk comments redact 1234 --comment 98765 --text "4111 1111 1111 1111" --yes
```

### Users (`zdk users`)

```bash
zdk users list --role agent --role admin ; zdk users list --organization-id 890 ; zdk users me
zdk users get jane@acme.com ; zdk users get me ; zdk users get 4567
zdk users search "jane" ; zdk users search --external-id crm-42 ; zdk users autocomplete jan ; zdk users related 4567
zdk users create --name "Jane Doe" --email jane@acme.com --role end-user --verified
zdk users create-or-update --file user.json ; zdk users update 4567 --field phone=+441234 ; zdk users update 4567 --suspended
zdk users delete 4567 --yes
```

### Organizations (`zdk orgs`)

```bash
zdk orgs list ; zdk orgs get 890 ; zdk orgs count ; zdk orgs search --name "Acme" ; zdk orgs autocomplete acm
zdk orgs create --name "Acme Corp" --domain acme.com --tag enterprise
zdk orgs update 890 --add-tag renewal-q4 --custom-field tier=gold ; zdk orgs delete 890 --yes
zdk orgs tickets 890 ; zdk orgs users 890 ; zdk orgs related 890
```

### Search (`zdk search`)

```bash
zdk search 'type:ticket status:open priority:urgent' --sort-by updated_at --sort-order desc
zdk search count 'type:user role:agent'
zdk search export 'type:ticket created>2026-01-01' --to tickets.ndjson      # cursor pagination, no 1,000 cap
zdk search export 'status:open' --filter-type ticket -o ndjson              # type inferred from a type: term when absent
zdk search explain --status open --assignee me --older-than 24h             # {"query":"type:ticket status:open assignee:me created<…","endpoint":"/api/v2/search"}
```

### API escape hatch (`zdk api`)

Any Zendesk endpoint, with auth, rate limiting, retries, pagination and scope preflight applied. Never refuses an unknown path; absolute URLs on the profile's host (or `https://status.zendesk.com`, unauthenticated) are accepted.

```bash
zdk api GET /api/v2/tickets/1234/audits --paginate --limit 500 -o ndjson
zdk api /api/v2/users/me                                     # a single argument is a GET path
zdk api POST /api/v2/tickets --data @ticket.json ; zdk api PUT /api/v2/users/4567 --field user.name="Jane"
zdk api GET /api/v2/tickets --query external_id=ORDER-1 --header X-On-Behalf-Of:agent@acme.com --items-key tickets
zdk api ops --grep views --method GET ; zdk api ops --spec help_center --tag Articles
zdk api describe ListTickets --schema ; zdk api describe GET /api/v2/views
```

Operation ids collide across the three specs, so `describe` also accepts `support.ListTickets` / `help_center.ListLocales`.

### Configuration, shell integration, diagnostics

```bash
zdk config init [--non-interactive] [--grant-type authorization_code|client_credentials|api_token] [--scopes A,B] [--email E]
zdk config show [--effective [--reveal-secrets]] ; zdk config get default.page_size ; zdk config set default.page_size 50
zdk config edit ; zdk config validate ; zdk config path
zdk config profiles list ; zdk config profiles add sandbox --subdomain acme1234 --client-id zdk_sandbox --switch
zdk config profiles remove sandbox ; zdk config profiles rename old new ; zdk config profiles switch production
zdk completions bash|zsh|fish|powershell|elvish
zdk man --to ./man
zdk doctor [--json]      # config, profile, credential_store, credentials, api_token_deadline, auth, clock_skew, rate_limit, registry
zdk version              # "zdk 0.1.0" on a TTY; version, target and the committed spec snapshots when piped
```

`zdk doctor` exits 1 when any check fails; each check carries `name`, `status` (`ok` / `warn` / `fail`), `detail` and, on failure, the error `code`.

### Roadmap (not yet shipped)

None of these commands exist in v0.1.0; the PRD in `docs/` describes them. Anything read-only among them is already reachable through `zdk api`.

- views, groups, macros, triggers, automations, SLA policies, ticket fields / forms, custom statuses, tags, suspended tickets, requests, CSAT
- job orchestration and `bulk-*` commands (exit 8 / 13 are reserved for them), attachments, side conversations, a schema resolver (`--resolve-names`, name-based `--group` / `--custom-field "Title=…"`)
- an incremental sync engine and account backup, Help Center and community content
- configuration as code (`rules export / plan / apply / diff / promote`)
- Talk, Chat, omnichannel routing, custom objects, webhooks, ZIS
- GDPR / redaction workflows, audit logs, an interactive API-token → OAuth migration assistant
- generated coverage for every remaining resource group, and an MCP server mode

## Resilience & Rate Limiting

Zendesk enforces an account-wide budget (200–2,500 requests/min by plan) **and** per-endpoint limits that are easy to hit in bulk work. `zdk` handles both:

- **Governor**: a token bucket per family (Support; Help Center paths under `/api/v2/help_center`, `/guide`, `/community` have their own) that starts at 200/min and rewrites its limit from the first `X-Rate-Limit` / `ratelimit-limit` header it sees, persisting it per profile under the state directory. `reserve_percent` (default 10) keeps headroom; `warn_threshold` (50) prints a stderr notice when a budget runs low; `high_volume_addon = true` raises the rules Zendesk raises with the High Volume API add-on. `--rate-limit-strategy wait` (default) sleeps until the bucket refills, `fail` exits 7 immediately without sleeping, `burst` skips every local bucket (only `--max-concurrency`, default 4, still applies) and relies on 429 handling.
- **Per-endpoint rules**, matched by method and path on every request and keyed the way Zendesk keys them:

  | Rule | Endpoint | Limit |
  |---|---|---|
  | `ticket_update` | `PUT /api/v2/tickets/{id}` | 100/min per account (300 with the add-on) |
  | `ticket_update_per_ticket` | `PUT /api/v2/tickets/{id}` | 30 per 10 min **per ticket** |
  | `incremental` | `GET /api/v2/incremental/…` | 10/min (30 with the add-on) |
  | `view_execute` | `GET /api/v2/views/{id}/execute` | 5/min per view |
  | `user_update` | `PUT /api/v2/users/{id}` | 5/min per user |
  | `user_create_or_update` | `POST /api/v2/users/create_or_update` | 5/min per email |
  | `org_update` | `PUT /api/v2/organizations/{id}` | 5/min per organization |
  | `search_export` | `GET /api/v2/search/export` | 100/min |
  | `side_conversations` | `POST …/side_conversations[/{id}/reply]` | 300 per 10 min |
  | `side_conversation_events` | `GET /api/v2/tickets/side_conversations/events` | 600 per 10 min |
  | `agent_availabilities` | `GET /api/v2/agent_availabilities` | 300/min |
  | `tickets_index_deep` | `GET /api/v2/tickets?page=N`, N > 500 | 50/min (applied by the offset paginator) |

- **Retries**: `Retry-After` (seconds or an HTTP date) is honoured on 429; 5xx and transport errors back off exponentially with full jitter — 6 attempts, 1 s base, 60 s cap by default. `--retries N` and `--timeout SECS` override per call; `[retry]` in the config file sets `max_attempts`, `base_ms`, `max_ms`, `jitter = "full" | "equal" | "none"` and `retry_on = [429, 500, 502, 503, 504]`. A first 401 invalidates the cached credential and retries once.
- **Idempotency**: every POST/PUT/PATCH carries an `Idempotency-Key` (UUID v4) that is fixed for the lifetime of the request, so a retry after a timeout cannot double-create; set your own with `--idempotency-key`.
- **Dry runs**: `--dry-run` prints the exact request — `{"method","url","headers","body"}` in machine formats, a `curl` line in table mode, `Authorization` redacted — exits 0 and sends nothing. Commands that make several requests (`tickets update` with tags) print one object per request.
- **Pagination**: cursor pagination by default (`page[size]`, ≤ 100) with `--all` (alias `--paginate`), `--limit N` (truncates mid-page), `--page-size N` and `--checkpoint FILE`, which records progress after every page and resumes from it. Offset-only endpoints are guarded *before* the request that would cross Zendesk's 100-page / 10,000-record wall — and before the first page when `--all` is set and the response's `count` already says the walk cannot finish — with exit 12 / `PAGINATION_LIMIT` and the cursor or export alternative in `help`. Plain search warns on stderr once 1,000 results have been fetched and treats a 422 on `next_page` as the end.
- **Audit**: `--audit-log FILE` appends one NDJSON record per API call (method, URL, status, timing, rate-limit headers; secrets redacted).

## Multi-Profile Support

```bash
zdk --profile production tickets list
zdk -p sandbox tickets create --subject "test" --comment "…" --requester me
zdk config profiles list
```

Config file: `~/.config/zendesk-cli/config.toml` (Linux), `~/Library/Application Support/zendesk-cli/config.toml` (macOS), `%APPDATA%\zendesk-cli\config.toml` (Windows); `XDG_CONFIG_HOME` is honoured on every OS; override the file with `--config` or `ZENDESK_CONFIG`. Unknown keys are warned about, not rejected. **No secret is ever a config field.**

```toml
[default]
active_profile = "production"
output = "table"          # table | json | ndjson | csv | tsv | yaml | raw
page_size = 100
confirm_destructive = true

[auth]
method = "authorization_code"   # authorization_code | client_credentials | api_token (profiles can override)
# callback_port = 8080          # fixed loopback port for clients registered with an explicit redirect URI
auto_refresh = true
refresh_at_percent = 80
credential_store = "auto"       # auto | keyring | file | env | none
suppress_deprecation = false

[rate_limit]
strategy = "wait"         # wait | fail | burst
max_concurrency = 4
reserve_percent = 10
warn_threshold = 50
respect_retry_after = true
high_volume_addon = false

[retry]
max_attempts = 6
base_ms = 1000
max_ms = 60000
jitter = "full"           # full | equal | none
retry_on = [429, 500, 502, 503, 504]

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

A profile may also set `email` (API-token auth), `callback_port`, `credential_store` and an informational `plan`. `[cache]`, `[sync]`, `[jobs]`, `[rules]` and `[audit]` sections are parsed and validated but only `[audit]` (`enabled`, `path`) has any effect in v0.1.0.

Destructive commands (`tickets delete` / `permanently-delete`, `users delete`, `orgs delete`, `comments redact`) print the active profile and subdomain in their confirmation prompt so a production instance is never mistaken for a sandbox; `--yes` skips the prompt, and on a non-TTY without `--yes` they exit 2 before any request.

## Environment Variables

Precedence: CLI flag → environment variable → profile → `[default]` → built-in default. The environment is read exactly once at startup (`crates/zdk-core/src/config/env.rs`); this is the complete list.

| Variable | Description |
|---|---|
| `ZENDESK_SUBDOMAIN` | Instance subdomain (`acme` for `acme.zendesk.com`) |
| `ZENDESK_CLIENT_ID` | OAuth client identifier |
| `ZENDESK_CLIENT_SECRET` | OAuth client secret (confidential clients / client credentials) |
| `ZENDESK_ACCESS_TOKEN` | Direct access token; bypasses the credential store and is never refreshed |
| `ZENDESK_REFRESH_TOKEN` | Read and masked like the other secrets; not consumed by any v0.1.0 code path |
| `ZENDESK_SCOPES` | Comma- or space-separated granular scopes for `auth login` |
| `ZENDESK_EMAIL` | Legacy API-token auth: agent email |
| `ZENDESK_API_TOKEN` | Legacy API-token auth: the token (deprecated; dead after 30 Apr 2027) |
| `ZENDESK_PROFILE` | Active profile (overridden by `--profile`) |
| `ZENDESK_CONFIG` | Config file path (overridden by `--config`) |
| `ZENDESK_OUTPUT` | Default output format (a bad value is a `CONFIG` error, exit 10) |
| `ZENDESK_PAGE_SIZE` | Cursor page size (1–100) |
| `ZENDESK_MAX_CONCURRENCY` | In-flight request ceiling |
| `ZENDESK_RATE_LIMIT_STRATEGY` | `wait` \| `fail` \| `burst` |
| `ZENDESK_NO_CACHE` | Truthy value (anything but empty, `0`, `false`) disables the local cache — reserved for the v0.3 name/schema cache |
| `ZENDESK_CREDENTIAL_STORE` | `auto` \| `keyring` \| `file` \| `env` \| `none` (`none` is what tests and CI use) |
| `ZENDESK_CREDENTIALS_PASSPHRASE` | Passphrase for the encrypted file store (otherwise `credentials.key` is used) |
| `ZENDESK_LOG` | Tracing filter, e.g. `zdk=debug,reqwest=info` (ignored when `-v` is given) |
| `ZENDESK_BASE_URL` | Base URL override for tests and proxies (also the hidden `--base-url` flag) |
| `NO_COLOR` | Present and non-empty disables colour (also `--no-color`; colour is off whenever the stream is not a TTY) |
| `VISUAL`, `EDITOR` | Editor for `zdk config edit` and `--editor` (`VISUAL` wins) |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Directory roots, honoured on every OS when set |

`zdk config show --effective` lists which secret-bearing variables are set (values masked as `***` unless `--reveal-secrets`).

## Verbosity

```bash
zdk tickets list              # warnings only (stderr)
zdk -v tickets list           # info
zdk -vv tickets list          # debug (log targets shown)
zdk -vvv tickets list         # trace: full request/response with secrets redacted
zdk -q tickets list           # suppress non-error stderr (also silences the API-token warning); errors still print
zdk --audit-log audit.ndjson tickets list   # append a structured record of every API call
```

Logs never go to stdout. At `-vvv` the `authorization`, `proxy-authorization`, `cookie` and `set-cookie` headers and any `access_token`, `refresh_token`, `client_secret`, `api_token`, `token` or `password` value in a body are redacted.

## Shell Completions

```bash
zdk completions bash > /etc/bash_completion.d/zdk       # or: >> ~/.bashrc
zdk completions zsh  > ~/.zfunc/_zdk                     # fpath+=(~/.zfunc); autoload -Uz compinit
zdk completions fish > ~/.config/fish/completions/zdk.fish
zdk completions powershell > zdk.ps1
zdk completions elvish > zdk.elv
```

Release archives ship all five under `share/completions/` (`zdk.bash`, `_zdk`, `zdk.fish`, `_zdk.ps1`, `zdk.elv`); the Homebrew formula installs bash, zsh and fish.

## Man Pages

Man pages are generated from the clap definitions with `clap_mangen`, so they never drift from `--help`:

```bash
zdk man --to ./man && man ./man/zdk.1     # zdk.1 plus one page per subcommand (85 files)
```

Release archives ship them under `share/man/man1/`, and the Homebrew formula installs them.

## Roadmap

v0.1.0 (above) is the OAuth-before-the-deadline milestone. Later milestones from the PRD, in order — **none of these exist yet**:

| Version | Scope |
|---|---|
| v0.2 | views, groups, macros, triggers, automations, SLA, fields, forms, statuses, tags, suspended tickets, requests, CSAT; crates.io and ghcr.io publishing |
| v0.3 | job orchestration, `bulk-*`, attachments, side conversations, schema resolver (`--resolve-names`, name-based `--group`/`--custom-field "Title=…"`) |
| v0.4 | incremental sync engine, account backup, Help Center and community |
| v0.5 | configuration-as-code: `rules export/plan/apply/diff/promote` |
| v0.6 | Talk, Chat, omnichannel routing, custom objects, webhooks, ZIS |
| v0.7 | GDPR/redaction workflows, audit logs, an interactive API-token → OAuth migration assistant |
| v0.8 | generated coverage for every remaining resource group |
| v0.9 | MCP server mode |
| v1.0 | stable command surface |

## Development

```bash
export PATH="$HOME/.cargo/bin:$PATH"                 # rustup's cargo first: Homebrew's cargo shadows it on macOS and breaks `cross` / `--target` builds
export ZENDESK_CREDENTIAL_STORE=none                 # never touch the OS keyring from local runs or tests
cargo build                                          # debug build of the workspace
cargo run -p zendesk-cli -- --help                   # run the zdk binary (target/debug/zdk)
cargo test --workspace --all-targets --locked        # unit + wiremock integration + black-box CLI tests
cargo insta test --accept && cargo insta review      # refresh snapshots after changing help text, tables or error reports
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
rustup run 1.88.0 cargo check --workspace --all-targets --locked   # MSRV (`cargo +1.88.0` fails when Homebrew's cargo is first on PATH)
cargo xtask codegen --check                          # generated registry is fresh (byte-identical)
cargo xtask spec-refresh && cargo xtask spec-diff    # pull new Zendesk OpenAPI snapshots and report drift
cargo deny check bans licenses sources advisories
typos
cross build --target x86_64-unknown-linux-musl       # optional: prove the static musl build still links
```

Workspace: `crates/zdk-core` (library: config, auth, credential stores, HTTP core, governor, pagination, generated operation registry, curated API, models, output), `crates/zdk-cli` (the `zdk` binary; package `zendesk-cli`), `xtask` (codegen, spec refresh/diff). See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Contributing

We welcome issues and PRs. Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Security

See [SECURITY.md](SECURITY.md) for our security policy and how to report vulnerabilities. Credentials never touch disk in plaintext, secrets are redacted from logs and `--audit-log`, TLS 1.2+ (rustls, `ring`) is enforced, and the binary talks to your Zendesk instance and `status.zendesk.com` and nothing else (no telemetry).

## License

MIT — see [LICENSE](LICENSE).
