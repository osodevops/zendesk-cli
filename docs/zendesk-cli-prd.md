# Product Requirements Document: `zendesk-cli`

## An OAuth-First, Full-Coverage Command-Line Interface for Zendesk Support Operations

**Version:** 1.0.0-draft
**Author:** Sion Smith / OSO
**Date:** 10 September 2026
**Language:** Rust (edition 2021, MSRV 1.82)
**License:** MIT
**Repository:** `osodevops/zendesk-cli`
**Binary:** `zdk`

---

## 1. Executive Summary

`zendesk-cli` is a Rust-based, single-binary command-line interface for the Zendesk APIs, targeting the **agent, admin, and customer-facing support surface**: tickets, comments, requests, users, organizations, views, macros, triggers, SLAs, side conversations, Help Center content, CSAT, Talk, Chat, custom objects, incremental exports, and bulk job orchestration.

It is explicitly **not** a replacement for `zcli`. Zendesk's official CLI is scoped to app and theme development — its entire command tree is `apps`, `themes`, `connectors`, `profiles`, `login`, `logout` ([zcli docs](https://developer.zendesk.com/documentation/apps/getting-started/using-zcli/)). There is no official tooling for the day-to-day work of running a support operation from a terminal, a CI pipeline, or an AI agent. `zendesk-cli` fills that gap.

Three things make this urgent and differentiated:

1. **Zendesk is removing API tokens.** Announced 1 June 2026: unused tokens auto-deactivate from 28 July 2026, **new token creation is blocked for all accounts from 27 October 2026**, and all tokens are permanently deactivated on **30 April 2027** ([Zendesk announcement](https://support.zendesk.com/hc/en-us/articles/10851263566234-Announcing-the-removal-of-API-tokens-as-an-authentication-method-for-API-requests)). Essentially every existing community Zendesk CLI is API-token-only and will simply stop working. Even the official `zcli` still logs in with an API token ([zcli #384](https://github.com/zendesk/zcli/issues/384)). `zendesk-cli` ships OAuth-first from v0.1.0.
2. **Nobody offers full API coverage.** The Zendesk REST surface is **279 documented resource groups and roughly 1,480 catalogued operations** across Ticketing (88 groups), Help Center (31), Voice/Talk (19), Live Chat (18), Custom Data (17), Omnichannel routing (10), ZIS (12), IT Asset Management (5), AI Agents (5) and more. The broadest existing CLI covers 28 commands.
3. **Zendesk has no configuration-as-code story.** Triggers, automations, macros, views, SLA policies, ticket forms and fields cannot be exported, version-controlled, diffed, or promoted between sandbox and production in any first-class way. This is the single most recurring complaint after rate limiting, and no existing tool addresses it.

### Why This Exists

| Existing option | What it actually is | Why it is not enough |
|---|---|---|
| [`zendesk/zcli`](https://github.com/zendesk/zcli) (official, TypeScript) | App/theme/connector development tool | Zero ticket, user, org or Help Center CRUD. README states: "We are not routinely reviewing issues and merging community-submitted pull requests." API-token login only. `zcli login` fails in headless/Docker environments ("Failed to load secure credentials store") |
| [`johanviberg/zd`](https://github.com/johanviberg/zd) (Go) | Best-maintained community CLI | Tickets + Help Center articles only. No users, orgs, macros, views, side conversations, or incremental export |
| [`zd-cli`](https://pypi.org/project/zd-cli/) (Python, part of a Claude skill) | Broadest community CLI — 28 commands | No macros, triggers, side conversations, custom objects, or incremental exports. Python runtime dependency |
| [`tizzo/zendesk-cli`](https://github.com/tizzo/zendesk-cli) (Rust) | Pre-1.0 read + reply | No OAuth, no create/update, requires libdbus. Binary is also named `zd`, colliding with the Go tool |
| [`xoseperez/zendesk-cli`](https://github.com/xoseperez/zendesk-cli) (Rust) | 4 commands | Self-described as "intentionally read-only and single-account… no OAuth" |
| [`tbs89/typer-zendesk-cli`](https://github.com/tbs89/typer-zendesk-cli) (Python) | Interactive admin menus | Unmaintained since April 2024. Menu-driven, not scriptable. No JSON output |
| [`formspree/zdesk`](https://github.com/formspree/zdesk), [`nvllsvm/zendesk-cli`](https://github.com/nvllsvm/zendesk-cli), ticket viewers | Bulk delete / TUI viewers | Dead (2016–2021). One is archived |
| SDKs: [zenpy](https://github.com/facetoe/zenpy), [node-zendesk](https://github.com/blakmatrix/node-zendesk), [zendesk_api_client_rb](https://github.com/zendesk/zendesk_api_client_rb), [zendesk-java-client](https://github.com/cloudbees-oss/zendesk-java-client) | Libraries, not CLIs | Long-open gaps: no side conversations ([zenpy #476](https://github.com/facetoe/zenpy/issues/476)), no custom objects ([php #558](https://github.com/zendesk/zendesk_api_client_php/issues/558)), no SLA policies ([rb #271](https://github.com/zendesk/zendesk_api_client_rb/issues/271), open ~10 years), no idempotency keys ([rb #510](https://github.com/zendesk/zendesk_api_client_rb/issues/510)), broken search pagination ([zenpy #672](https://github.com/facetoe/zenpy/issues/672)), infinite loops on incremental export ([zenpy #489](https://github.com/facetoe/zenpy/issues/489)), no rate-limit handling ([node-zendesk #430](https://github.com/blakmatrix/node-zendesk/issues/430)) |
| Rust crate [`zendesk` 0.1.0](https://crates.io/crates/zendesk) | Single 2022 release | Abandoned, no repository link |

---

## 2. Goals & Non-Goals

### Goals

- **Full documented API coverage** of the Zendesk Ticketing, Help Center, Voice/Talk, Live Chat, Custom Data, Omnichannel Routing, Webhooks, and IT Asset Management APIs — generated from Zendesk's own OpenAPI specifications, with hand-curated ergonomic commands over the high-traffic 20%.
- **OAuth-first authentication** — authorization code + PKCE for humans, client credentials for CI, with a migration path off API tokens before the 27 October 2026 and 30 April 2027 deadlines.
- **A rate-limit-aware HTTP core** that reads every Zendesk rate-limit header, respects per-endpoint limits, honours `Retry-After`, and never wedges a long export.
- **Correct pagination everywhere** — cursor-based by default, with automatic fallbacks for the endpoints that only support offset, incremental-export cursors, or the `/api/v2/ticket_audits` variant.
- **Resumable, deduplicating incremental sync** with durable high-water marks — the piece every team rebuilds by hand.
- **Bulk operations with dry-run, job polling, and partial-failure reporting** — the async job model made safe.
- **Configuration-as-code** for business rules (triggers, automations, macros, views, SLAs, forms, fields, webhooks): export, diff, plan, apply, and promote between sandbox and production.
- **Human-readable names, not just IDs** — a local schema cache that resolves custom field, form, group, brand, and status IDs to names and back.
- **Pipeline-friendly output** — table, JSON, NDJSON, CSV, YAML, with `--fields` projection and stable exit codes.
- **Customer-facing surface as a first-class citizen** — the Requests API (end-user perspective), Help Center content, CSAT surveys, satisfaction ratings, and end-user identity management.
- **Headless-safe credential storage** — must work in Docker, CI, and over SSH with no Secret Service, D-Bus, or X11 present.
- **A single static binary** for macOS (Intel + ARM), Linux (glibc + musl, x86_64 + aarch64), and Windows.
- **Optional MCP server mode** from the same binary, so the CLI doubles as an agent tool surface.

### Non-Goals

- **Not an app/theme development tool.** `zcli` owns that; `zendesk-cli` will not duplicate `apps`, `themes`, or `connectors` scaffolding. (It will still expose the *Apps REST API* for installation and settings management, which is a different thing.)
- **Not a TUI.** No interactive full-screen inbox in v1. Pure CLI, plus optional `--watch` polling.
- **Not Zendesk Sell** in v1. Sell is 66 resource groups on a different API version and base path, and **Zendesk Sell retires on 31 August 2027** — building for it is wasted effort. Deferred permanently unless a client needs it.
- **Not Sunshine Conversations** in v1. Different auth model, different host (`api.smooch.io`), 17 resource groups. Deferred to v1.2.
- **Not the JavaScript SDK surfaces** — Web Widget, Android/iOS/Unity SDKs, and the Apps framework client APIs are not HTTP APIs and are out of scope.
- **Not a reporting engine.** Zendesk publishes no public Explore or reporting API; `zendesk-cli` will not fake one. It exposes ticket metrics, metric events, satisfaction ratings, and Talk stats, and leaves aggregation to the pipeline.
- **Not a data warehouse.** Exports write files; loading them is someone else's job.

---

## 3. Target Users

| Persona | Use Case |
|---|---|
| **Support engineer / agent** | Triage from the terminal: list assigned tickets, read the thread, reply, apply a macro, escalate, set status — without leaving the shell |
| **Support ops / admin** | Bulk reassignments, macro and trigger management, form and field audits, sandbox-to-production promotion, CSAT reporting |
| **Platform engineer / SRE** | Nightly incremental exports into a lakehouse, webhook management, ticket creation from alerting, backup and disaster recovery |
| **Developer building on Zendesk** | Explore the API interactively, prototype calls, debug rate limits and payloads with a raw escape hatch |
| **AI agent / automation** | Deterministic, scriptable, JSON-first access with exit codes — via CLI or the built-in MCP server |
| **Compliance / DPO** | GDPR erasure workflows, redaction, audit log extraction, deletion schedules |
| **Migration consultant (OSO)** | Bulk import, cross-instance diff, tenant-to-tenant content moves, health assessments |

---

## 4. The Zendesk API Reality Check

This section is the design brief. Every constraint below is a documented behaviour the CLI must absorb so users never have to.

### 4.1 Account rate limits

| Zendesk Suite plan | Support + Help Center req/min | Chat API req/min |
|---|---:|---:|
| Team | 200 | 200 |
| Growth | 400 | 200 |
| Professional | 400 | 200 |
| Enterprise | 700 | 200 |
| Enterprise Plus | 2,500 | 200 |

Source: [Rate limits](https://developer.zendesk.com/api-reference/introduction/rate-limits/). The High Volume API add-on raises the Support/Help Center limit to 2,500/min (it does not add 2,500), requires Suite Growth+ or Support Professional+ and a minimum of 10 agent seats. Help Center requests do not count against the Support budget and vice versa.

### 4.2 Per-endpoint rate limits (the ones that surprise people)

| Endpoint | Limit |
|---|---|
| `GET /api/v2/tickets.json?page={num}` where num > 500 | 50 req/min |
| `PUT /api/v2/tickets/{id}` | 30 updates per 10 min **per user per ticket**; 100 req/min per account (300 with High Volume) |
| `GET /api/v2/incremental/*` | **10 req/min** globally (30 with High Volume) |
| `GET /api/v2/views/{view_id}/execute.json` | 5 req/min per view per agent |
| `GET /api/v2/views/{id}/export.json` | 100,000 req/hour |
| `PUT /api/v2/users/{id}` and `POST /api/v2/users/create_or_update` | 5 req/min per user |
| `PUT /api/v2/organizations/{id}` | 5 req/min per organization |
| `GET /api/v2/search/export` | 100 req/min per account |
| `POST /api/v2/tickets/{id}/side_conversations[/{id}/reply]` | 300 req per 10 min |
| `GET /api/v2/tickets/side_conversations/events` | 600 req per 10 min |
| `GET /attachments/token/{token}/?name={file}` | 2,500 req/min (10 req/min for uploads not yet attached) |
| `GET /api/v2/agent_availabilities` | 300 req/min |
| Background jobs | **30 queued or running at once**, else `TooManyJobs` |

Source: [Rate limits](https://developer.zendesk.com/api-reference/introduction/rate-limits/). Note the trap: the incremental export limit is 10/min, which is 240× tighter than an Enterprise Plus account limit. A naive exporter looks fine for ten seconds then collapses.

### 4.3 Rate-limit headers to consume

| Header | Meaning |
|---|---|
| `X-Rate-Limit` / `ratelimit-limit` | Current account limit |
| `X-Rate-Limit-Remaining` / `ratelimit-remaining` | Requests left this minute |
| `ratelimit-reset` | Seconds until reset |
| `Retry-After` | Seconds to wait after a 429 |
| `zendesk-ratelimit-tickets-index` | `total=100; remaining=99; resets=41` — ticket index sub-budget |
| `zendesk-ratelimit-inflight-jobs` | `total=30; remaining=29; resets=60` — concurrent job budget |

Source: [Rate limits](https://developer.zendesk.com/api-reference/introduction/rate-limits/). Community reports also describe undocumented account-specific "conditional rate limits" that produce 429s while `remaining` still shows several hundred — so the limiter must trust `Retry-After` over its own arithmetic.

### 4.4 Pagination

- **Cursor pagination** is enabled by passing `page[size]` (max 100 on most endpoints). Response carries `meta.has_more`, `meta.after_cursor`, `meta.before_cursor`, and `links.next` / `links.prev`. Stop when `meta.has_more` is `false`. **No page-depth or record-count limit.**
- **Offset pagination** is the default when `page[size]` is absent. Since 15 August 2023, requests beyond the first **10,000 records / 100 pages** return `400 Bad Request`.
- Help Center additionally returns `next`/`prev` in the `Link` HTTP header and supports backward pagination and a `links.last` jump.
- Incremental exports use a **third** dialect, and `GET /api/v2/ticket_audits` uses a fourth.
- `sort_order` / `sort_by` support is per-endpoint and is silently ignored on several cursor-paginated endpoints.

Source: [Pagination](https://developer.zendesk.com/api-reference/introduction/pagination/).

### 4.5 Incremental exports

- Cursor-based incremental export is the recommended mode; `after_cursor` is the high-water mark.
- Time-based mode has a **one-minute exclusion window** — results only include records up to one minute before the request — and returns `end_time` to feed the next call.
- **Duplicates are by design.** The same record can appear in consecutive pages if it changed again inside the window; consumers must dedupe on `id` + `generated_timestamp`.
- Sending the same `start_time` returns the same page, so a naive loop that does not advance the cursor spins forever (exactly [zenpy #489](https://github.com/facetoe/zenpy/issues/489)).
- `count` is unreliable and must not be used as a loop terminator.
- Global limit: 10 req/min.
- Zendesk explicitly does not recommend continuous syncing with this API.

Source: [Using the Incremental Exports API](https://developer.zendesk.com/documentation/api-basics/working-with-data/using-the-incremental-export-api/), [Incremental Exports](https://developer.zendesk.com/api-reference/ticketing/ticket-management/incremental_exports/).

### 4.6 Search

- The Search API caps results at **1,000 records** even when `count` reports more, and `next_page` can link to a page that then fails.
- The index is eventually consistent — a just-created ticket may not appear.
- `GET /api/v2/search/export` is the high-volume alternative (100 req/min, cursor-paginated, different field semantics).
- Search does not support the standard cursor `page[size]` contract.

### 4.7 Bulk operations and jobs

Endpoints like `Update Many Tickets`, `Create Many Users`, and `Delete Many` return a **job status** rather than results. Batches are capped at 100 records. Job history is short-lived and only the last 100 jobs are retrievable. Jobs also consume the per-minute request budget, and only 30 may be queued or running at once.

Source: [Job Statuses](https://developer.zendesk.com/api-reference/ticketing/ticket-management/job_statuses/), [Rate limits](https://developer.zendesk.com/api-reference/introduction/rate-limits/).

### 4.8 Authentication — the dated cliff edge

| Date | Phase |
|---|---|
| 1 June 2026 | Announcement published |
| **28 July 2026** | Phase 1 — one-time cleanup: tokens unused ≥30 days deactivated; deactivated ≥60 days permanently deleted; ongoing 30-day inactivity rule; accounts created on/after this date cannot create or use API tokens |
| 21 / 26 Sept 2026 | First deletion-warning emails; first permanent deletions |
| **27 October 2026** | Phase 2 — **no account can create new API tokens** via UI or API |
| **30 April 2027** | Phase 3 — all remaining tokens permanently deactivated; management pages removed from Admin Center; webhook auth via API token stops working |

Sources: [Zendesk announcement](https://support.zendesk.com/hc/en-us/articles/10851263566234-Announcing-the-removal-of-API-tokens-as-an-authentication-method-for-API-requests), [Migrating from API tokens to OAuth access tokens](https://developer.zendesk.com/documentation/authentication/oauth-migration/).

Affected: Ticketing, Help Center, and Voice APIs. Not affected: Messaging and Chat product tokens.

Also relevant:

- Email + password auth for APIs was already deprecated, ending 12 January 2026 ([announcement](https://support.zendesk.com/hc/en-us/articles/9941874259354-Deprecation-of-password-access-for-APIs)).
- **Granular OAuth scopes** landed 6–11 August 2026 (`tickets:read`, `triggers:write`, `webhooks:write`, `auditlogs:read`, …). Requesting a scope outside a client's allowed set fails with `400 Bad Request` / `invalid_scope` ([announcement](https://support.zendesk.com/hc/en-us/articles/11093380955674-Announcing-more-granular-OAuth-client-scopes)).
- OAuth clients created on or after 30 April 2026 default to `expires_in` of **30 minutes** (configurable 300 s – 172,800 s). Refresh-token flow is enforced for global clients from 2 February 2026 and new local clients from 30 April 2026, with an adoption deadline of 1 April 2027 ([Creating and using OAuth tokens](https://developer.zendesk.com/documentation/authentication/creating-and-using-oauth-tokens-with-the-api/)).
- `POST /api/v2/oauth/tokens` is deprecated as of 26 August 2026 ([changelog](https://developer.zendesk.com/api-reference/changelog/changelog/)).
- Authorization codes expire after **120 seconds**.
- If no scope is requested, the token defaults to full read+write across all resources — the CLI must therefore always request explicit, narrow scopes.
- Known migration gaps to be transparent about: `organization_field` and `custom_objects` have no granular write scopes, forcing blanket global `write` ([community thread](https://community.zendesk.com/platform-and-developer-18/oauth-scopes-organization-field-and-custom-objects-22314)).

### 4.9 Other dated changes to design around

| Change | Date |
|---|---|
| Legacy Sunshine custom objects removed | July 2026 |
| Legacy AI Agents Data Export EOL | 29 May 2026 |
| Legacy `*.ultimate.ai` AI-agents URL retired | 1 July 2026 |
| ITAM `purchase_cost` becomes an object | 6 Aug 2026 |
| `/api/v2/apps/locations/{id}` removed | 30 Apr 2026 |
| Chat Conversations API closed to new integrations | 30 Apr 2025 |
| Help Center themes must be on v4 | by 31 July 2027 |
| **Zendesk Sell retires** | **31 Aug 2027** |

Zendesk's change-notice policy: Business Critical 3 months, Standard 1 month, Beta 1 week, Development none ([API changes](https://developer.zendesk.com/api-reference/introduction/api_changes/)). The CLI should therefore pin to a spec snapshot and refresh on a schedule.

---

## 5. Architecture

### 5.1 High-Level Design

```
┌──────────────────────────────────────────────────────────────┐
│                        zdk binary                             │
├──────────┬──────────┬────────────┬──────────┬────────────────┤
│  CLI     │  Auth    │  API       │  Output  │  MCP           │
│  Parser  │  Engine  │  Client    │  Render  │  Server        │
│  (clap)  │ (OAuth2) │ (reqwest)  │          │  (optional)    │
├──────────┴──────────┴────────────┴──────────┴────────────────┤
│                      Core Services                            │
├────────────┬────────────┬────────────┬───────────┬───────────┤
│  Rate      │ Pagination │  Job       │  Sync     │  Schema   │
│  Governor  │  Engine    │  Orchestr. │  Engine   │  Resolver │
├────────────┼────────────┼────────────┼───────────┼───────────┤
│  Config-   │  Retry &   │  Cache     │  Config   │  Scope    │
│  as-Code   │  Idempot.  │  (SQLite)  │  Manager  │  Manager  │
├────────────┴────────────┴────────────┴───────────┴───────────┤
│                Generated API Surface (from OAS)               │
│      279 resource groups · ~1,480 operations · typed          │
├───────────────────────────────────────────────────────────────┤
│              Credential Store (keyring → encrypted file)      │
└───────────────────────────────────────────────────────────────┘
```

### 5.2 The coverage strategy — how "full API coverage" is actually achieved

Hand-writing ~1,480 operations is not viable and would rot within a quarter. Coverage comes from three layers:

**Layer 1 — Generated surface (100% of documented endpoints).**
A `xtask` build tool ingests Zendesk's published OpenAPI specifications and generates typed request/response structs plus a low-level operation registry:

| Spec | Covers |
|---|---|
| Support / Ticketing OAS | 88 Ticketing resource groups |
| Help Center OAS | 31 Help Center / Guide groups |
| Voice OAS | 19 Talk groups |
| Chat (documented endpoints) | 18 Live Chat groups |
| Custom Data / Custom Objects | 17 groups |
| Agent availability & routing | 10 groups |
| Webhooks, ZIS, ITAM, AI Agents, Status, Reseller | remainder |

The generated registry is committed to the repo (not generated at build time from the network) so builds are reproducible and offline. A CI job re-runs generation weekly and opens a PR when the upstream spec changes — this is how the CLI keeps pace with Zendesk's 1-month standard change notice.

**Layer 2 — Curated ergonomic commands (the 20% that gets 95% of use).**
Hand-written, hand-tested, flag-rich commands for tickets, comments, requests, users, organizations, groups, views, search, macros, triggers, automations, SLAs, side conversations, satisfaction, Help Center articles, custom objects, webhooks, incremental exports, and jobs.

**Layer 3 — The universal escape hatch.**
```bash
zdk api GET /api/v2/tickets/123
zdk api POST /api/v2/tickets --data @ticket.json
zdk api GET /api/v2/tickets --paginate --page-size 100
zdk api PUT /api/v2/users/456 --field name="New Name" --field verified:=true
```
Anything Zendesk ships tomorrow works today: auth, rate limiting, pagination, retries, and output formatting all apply. `zdk api ops` lists every operation in the generated registry; `zdk api describe <operation>` prints its schema.

This means the answer to "does it support X?" is always yes.

### 5.3 Crate Dependencies (Recommended)

| Crate | Purpose |
|---|---|
| `clap` (v4, derive, env, string) | CLI parsing, subcommands, env fallback |
| `clap_complete`, `clap_mangen` | Shell completions and man pages |
| `tokio` (full) | Async runtime |
| `reqwest` (rustls-tls, json, stream, multipart) | HTTP client — no OpenSSL dependency |
| `reqwest-middleware` | Retry/rate-limit/logging middleware chain |
| `oauth2` (v5) | Authorization code + PKCE + client credentials |
| `tiny_http` | Loopback callback listener for the browser flow |
| `open` | Launch the system browser |
| `keyring` | OS keychain (macOS Keychain, Windows Credential Manager, Secret Service) |
| `age` or `chacha20poly1305` + `argon2` | Encrypted-file credential fallback for headless environments |
| `serde`, `serde_json`, `serde_yaml`, `toml`, `toml_edit` | Serialisation and config editing |
| `jsonschema` | Validate config-as-code manifests before apply |
| `similar` | Unified diffs for `zdk rules plan` |
| `comfy-table` | Terminal tables |
| `csv` | CSV output |
| `rusqlite` (bundled) | Schema cache, sync state, rate-limit ledger |
| `governor` | Token-bucket / GCRA rate limiting |
| `backoff` or hand-rolled | Exponential backoff with jitter |
| `chrono` | Timestamps, Unix epoch conversion for incremental exports |
| `indicatif` | Progress bars on stderr |
| `tracing`, `tracing-subscriber` (env-filter) | Structured logging |
| `thiserror`, `miette` (fancy) | Typed errors and rich diagnostics |
| `dirs` | XDG-compliant paths |
| `is-terminal` | TTY detection for format auto-selection |
| `uuid` | Idempotency keys |
| `url` | URL construction and cursor parsing |
| `mime_guess` | Attachment upload content types |
| `rmcp` (or hand-rolled JSON-RPC) | MCP server mode |
| **dev:** `wiremock`, `assert_cmd`, `predicates`, `insta`, `tempfile`, `proptest` | Mock API, CLI assertions, snapshot tests, property tests |

### 5.4 Project Structure

```
zendesk-cli/
├── Cargo.toml                       # workspace
├── xtask/                           # codegen + spec refresh tooling
│   ├── src/main.rs
│   └── src/openapi/                 # OAS → Rust generator
├── specs/                           # committed OAS snapshots + version manifest
│   ├── support.yaml
│   ├── help_center.yaml
│   ├── voice.yaml
│   ├── chat.yaml
│   ├── custom_objects.yaml
│   └── SPEC_VERSIONS.toml
├── crates/
│   ├── zdk-core/                    # library: everything except argv parsing
│   │   ├── src/lib.rs
│   │   ├── src/auth/
│   │   │   ├── mod.rs
│   │   │   ├── authorization_code.rs   # + PKCE
│   │   │   ├── client_credentials.rs
│   │   │   ├── api_token.rs            # legacy, with deprecation warning
│   │   │   ├── token_store.rs          # keyring → encrypted file → env
│   │   │   ├── refresh.rs              # pre-emptive refresh at 80% of TTL
│   │   │   └── scopes.rs               # granular scope catalogue + presets
│   │   ├── src/http/
│   │   │   ├── client.rs               # ZendeskClient
│   │   │   ├── middleware.rs           # auth → rate limit → retry → trace
│   │   │   ├── rate_limit/
│   │   │   │   ├── governor.rs          # account + per-endpoint buckets
│   │   │   │   ├── headers.rs           # parse all 6 header families
│   │   │   │   ├── endpoint_rules.rs    # static table from §4.2
│   │   │   │   └── ledger.rs            # SQLite usage history
│   │   │   ├── retry.rs                # 429/5xx backoff + jitter
│   │   │   └── idempotency.rs          # Idempotency-Key on POST
│   │   ├── src/pagination/
│   │   │   ├── mod.rs
│   │   │   ├── cursor.rs               # page[size] / meta.has_more
│   │   │   ├── offset.rs               # + 100-page guard rail
│   │   │   ├── link_header.rs          # Help Center Link header
│   │   │   ├── incremental.rs          # after_cursor / end_time dialect
│   │   │   └── audits.rs               # ticket_audits variant
│   │   ├── src/jobs/
│   │   │   ├── orchestrator.rs         # batch → submit → poll → reconcile
│   │   │   ├── poller.rs               # inflight-jobs budget aware
│   │   │   └── results.rs              # partial-failure report
│   │   ├── src/sync/
│   │   │   ├── engine.rs               # incremental export driver
│   │   │   ├── state.rs                # durable high-water marks
│   │   │   └── dedupe.rs               # id + generated_timestamp
│   │   ├── src/schema/
│   │   │   ├── resolver.rs             # ID ↔ name for fields/forms/groups/…
│   │   │   └── cache.rs
│   │   ├── src/rules/                  # configuration-as-code
│   │   │   ├── export.rs
│   │   │   ├── manifest.rs             # canonical YAML representation
│   │   │   ├── plan.rs                 # diff engine
│   │   │   ├── apply.rs
│   │   │   └── refs.rs                 # cross-instance reference remapping
│   │   ├── src/api/
│   │   │   ├── mod.rs
│   │   │   ├── generated/              # ← xtask output, committed
│   │   │   │   ├── registry.rs         # operation catalogue
│   │   │   │   ├── ticketing/…
│   │   │   │   ├── help_center/…
│   │   │   │   ├── voice/…
│   │   │   │   ├── chat/…
│   │   │   │   └── custom_data/…
│   │   │   └── curated/                # hand-written ergonomic wrappers
│   │   ├── src/models/                 # newtypes, enums, sideload types
│   │   ├── src/output/
│   │   │   ├── table.rs  json.rs  ndjson.rs  csv.rs  yaml.rs
│   │   │   └── project.rs              # --fields / --exclude projection
│   │   ├── src/config/
│   │   │   ├── file.rs  profiles.rs  env.rs
│   │   ├── src/cache/
│   │   └── src/error.rs
│   ├── zdk-cli/                     # binary `zdk`
│   │   ├── src/main.rs
│   │   └── src/cmd/
│   │       ├── auth.rs        tickets.rs     comments.rs
│   │       ├── requests.rs    users.rs       orgs.rs
│   │       ├── groups.rs      views.rs       search.rs
│   │       ├── macros.rs      triggers.rs    automations.rs
│   │       ├── sla.rs         forms.rs       fields.rs
│   │       ├── side_conv.rs   satisfaction.rs csat.rs
│   │       ├── suspended.rs   tags.rs        brands.rs
│   │       ├── hc.rs          community.rs   themes.rs
│   │       ├── talk.rs        chat.rs        routing.rs
│   │       ├── objects.rs     webhooks.rs    targets.rs
│   │       ├── jobs.rs        sync.rs        backup.rs
│   │       ├── gdpr.rs        audit.rs       schema.rs
│   │       ├── rules.rs       diff.rs        sandbox.rs
│   │       ├── api.rs         rate_limit.rs  config.rs
│   │       ├── completions.rs man.rs         doctor.rs
│   │       └── mcp.rs
│   └── zdk-mcp/                     # MCP tool surface over zdk-core
├── tests/
│   ├── fixtures/                    # real API response samples
│   ├── integration/                 # wiremock-driven
│   └── e2e/                         # against a sandbox
├── docs/
│   ├── zendesk-cli-prd.md
│   ├── oauth-migration.md
│   └── recipes/
└── .github/workflows/
    ├── test.yml  release.yml  spec-refresh.yml
```

---

## 6. Authentication

**Design principle: OAuth is the default and the documented path. API token support exists only as a migration ramp and warns on every use.**

### 6.1 Authorization Code + PKCE (interactive default)

```bash
# First run — walks through subdomain, client, scopes, browser consent
zdk auth login
zdk auth login --subdomain acme --client-id zdk_local --scopes tickets:read,tickets:write,users:read
zdk auth login --port 9876                 # custom loopback port
zdk auth login --no-browser                # print URL, paste code back (headless)
zdk auth login --redirect-uri https://localhost   # copy code from address bar
```

Implementation requirements:

- Generate `code_verifier`, derive `code_challenge`, send `code_challenge_method=S256`, omit `client_secret` on exchange for public clients.
- Send and verify a random `state`.
- **Authorization codes expire in 120 seconds** — exchange immediately and surface a clear error if the window is missed.
- Loopback listener binds `127.0.0.1` only, serves a single request, then shuts down.
- `--no-browser` mode prints the URL and accepts the code on stdin, for SSH and containers.

### 6.2 Client Credentials (CI / automation)

```bash
zdk auth login --client-credentials \
  --subdomain acme --client-id zdk_ci --client-secret "$ZENDESK_CLIENT_SECRET" \
  --scopes tickets:read,users:read
```

- Confidential clients only. **No refresh token is issued** — request a fresh access token with the same credentials when the current one expires.
- Must never be suggested for interactive desktop use.

### 6.3 API token (legacy, deprecated)

```bash
zdk auth login --api-token --subdomain acme --email me@acme.com --token "$ZENDESK_API_TOKEN"
```

Every invocation using API-token auth emits a one-line stderr warning with the live countdown:

```
warning: API token auth is deprecated. New tokens cannot be created after 27 Oct 2026;
         all tokens stop working on 30 Apr 2027 (232 days). Run `zdk auth migrate`.
```

The warning is suppressible with `--quiet` or `auth.suppress_deprecation = true`, and is never printed on stdout.

### 6.4 Migration assistant — a headline feature

```bash
zdk auth migrate
```

Interactive walkthrough that:

1. Detects the current auth method and, where the account permits, lists API tokens with their `last_used` and `Deactivates on` values.
2. Prints the exact Admin Center path (**Apps and integrations → APIs → OAuth clients**) and the client kind to choose (Public for interactive, Confidential for CI/client-credentials).
3. Recommends an **Allowed scopes** set derived from the commands actually used in the local audit log — least privilege by evidence, not guesswork.
4. Runs the new flow, verifies the token with a `GET /api/v2/users/me`, and stores it.
5. Keeps the old token configured in parallel until the user confirms, since both methods work until 30 April 2027.
6. `zdk auth migrate --report` emits a markdown report of every integration hostname/user-agent seen in the account's API token usage report, for teams auditing beyond the CLI.

### 6.5 Scope management

```bash
zdk auth scopes                        # show granted vs required
zdk auth scopes list                   # full granular catalogue
zdk auth scopes add triggers:write     # triggers re-auth
zdk auth scopes preset agent           # tickets:*, users:read, orgs:read, hc:read
zdk auth scopes preset admin           # + triggers:write, automations:write, macros:write, webhooks:write
zdk auth scopes preset readonly        # every *:read
zdk auth scopes preset exporter        # tickets:read, users:read, organizations:read, auditlogs:read
zdk auth scopes check tickets update   # "requires tickets:write — you have tickets:read"
```

Requirements:

- Ship a static map of command → required granular scope. Fail **before** the HTTP call with an actionable message rather than surfacing a bare 403.
- Never request an empty scope — an empty scope grants full read+write, which is unacceptable default behaviour.
- Handle `400 invalid_scope` (scope outside the client's Allowed scopes) with a message naming the offending scope and the Admin Center screen to change.
- Document the known gaps honestly: `organization_field` and `custom_objects` currently have no granular write scope, so those commands require global `write`. `zdk auth scopes check` says so explicitly.

### 6.6 Token storage and refresh

- **Primary:** OS keychain via `keyring`.
- **Fallback:** `~/.config/zendesk-cli/credentials.age`, XChaCha20-Poly1305 with an Argon2id-derived key, `0600`. Chosen automatically when no keychain is available (Docker, CI, SSH) — this is a direct fix for the `zcli` "Failed to load secure credentials store" failure mode.
- **Explicit:** `--credential-store keyring|file|env|none`.
- **Env:** `ZENDESK_ACCESS_TOKEN` bypasses the store entirely, for ephemeral CI.
- Refresh pre-emptively at **80% of `expires_in`** — with the 30-minute default on clients created after 30 April 2026, that is a refresh at ~24 minutes. Never let a long export die mid-flight on a 401.
- Refresh-token rotation is mandatory: persist the new refresh token atomically and treat a rotation failure as fatal rather than retrying with a burned token.
- `zdk auth status` shows subdomain, client id, grant type, granted scopes, token expiry countdown, refresh-token presence, store backend, and — when on API tokens — the days remaining until each deadline.

```bash
zdk auth status
zdk auth refresh
zdk auth logout                # revoke + purge
zdk auth whoami                # GET /api/v2/users/me, shows role and permissions
zdk auth test                  # one cheap call per configured product API
```

---

## 7. Command Structure

### 7.1 Global Flags

| Flag | Description |
|---|---|
| `-p, --profile <name>` | Profile / instance to target |
| `--all-profiles` | Fan out across every configured instance |
| `--subdomain <sub>` | Override subdomain for one call |
| `-o, --output <fmt>` | `table` \| `json` \| `ndjson` \| `csv` \| `yaml` \| `raw` \| `tsv` |
| `--fields <a,b,c>` | Project specific fields (dot paths supported) |
| `--exclude <a,b>` | Drop fields |
| `--compact` | Single-line JSON |
| `--jq <expr>` | Post-process with an embedded jq-compatible filter |
| `--all` / `--paginate` | Auto-paginate to completion |
| `--limit <n>` | Stop after n records |
| `--page-size <n>` | Cursor page size (default 100) |
| `--sideload <a,b>` | Request sideloads (`users`, `groups`, `comment_count`, …) |
| `--resolve-names` | Replace IDs with human names via the schema cache |
| `--dry-run` | Print the request that would be sent; consume no quota |
| `--yes` / `-y` | Skip destructive-action confirmation |
| `--idempotency-key <k>` | Explicit key (auto-generated for writes by default) |
| `--no-cache` | Bypass the local cache |
| `--rate-limit-strategy <s>` | `wait` (default) \| `fail` \| `burst` |
| `--max-concurrency <n>` | In-flight request ceiling |
| `--timeout <secs>` | Per-request timeout |
| `--retries <n>` | Override retry count |
| `--quiet` / `-q` | Suppress non-error stderr |
| `-v` / `-vv` / `-vvv` | Increase verbosity (`-vvv` logs full request/response, secrets redacted) |
| `--no-color` | Disable colour (also honours `NO_COLOR`) |
| `--audit-log <path>` | Append a structured record of every API call |
| `--config <path>` | Alternative config file |
| `--version` / `--help` | Standard |

**Output auto-selection:** `table` when stdout is a TTY, `json` when piped. Progress bars, warnings, and prompts always go to stderr so stdout stays machine-parseable.

### 7.2 Resource Command Pattern

Every resource follows the same shape, so learning one teaches all 279:

```
zdk <resource> list [--all] [FILTERS]
zdk <resource> get <ID> [--sideload …]
zdk <resource> create [--file f.json | --field k=v …]
zdk <resource> update <ID> [--field k=v …]
zdk <resource> delete <ID> [--yes]
zdk <resource> count
zdk <resource> search <QUERY>
zdk <resource> bulk-update --from ids.txt --field status=solved
zdk <resource> bulk-delete --from ids.txt --yes
zdk <resource> export [--to file] [--format ndjson]
zdk <resource> watch [--interval 30s]
```

---

## 8. Full Command Reference

### 8.1 Tickets — the core

```bash
# Reading
zdk tickets list                                   # cursor-paginated
zdk tickets list --all                             # every ticket
zdk tickets list --status open,pending
zdk tickets list --assignee me
zdk tickets list --assignee "sion@oso.sh"
zdk tickets list --group "Platform Support"
zdk tickets list --organization "Acme Corp"
zdk tickets list --requester "user@acme.com"
zdk tickets list --brand "OSO Support"
zdk tickets list --form "Incident"
zdk tickets list --tag urgent --tag escalated
zdk tickets list --priority urgent
zdk tickets list --type incident
zdk tickets list --created-after 2026-09-01 --created-before 2026-09-10
zdk tickets list --updated-after "2 hours ago"     # human durations accepted
zdk tickets list --custom-field "Environment=production"
zdk tickets list --external-id ORDER-4471
zdk tickets list --sort-by updated_at --sort-order desc
zdk tickets list --sideload users,groups,organizations,comment_count
zdk tickets list --resolve-names -o table
zdk tickets list --unassigned --older-than 24h     # composite convenience filters
zdk tickets list --sla-breached
zdk tickets list --awaiting-customer

zdk tickets get 12345
zdk tickets get 12345 --with-comments              # single logical view
zdk tickets get 12345 --with-audits --with-metrics
zdk tickets show 12345                             # rendered conversation transcript
zdk tickets related 12345                          # incidents/problems/followups
zdk tickets collaborators 12345
zdk tickets followers 12345
zdk tickets email-ccs 12345
zdk tickets count --status open
zdk tickets recent                                 # recently viewed by me

# Writing
zdk tickets create --subject "API latency" --comment "Investigating" \
  --requester "user@acme.com" --priority high --type incident \
  --tag latency --group "Platform Support" --form "Incident" \
  --custom-field "Environment=production"
zdk tickets create --file ticket.json
zdk tickets create --from-stdin < ticket.json
zdk tickets create --template incident --var service=kafka   # local templates

zdk tickets update 12345 --status pending --priority urgent
zdk tickets update 12345 --assignee me
zdk tickets update 12345 --add-tag escalated --remove-tag triage
zdk tickets update 12345 --add-collaborator "lead@acme.com"
zdk tickets update 12345 --add-follower "cto@acme.com"
zdk tickets update 12345 --custom-field "Root Cause=upstream"
zdk tickets update 12345 --custom-status "Awaiting parts"
zdk tickets update 12345 --safe-update --updated-stamp "$STAMP"   # optimistic concurrency

zdk tickets reply 12345 --body "Fixed in 4.2.1" --public
zdk tickets reply 12345 --body-file reply.md --public
zdk tickets reply 12345 --editor                    # opens $EDITOR
zdk tickets note 12345 --body "Customer called"      # internal note
zdk tickets reply 12345 --body "See attached" --attach ./trace.log --attach ./chart.png
zdk tickets reply 12345 --macro "Escalate to Tier 2"
zdk tickets solve 12345 --body "Resolved"            # reply + status in one call
zdk tickets close 12345
zdk tickets reopen 12345

zdk tickets assign 12345 --to me
zdk tickets assign 12345 --to "sion@oso.sh" --group "Platform Support"
zdk tickets escalate 12345 --to "Tier 2" --priority urgent --note "…"
zdk tickets macro apply 12345 "Refund request"
zdk tickets macro preview 12345 "Refund request"     # show effect without applying
zdk tickets merge --into 12345 --tickets 12346,12347 --target-comment "…" --source-comment "…"
zdk tickets mark-spam 12345
zdk tickets skip 12345 --reason "no action needed"
zdk tickets delete 12345 --yes
zdk tickets restore 12345                            # from deleted tickets
zdk tickets permanently-delete 12345 --yes

# Bulk
zdk tickets bulk-update --from ids.txt --field status=solved --dry-run
zdk tickets bulk-update --from ids.txt --field status=solved --batch-size 100 --wait
zdk tickets bulk-update --query "status:pending tags:stale" --field status=solved
zdk tickets bulk-assign --from ids.txt --to "sion@oso.sh"
zdk tickets bulk-tag --from ids.txt --add-tag audited-2026
zdk tickets bulk-delete --from ids.txt --yes --wait
zdk tickets bulk-macro --from ids.txt --macro "Close as duplicate"

# Import (bypasses triggers/notifications, preserves timestamps)
zdk tickets import --file historical.json
zdk tickets import --file historical.ndjson --bulk --batch-size 100 --wait

# Watch
zdk tickets watch --status new --interval 30s
zdk tickets watch --query "priority:urgent status<solved" --exec "./page-oncall.sh {{id}}"
```

### 8.2 Comments and audits

```bash
zdk comments list 12345
zdk comments list 12345 --public-only
zdk comments list 12345 --include-inline-images
zdk comments get 12345 --comment 98765
zdk comments redact 12345 --comment 98765 --text "4111-1111-1111-1111"
zdk comments redact-attachment 12345 --comment 98765 --attachment 4567
zdk comments make-private 12345 --comment 98765
zdk comments count 12345

zdk audits list 12345                     # paginated with the audits cursor dialect
zdk audits get 12345 --audit 555
zdk audits list-all --all                 # account-wide audit stream (cursor)
zdk audits count 12345

zdk metrics get 12345                     # ticket metrics
zdk metrics list --all
zdk metric-events list --start-time 2026-09-01     # incremental metric events
zdk activities list
```

### 8.3 Requests — the customer-facing view

The Requests API is the end user's perspective on a ticket: only public comments and a restricted field set. Critical for building customer-facing tooling, and absent from every existing CLI.

```bash
zdk requests list                                  # requests for the authenticated end user
zdk requests list --status open --organization-requests
zdk requests list --ccd                            # requests where the user is CC'd
zdk requests get 12345
zdk requests create --subject "Cannot log in" --comment "…" \
  --requester-name "Jane" --requester-email "jane@acme.com"
zdk requests update 12345 --comment "Additional detail" --solved
zdk requests comments list 12345
zdk requests comments get 12345 --comment 99
zdk requests search "billing"
zdk requests --on-behalf-of "jane@acme.com" list   # admin impersonation of end-user view
```

### 8.4 Users

```bash
zdk users list --all
zdk users list --role agent,admin
zdk users list --role end-user --organization "Acme Corp"
zdk users list --group "Platform Support"
zdk users list --external-id CRM-9912
zdk users search "jane@acme.com"
zdk users search --external-id CRM-9912
zdk users autocomplete "jan"
zdk users get 456
zdk users get 456 --sideload organizations,identities,roles,groups
zdk users me
zdk users related 456                          # ticket counts, org membership
zdk users create --name "Jane Doe" --email "jane@acme.com" --role end-user \
  --organization "Acme Corp" --custom-field "Tier=gold" --verified
zdk users create-or-update --email "jane@acme.com" --name "Jane Doe"
zdk users update 456 --name "Jane Smith" --suspended
zdk users merge --source 456 --target 789
zdk users delete 456 --yes
zdk users permanently-delete 456 --yes
zdk users bulk-create --file users.ndjson --wait
zdk users bulk-update --from ids.txt --field organization_id=123 --wait
zdk users bulk-create-or-update --file users.ndjson --wait
zdk users set-password 456 --password-prompt
zdk users request-password-change 456

zdk users identities list 456
zdk users identities add 456 --type email --value "jane.alt@acme.com"
zdk users identities verify 456 --identity 22
zdk users identities make-primary 456 --identity 22
zdk users identities delete 456 --identity 22

zdk users sessions list 456
zdk users sessions delete 456 --session 99
zdk users sessions logout 456                 # invalidate all
zdk users skills list 456                     # skill-based routing attributes
zdk users compliance-deletion-statuses 456
```

### 8.5 Organizations

```bash
zdk orgs list --all
zdk orgs get 123 --sideload users,tickets
zdk orgs search --external-id ACME-1
zdk orgs autocomplete "acm"
zdk orgs create --name "Acme Corp" --domain acme.com --domain acme.co.uk \
  --tag enterprise --custom-field "Region=EMEA" --shared-tickets
zdk orgs update 123 --add-tag renewal-q4
zdk orgs delete 123 --yes
zdk orgs bulk-create --file orgs.ndjson --wait
zdk orgs bulk-update --from ids.txt --field group_id=55 --wait
zdk orgs related 123
zdk orgs tickets 123
zdk orgs users 123
zdk orgs memberships list --organization 123
zdk orgs memberships add --user 456 --organization 123 --default
zdk orgs memberships delete --user 456 --organization 123
zdk orgs subscriptions list --organization 123
```

### 8.6 Groups, roles, brands, agents

```bash
zdk groups list [--all] [--assignable]
zdk groups get 55 --sideload users
zdk groups create --name "Tier 2" --description "Escalations"
zdk groups update 55 --name "Tier 2 — Platform"
zdk groups delete 55 --yes
zdk groups members list 55
zdk groups members add 55 --user 456 --default
zdk groups members delete 55 --user 456
zdk groups assignable list

zdk roles list                                  # custom agent roles
zdk roles get 9
zdk brands list
zdk brands get 3
zdk brands create --name "OSO Support" --subdomain oso-support
zdk brands update 3 --active
zdk brand-agents list --brand 3
zdk brand-agents add --brand 3 --agent 456
zdk locales list
zdk locales get en-GB
zdk locales best-match --accept "en-GB,en;q=0.9"
```

### 8.7 Views

```bash
zdk views list [--all] [--active] [--group 55] [--personal]
zdk views get 77
zdk views execute 77                            # respects 5 req/min/view/agent
zdk views execute 77 --all --resolve-names
zdk views export 77 --to open-tickets.csv       # high-throughput export endpoint
zdk views count 77
zdk views count-many 77,78,79
zdk views tickets 77 --all
zdk views preview --file view-definition.json   # preview before creating
zdk views create --file view.json
zdk views update 77 --file view.json
zdk views delete 77 --yes
zdk views bulk-delete --from ids.txt --yes
zdk views reorder --file order.json
zdk views search "unassigned"
```

### 8.8 Search

```bash
zdk search "type:ticket status:open priority:urgent"
zdk search "type:ticket created>2026-09-01" --all      # warns at the 1,000 cap
zdk search "type:user role:agent"
zdk search "type:organization tags:enterprise"
zdk search count "type:ticket status:pending"
zdk search export "type:ticket status:open" --all      # cursor-paginated, no 1,000 cap
zdk search export "type:ticket" --filter-type ticket --to tickets.ndjson
zdk search unified "kafka"                             # tickets + HC + community
zdk search explain "status:open assignee:me"           # show the resolved query string
```

Search-specific behaviours the CLI must implement:

- Warn on stderr when a `search` result set is truncated at 1,000 and recommend `search export`.
- Never trust `count` as a loop terminator; detect the documented case where `next_page` links to a failing page and stop cleanly.
- Note the eventual-consistency lag in `--help` and in the warning shown when a search immediately follows a write in the same session.
- `--filter-type` is mandatory for `search export` when the query does not constrain type.

### 8.9 Business rules — macros, triggers, automations, SLAs

```bash
# Macros
zdk macros list [--all] [--active] [--category "Refunds"] [--group 55] [--personal]
zdk macros get 200
zdk macros show 200                             # rendered actions, names resolved
zdk macros apply-preview --ticket 12345 --macro 200
zdk macros create --file macro.yaml
zdk macros update 200 --file macro.yaml
zdk macros delete 200 --yes
zdk macros bulk-destroy --from ids.txt --yes
zdk macros categories list
zdk macros attachments list 200
zdk macros usage 200                            # where referenced

# Triggers
zdk triggers list [--all] [--active] [--category "Notifications"]
zdk triggers get 300
zdk triggers show 300                           # human-readable conditions/actions
zdk triggers create --file trigger.yaml
zdk triggers update 300 --file trigger.yaml
zdk triggers activate 300 / deactivate 300
zdk triggers delete 300 --yes
zdk triggers reorder --file order.json
zdk triggers revisions list 300                 # Enterprise revision history
zdk triggers categories list
zdk triggers definitions                        # valid conditions and actions
zdk triggers test 300 --ticket 12345            # dry-run evaluation

# Automations
zdk automations list [--all] [--active]
zdk automations get 400
zdk automations create --file automation.yaml
zdk automations update 400 --file automation.yaml
zdk automations delete 400 --yes
zdk automations definitions

# SLA and schedules
zdk sla list
zdk sla get 500
zdk sla create --file sla.yaml
zdk sla update 500 --file sla.yaml
zdk sla delete 500 --yes
zdk sla reorder --file order.json
zdk sla group-policies list                     # group SLA policies
zdk schedules list
zdk schedules get 12
zdk schedules create --name "UK business hours" --timezone "Europe/London"
zdk schedules workweek set 12 --file workweek.json
zdk schedules holidays list 12
zdk schedules holidays add 12 --name "Christmas" --start 2026-12-25 --end 2026-12-26
```

### 8.10 Fields, forms, statuses, tags

```bash
zdk fields list                                 # ticket fields — the only source of field titles
zdk fields get 1900001
zdk fields create --type dropdown --title "Environment" \
  --option "production" --option "staging"
zdk fields update 1900001 --title "Env"
zdk fields delete 1900001 --yes
zdk fields reorder --file order.json
zdk fields options list 1900001                 # custom field options
zdk fields count

zdk user-fields list
zdk user-fields create --type text --key tier --title "Tier"
zdk org-fields list
zdk org-fields create --type text --key region --title "Region"

zdk forms list [--active]
zdk forms get 700
zdk forms create --file form.yaml
zdk forms update 700 --file form.yaml
zdk forms clone 700 --name "Incident (EU)"
zdk forms reorder --file order.json
zdk forms statuses 700                          # ticket form statuses

zdk statuses list                               # custom ticket statuses
zdk statuses create --category pending --agent-label "Awaiting parts" \
  --end-user-label "In progress"
zdk statuses update 800 --active
zdk statuses default 800

zdk tags list --all
zdk tags count
zdk tags autocomplete "esc"
zdk tags set --ticket 12345 --tags a,b,c
zdk tags add --ticket 12345 --tags escalated
zdk tags remove --ticket 12345 --tags triage
zdk tags rename --from old-tag --to new-tag --dry-run   # bulk retag across tickets
```

### 8.11 Side conversations

Absent from every existing CLI and still an open request in the Python SDK.

```bash
zdk side-conversations list --ticket 12345
zdk side-conversations get --ticket 12345 --id abc123
zdk side-conversations create --ticket 12345 \
  --to "vendor@supplier.com" --subject "RMA request" --body "…" \
  --attach ./serial.png
zdk side-conversations create --ticket 12345 --channel slack --to "#platform-oncall" --body "…"
zdk side-conversations create --ticket 12345 --channel msteams --to "Support" --body "…"
zdk side-conversations reply --ticket 12345 --id abc123 --body "Thanks"
zdk side-conversations update --ticket 12345 --id abc123 --state closed
zdk side-conversations events --ticket 12345 --id abc123
zdk side-conversations events-stream --start-time 2026-09-01   # incremental, 600/10min
zdk side-conversations attachments upload --ticket 12345 --id abc123 --file ./doc.pdf
```

Rate-limit awareness: create/reply is 300 per 10 minutes, event polling 600 per 10 minutes. The governor holds separate buckets for both.

### 8.12 Satisfaction and CSAT

```bash
zdk satisfaction list [--all] [--score good,bad] [--start-time 2026-09-01]
zdk satisfaction get 900
zdk satisfaction create --ticket 12345 --score good --comment "Fast fix"
zdk satisfaction reasons list
zdk csat surveys list
zdk csat surveys get 5
zdk csat responses list --survey 5 --all
zdk csat responses export --survey 5 --to csat.csv
zdk csat summary --from 2026-08-01 --to 2026-08-31    # computed locally, not an API call
```

### 8.13 Suspended and deleted tickets

```bash
zdk suspended list --all
zdk suspended get 1001
zdk suspended recover 1001
zdk suspended recover-many --from ids.txt
zdk suspended delete 1001 --yes
zdk suspended delete-many --from ids.txt --yes
zdk suspended attachments 1001
zdk suspended export --to suspended.ndjson       # for spam-rule tuning

zdk deleted list --all
zdk deleted restore 12345
zdk deleted restore-many --from ids.txt
zdk deleted purge 12345 --yes
zdk deleted purge-many --from ids.txt --yes
```

### 8.14 Attachments and uploads

```bash
zdk attachments get 4567
zdk attachments download 4567 --to ./file.png
zdk attachments download-all --ticket 12345 --to ./ticket-12345/
zdk attachments download-all --ticket 12345 --dedupe-filenames   # handles collisions
zdk attachments upload ./file.png                 # returns an upload token
zdk attachments upload ./a.png ./b.png --json     # multiple, one token
zdk attachments redact 4567 --ticket 12345 --comment 98765
zdk attachments update 4567 --malware-scan-result-override
zdk attachments content 4567                      # streams to stdout
```

Behaviours: upload tokens expire after 60 minutes, so `--attach` on a reply uploads and attaches in a single operation rather than leaving a dangling token. Filename collisions inside one ticket are resolved with a numeric suffix under `--dedupe-filenames`. Unattached uploads are subject to a 10 req/min limit, which the governor knows about.

### 8.15 Help Center / Guide (customer-facing content)

```bash
# Articles
zdk hc articles list --all [--locale en-gb] [--section 10] [--category 2] [--label-name faq]
zdk hc articles get 5001 [--locale en-gb]
zdk hc articles create --title "Kafka backup runbook" --body-file runbook.html \
  --section 10 --locale en-gb --draft --label faq --label kafka
zdk hc articles create --body-file runbook.md --markdown   # local conversion
zdk hc articles update 5001 --body-file updated.html
zdk hc articles publish 5001 / unpublish 5001
zdk hc articles archive 5001
zdk hc articles delete 5001 --yes
zdk hc articles bulk-publish --from ids.txt
zdk hc articles search "kafka backup"
zdk hc articles export --to ./kb/ --format markdown --all   # full KB to disk
zdk hc articles import --from ./kb/ --section 10 --dry-run   # docs-as-code round trip
zdk hc articles labels list 5001
zdk hc articles labels add 5001 --name kafka
zdk hc articles attachments list 5001
zdk hc articles attachments upload 5001 --file diagram.png --inline
zdk hc articles comments list 5001
zdk hc articles comments delete 5001 --comment 77 --yes
zdk hc articles votes 5001
zdk hc articles subscriptions list 5001
zdk hc articles translations list 5001
zdk hc articles translations create 5001 --locale fr --title "…" --body-file fr.html
zdk hc articles translations missing 5001

# Structure
zdk hc sections list [--category 2] [--all]
zdk hc sections create --name "Runbooks" --category 2 --locale en-gb
zdk hc sections update 10 --name "Operations runbooks"
zdk hc sections reorder --file order.json
zdk hc categories list
zdk hc categories create --name "Platform" --locale en-gb
zdk hc user-segments list
zdk hc permission-groups list
zdk hc content-tags list
zdk hc content-tags create --name "kafka"
zdk hc medias list
zdk hc redirect-rules list

# Community
zdk community topics list
zdk community posts list --all [--topic 3]
zdk community posts create --title "…" --details "…" --topic 3
zdk community posts comments list 6001
zdk community posts votes 6001

# Themes and federated search
zdk hc themes list
zdk hc themes get abc
zdk hc themes import --file theme.zip
zdk hc themes publish abc
zdk hc external-content sources list
zdk hc external-content types list
zdk hc external-content records create --file records.ndjson  # 770 records/min limit
zdk hc search "kafka" --locale en-gb
zdk hc unified-search "kafka"
```

### 8.16 Talk / Voice

```bash
zdk talk stats
zdk talk stats current-queue
zdk talk stats agents-activity
zdk talk stats account-overview
zdk talk availability list
zdk talk availability set --agent 456 --via phone
zdk talk calls list --all
zdk talk calls get 7001
zdk talk calls recording 7001 --to ./call.mp3
zdk talk calls legs 7001
zdk talk phone-numbers list
zdk talk phone-numbers search --country GB --area-code 20
zdk talk phone-numbers create --number "+442012345678"
zdk talk digital-lines list
zdk talk greetings list
zdk talk greetings upload --file greeting.mp3 --category voicemail
zdk talk ivrs list
zdk talk ivrs menus list 12
zdk talk ivrs routes list 12 --menu 3
zdk talk addresses list
zdk talk callback-requests list
zdk talk settings get / set --field recording_enabled:=true
zdk talk incremental calls --start-time 2026-09-01
zdk talk incremental legs --start-time 2026-09-01
```

### 8.17 Live Chat

```bash
zdk chat chats list --all
zdk chat chats get abc-123
zdk chat chats search "refund"
zdk chat chats delete abc-123 --yes
zdk chat agents list
zdk chat agents get 456
zdk chat departments list / create / update / delete
zdk chat triggers list / create / update / delete
zdk chat shortcuts list / create / update / delete
zdk chat goals list
zdk chat bans list / create --visitor-id v-1 / delete
zdk chat visitors list / get / update
zdk chat roles list
zdk chat skills list
zdk chat routing-settings get / set
zdk chat account get
zdk chat incremental chats --start-time 2026-09-01
zdk chat incremental agent-events --start-time 2026-09-01
zdk chat incremental agent-timeline --start-time 2026-09-01
```

Note in `--help`: Chat has a fixed **200 req/min** limit on every plan, separate from the Support budget, and Chat product tokens are **not** part of the API token retirement.

### 8.18 Omnichannel routing and agent availability

```bash
zdk routing queues list
zdk routing queues get 1
zdk routing queues create --file queue.yaml
zdk routing queues update 1 --file queue.yaml
zdk routing queues reorder --file order.json
zdk routing queues percentage-based list
zdk routing agent-statuses list                 # unified agent statuses
zdk routing agent-statuses create --name "In training" --category away
zdk routing availability list                   # 300 req/min
zdk routing availability get --agent 456
zdk routing work-items --agent 456 --channel messaging
zdk routing capacity-rules list / create / update / delete
zdk routing engagements list
zdk routing queue-events --start-time 2026-09-01
zdk routing skills list                         # skill-based routing
zdk routing skills attributes list
zdk routing skills attribute-values list
zdk routing skills ticket-attributes 12345
zdk routing skills incremental instance-values --start-time 2026-09-01
```

### 8.19 Custom objects

Absent from every existing CLI and still unimplemented in Zendesk's own PHP client.

```bash
zdk objects list                                # object type definitions
zdk objects get asset
zdk objects create --key asset --title Asset --title-pluralized Assets
zdk objects delete asset --yes
zdk objects fields list asset
zdk objects fields create asset --type text --key serial --title "Serial"
zdk objects records list asset --all
zdk objects records get asset rec_123
zdk objects records create asset --field serial=SN-001 --field name="Laptop"
zdk objects records update asset rec_123 --field status=retired
zdk objects records delete asset rec_123 --yes
zdk objects records search asset "laptop"
zdk objects records filter asset --file filter.json
zdk objects records bulk-create asset --file records.ndjson --wait
zdk objects records bulk-delete asset --from ids.txt --wait
zdk objects records attachments list asset rec_123
zdk objects records events asset rec_123
zdk objects permissions get asset
zdk objects triggers list asset
zdk objects limits
zdk objects incremental asset --start-time 2026-09-01
zdk lookup-relationships sources --ticket 12345
```

`--help` must state the plan limits: 3/5/30/50/50 custom object types by plan tier, 32 KB per object.

### 8.20 Webhooks, targets, integrations

```bash
zdk webhooks list --all
zdk webhooks get wh_1
zdk webhooks create --name "Alert bridge" --endpoint https://oso.sh/hook \
  --subscription "conditional_ticket_events" --auth bearer --secret-prompt
zdk webhooks update wh_1 --active
zdk webhooks delete wh_1 --yes
zdk webhooks test wh_1 --payload-file test.json
zdk webhooks test-new --endpoint https://oso.sh/hook --payload-file test.json
zdk webhooks invocations list wh_1
zdk webhooks invocation-attempts wh_1 --invocation inv_1
zdk webhooks signing-key show / rotate
zdk webhooks event-types
zdk webhooks clone wh_1 --to-profile production   # cross-instance copy

zdk targets list                                  # legacy targets
zdk target-failures list
zdk zis bundles list / install --file bundle.json
zdk zis integrations list / create
zdk zis connections list
zdk zis job-specs list / install
zdk zis inbound-webhooks list
```

### 8.21 Jobs — bulk orchestration made safe

```bash
zdk jobs list                                     # last 100 job statuses
zdk jobs get JOB_ID
zdk jobs watch JOB_ID                             # poll with backoff, live progress
zdk jobs wait JOB_ID --timeout 600
zdk jobs results JOB_ID                           # per-record success/failure
zdk jobs failures JOB_ID -o csv                   # only the failures, for retry
zdk jobs retry JOB_ID                             # resubmit only failed records
zdk jobs budget                                   # inflight-jobs remaining / resets
zdk jobs cancel JOB_ID
```

Requirements:

- Never exceed the documented **30 concurrent jobs**; queue locally and report position rather than triggering `TooManyJobs`.
- Poll with exponential backoff, since polling consumes the per-minute request budget.
- Persist job IDs and results locally in SQLite immediately, because Zendesk only retains the last 100 job statuses for a short window — a `zdk jobs results` an hour later must still work.
- Always report partial failures explicitly with a non-zero exit code and a machine-readable failure list.

### 8.22 Incremental sync and exports

```bash
zdk sync tickets --to ./data/tickets/ --format ndjson
zdk sync tickets --resume                         # continues from the stored cursor
zdk sync tickets --start-time 2026-01-01 --cursor-mode
zdk sync users --to ./data/users/ --resume
zdk sync organizations --resume
zdk sync ticket-events --resume
zdk sync ticket-metric-events --resume
zdk sync ticket-fields --resume
zdk sync nps-responses --resume
zdk sync custom-objects asset --resume
zdk sync all --to ./data/ --resume                # every incremental endpoint
zdk sync status                                   # per-stream high-water marks
zdk sync reset tickets --yes
zdk sync verify tickets                           # re-fetch a sample and diff
```

Design requirements — this is where most in-house scripts are wrong:

1. **Cursor mode by default.** `after_cursor` is the durable high-water mark, persisted in SQLite after every successful page write, so a killed process resumes without loss or replay.
2. **Never loop on `count`.** Terminate on `end_of_stream`/`has_more`, never on a record count.
3. **Deduplicate.** Track `(id, generated_timestamp)` in a rolling window and drop repeats — duplicates are documented behaviour, not a bug.
4. **Respect the one-minute exclusion window** in time-based mode and never re-request the same `start_time` without advancing, which is the documented infinite-loop trap.
5. **Hold a dedicated 10 req/min bucket** for `/api/v2/incremental/*` (30 with High Volume) independent of the account budget.
6. **Write-ahead output.** Records are flushed to disk before the cursor advances, so the on-disk data is never ahead of the checkpoint.
7. **Honour the guidance** that incremental export is not for continuous syncing: `--interval` below 5 minutes prints a warning explaining why.
8. **Sideload support** where the endpoint allows it, to avoid a second pass for users and organizations.

### 8.23 Backup and restore

```bash
zdk backup create --to ./backup-2026-09-10/ \
  --include tickets,users,organizations,groups,macros,triggers,automations,views,sla,forms,fields,hc
zdk backup create --to s3://oso-backups/zendesk/ --incremental --since-last
zdk backup manifest ./backup-2026-09-10/          # counts, checksums, spec version
zdk backup verify ./backup-2026-09-10/
zdk backup restore ./backup-2026-09-10/ --profile sandbox --dry-run
zdk backup restore ./backup-2026-09-10/ --profile sandbox --only macros,triggers,views
zdk backup diff ./backup-a/ ./backup-b/
```

Reality check to state in the docs: a full export of a large instance is bounded by the 10 req/min incremental limit and 1,000 records per page — roughly 600,000 records/hour at best. Teams have reported multi-week timelines for tens of millions of tickets. `zdk backup create --estimate` prints a projected wall-clock time before starting.

### 8.24 GDPR, redaction, audit

```bash
zdk gdpr delete-user 456 --yes                    # compliance deletion
zdk gdpr status 456                               # deletion status across products
zdk gdpr redact-ticket 12345 --text "4111111111111111"
zdk gdpr redact-comment 12345 --comment 98765 --text "…"
zdk gdpr redact-attachment 12345 --comment 98765 --attachment 4567
zdk gdpr checklist 456                            # ordered multi-product workflow
zdk gdpr export-user 456 --to ./subject-access-456/    # DSAR bundle
zdk deletion-schedules list / create / update / delete
zdk audit-logs list --all --filter "action=update"
zdk audit-logs get 999
zdk audit-logs export --from 2026-08-01 --to 2026-08-31 --to-file audit.ndjson
zdk access-logs list                              # account access log
```

`zdk gdpr checklist` is a genuine differentiator: redaction is irreversible, must be performed in a specific order, cannot be applied to closed tickets, and leaves residue in Chat, Talk, and Explore. The command prints the ordered steps, flags which are impossible for the given ticket state, and requires explicit confirmation for each irreversible action.

### 8.25 Configuration-as-code (flagship differentiator)

Nothing in the ecosystem does this. Zendesk has no export/import/versioning story for business rules, no way to answer "what uses field X?", and sandbox promotion is one-way, unschedulable, unrevertable, and capped at 100 dependencies — with sandboxes gated to higher plans in the first place.

```bash
# Export the whole configuration to canonical, diffable YAML
zdk rules export --to ./zendesk-config/
zdk rules export --to ./zendesk-config/ \
  --include triggers,automations,macros,views,sla,forms,fields,statuses,webhooks,schedules,groups,brands,roles,user-segments
zdk rules export --to ./zendesk-config/ --symbolic-refs   # names, not numeric IDs

# Plan and apply, Terraform-style
zdk rules plan  --from ./zendesk-config/
zdk rules plan  --from ./zendesk-config/ --profile production
zdk rules apply --from ./zendesk-config/ --profile production
zdk rules apply --from ./zendesk-config/ --only triggers --yes
zdk rules validate ./zendesk-config/              # schema + reference integrity

# Cross-instance diff and promotion
zdk diff --source sandbox --target production
zdk diff --source sandbox --target production --only triggers,macros -o json
zdk promote --from sandbox --to production --only triggers --dry-run
zdk promote --from sandbox --to production --file changeset.yaml

# Impact analysis — "what uses this?"
zdk refs field 1900001                            # triggers/macros/views/forms referencing it
zdk refs group 55
zdk refs macro 200
zdk refs tag escalated
zdk refs orphans                                  # rules referencing deleted entities
zdk lint                                          # inactive rules, duplicate conditions,
                                                  # unreachable triggers, overlapping SLAs
```

Design requirements:

- **Canonical YAML** with deterministic key ordering, so `git diff` is meaningful.
- **Symbolic references** — `group: "Tier 2"` rather than `group_id: 55` — resolved at apply time per target instance, which is what makes sandbox-to-production promotion actually work.
- **Plan output** is a unified diff plus a change summary, showing creates/updates/deletes and any references that cannot be resolved on the target.
- **Apply is transactional per resource type** with a rollback journal; a failed apply prints the exact commands to revert.
- **Dependency ordering** — categories before triggers, fields before forms, groups before views.
- **Position/ordering is a first-class property.** Trigger execution order matters and must round-trip.
- **Never invent an API.** Where Zendesk offers no update endpoint, `plan` says so explicitly rather than silently skipping.

### 8.26 Schema introspection and name resolution

```bash
zdk schema refresh                     # cache fields, forms, groups, brands, statuses, locales
zdk schema fields                      # id ↔ title ↔ key mapping
zdk schema resolve field "Environment" # → 1900001
zdk schema resolve group 55            # → "Tier 2"
zdk schema resolve user "jane@acme.com"
zdk schema dump -o json                # entire cached schema
zdk schema stale                       # what needs refreshing
```

This exists because, as the community puts it, "the only end point that will expose field title is `ticket_fields`". Everything else is numeric IDs. With `--resolve-names`, `zdk tickets list` renders `Environment: production` instead of `1900001: production`, and filters accept names on input. The cache is a local SQLite table with a TTL, refreshed automatically when a lookup misses.

### 8.27 Rate limits, diagnostics, config

```bash
zdk rate-limit status                  # account + per-endpoint budgets, live
zdk rate-limit history --since 1h
zdk rate-limit test                    # one probe call, reports headers verbatim
zdk rate-limit plan --operation "tickets list --all"   # projected calls and wall time

zdk doctor                             # auth, clock skew, TLS, scopes, plan detection,
                                       # token deadline countdown, cache health
zdk config init
zdk config show [--reveal-secrets]
zdk config set output.format json
zdk config get rate_limit.strategy
zdk config edit                        # $EDITOR
zdk config validate
zdk config profiles list / add / remove / rename / switch
zdk sandbox list                       # available sandboxes
zdk sandbox create --name "release-test"
zdk completions bash|zsh|fish|powershell|elvish
zdk man --to ./man/
zdk version --check-updates
zdk cache clear / stats
zdk api ops [--grep tickets]           # every generated operation
zdk api describe UpdateManyTickets     # schema for one operation
```

### 8.28 MCP server mode

```bash
zdk mcp serve --stdio
zdk mcp serve --http --port 8931 --profile production
zdk mcp serve --stdio --readonly --scopes tickets:read,users:read
zdk mcp tools                          # list exposed tools
```

One binary, two interfaces. The MCP surface exposes curated, safety-bounded tools (search tickets, read thread, draft reply, apply macro, look up user) over the same core, with `--readonly` and scope narrowing so an agent can be given strictly less power than the operator. This is deliberate: existing Zendesk MCP servers are separately maintained projects with acknowledged gaps — no Talk/Chat coverage, no CSAT, no timing metrics, no followers/CCs. Sharing `zdk-core` means the MCP surface inherits full coverage for free.

---

## 9. Pagination Engine

A single abstraction with four documented dialects behind it:

| Dialect | Applies to | Mechanism |
|---|---|---|
| **Cursor** | Most list endpoints | `page[size]` ≤ 100; follow `links.next`; stop on `meta.has_more == false` |
| **Offset** | Endpoints without cursor support | `page` + `per_page`; **hard stop at 100 pages / 10,000 records** |
| **Link header** | Help Center | `Link: <…>; rel="next"`; supports `prev` and `links.last` |
| **Incremental** | `/api/v2/incremental/*` | `after_cursor` or `start_time`/`end_time`; 1,000 records/page; duplicates expected |
| **Audits** | `/api/v2/ticket_audits` | Cursor variant with its own field names |

Behaviours:

- `--all` selects the best dialect available for the endpoint automatically and prefers cursor.
- When an endpoint only supports offset and the walk would exceed 10,000 records, the CLI **fails early with a specific message** naming the alternative (`search export`, an incremental endpoint, or a view export) rather than letting the user discover a `400` at page 101.
- `--sort-by` on a cursor-paginated endpoint that does not honour sorting prints a warning that ordering will be ignored, instead of silently returning unordered data.
- NDJSON output streams as pages arrive; memory stays flat regardless of result size.
- Progress on stderr: records fetched, pages, elapsed, current rate, and ETA where `count` is available and trustworthy.
- `--limit` short-circuits mid-page without fetching further pages.
- A resumable `--checkpoint <file>` writes the last cursor so an interrupted large list can continue.

---

## 10. Rate Limiting and Concurrency

### 10.1 The Governor

```rust
struct RateGovernor {
    account: GcraLimiter,                       // plan-derived, e.g. 700/min
    endpoint: HashMap<EndpointKey, GcraLimiter>, // per §4.2 static rules
    inflight_jobs: Semaphore,                    // 30
    concurrency: Semaphore,                      // --max-concurrency, default 4
    ledger: SqliteLedger,                        // rolling usage history
}
```

### 10.2 Behaviours

- **Plan auto-detection.** On first run, probe `X-Rate-Limit` from a cheap call and set the account bucket accordingly; cache per profile. Never hard-code 700.
- **Header truth beats local arithmetic.** Because accounts can carry undocumented conditional limits that 429 while `remaining` still reads in the hundreds, `Retry-After` always wins over the local bucket's opinion.
- **Per-endpoint buckets** for the tight ones: incremental (10/min), view execute (5/min/view/agent), user update (5/min/user), org update (5/min/org), search export (100/min), side conversations (300/10min), side-conversation events (600/10min), ticket update (100/min account, 30/10min per user per ticket), unattached upload fetch (10/min).
- **Sub-budget headers** parsed and respected: `zendesk-ratelimit-tickets-index`, `zendesk-ratelimit-inflight-jobs`.
- **Separate Help Center and Chat budgets**, since Help Center requests do not consume the Support budget and Chat is a flat 200/min.
- **429 handling:** honour `Retry-After` exactly, then exponential backoff with full jitter (base 1 s, cap 60 s, 6 attempts). Retry `5xx` and connection errors; never blind-retry a non-idempotent `POST` without an idempotency key.
- **Strategies:** `wait` (default — pause with a progress indicator and continue), `fail` (exit 7 immediately, for CI that must not hang), `burst` (ignore local buckets, rely purely on server 429s — for High Volume accounts).
- **Reserve headroom.** `rate_limit.reserve_percent` (default 10) keeps quota free so an agent using the UI is not starved by a bulk job.
- **Long-operation etiquette.** Any operation projected to exceed 60 seconds prints a one-line plan first: estimated calls, estimated wall time, and budget impact. `--yes` skips the confirmation.
- **`--dry-run` consumes zero quota** and prints the exact HTTP requests.

---

## 11. Bulk Operations and Idempotency

- Batches auto-chunk to the documented **100-record** maximum.
- `--dry-run` prints exactly which records would change, with a per-record diff where the current state is known. Zendesk has no dry-run; the CLI simulates one by fetching current state first.
- All writes send an `Idempotency-Key` by default (auto-generated UUID v4, stable across retries of the same logical operation), closing the gap that is still open in Zendesk's own Ruby client. Zendesk treats a duplicate key as new after two hours, which the CLI documents and factors into retry windows.
- Async endpoints return job statuses; the orchestrator submits, records the job ID locally, polls with backoff, and reconciles per-record results.
- **Partial failure is never silent.** Exit code 8, a failure summary on stderr, and a machine-readable failure list on stdout. `zdk jobs retry` resubmits only the failures.
- `--continue-on-error` for large sweeps, with a final report.
- `--max-batch-concurrency` bounded by the inflight-jobs budget.

---

## 12. Output Formatting

```bash
zdk tickets list                                   # table (TTY)
zdk tickets list | jq '.[].id'                     # JSON (piped)
zdk tickets list -o ndjson > tickets.ndjson        # streaming, one object per line
zdk tickets list -o csv > tickets.csv
zdk tickets list -o yaml
zdk tickets list -o raw                            # untouched API response
zdk tickets list -o tsv
zdk tickets list --fields id,subject,status,assignee.name,organization.name
zdk tickets list --exclude description,raw_subject
zdk tickets list --jq '.[] | select(.priority=="urgent") | .id'
zdk tickets list --resolve-names --fields id,subject,'custom.Environment'
```

- Dot-path projection works across sideloaded objects.
- `--resolve-names` replaces numeric IDs with names via the schema cache and exposes custom fields under a `custom.<Title>` namespace.
- Tables: sensible per-resource default columns, terminal-width-aware truncation with an ellipsis, right-aligned numerics, colourised status and priority when colour is enabled.
- Timestamps render in the local timezone in tables and as RFC 3339 UTC in machine formats.
- `NO_COLOR` and `--no-color` respected; colour disabled automatically when not a TTY.
- Everything non-data — progress, warnings, prompts, rate-limit notices — goes to stderr.

---

## 13. Configuration

### 13.1 Config file

`~/.config/zendesk-cli/config.toml` (XDG on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows)

```toml
[default]
active_profile = "production"
output = "table"
page_size = 100
color = true
resolve_names = false
confirm_destructive = true

[auth]
method = "authorization_code"      # authorization_code | client_credentials | api_token
callback_port = 8080
auto_refresh = true
refresh_at_percent = 80            # refresh when 80% of TTL elapsed
credential_store = "auto"          # auto | keyring | file | env
suppress_deprecation = false

[rate_limit]
strategy = "wait"                  # wait | fail | burst
max_concurrency = 4
reserve_percent = 10
warn_threshold = 50
respect_retry_after = true
high_volume_addon = false

[retry]
max_attempts = 6
base_ms = 1000
max_ms = 60000
jitter = "full"
retry_on = [429, 500, 502, 503, 504]

[cache]
enabled = true
schema_ttl_seconds = 3600
list_ttl_seconds = 60
directory = "~/.cache/zendesk-cli"
max_size_mb = 500

[sync]
state_dir = "~/.local/state/zendesk-cli"
dedupe_window = 10000
output_format = "ndjson"
min_interval_seconds = 300

[jobs]
poll_base_ms = 2000
poll_max_ms = 30000
max_concurrent = 30
persist_results = true

[rules]
manifest_dir = "./zendesk-config"
symbolic_refs = true

[audit]
enabled = true
path = "~/.local/state/zendesk-cli/audit.ndjson"

[profiles.production]
subdomain = "oso"
client_id = "zdk_production"
grant_type = "authorization_code"
scopes = ["tickets:read", "tickets:write", "users:read", "organizations:read", "hc:read"]
plan = "enterprise"

[profiles.sandbox]
subdomain = "oso1234567890"
client_id = "zdk_sandbox"
grant_type = "authorization_code"
scopes = ["read", "write"]

[profiles.ci]
subdomain = "oso"
client_id = "zdk_ci"
grant_type = "client_credentials"
scopes = ["tickets:read", "users:read", "auditlogs:read"]
```

### 13.2 Environment Variables

```bash
ZENDESK_SUBDOMAIN            # Instance subdomain
ZENDESK_CLIENT_ID            # OAuth client identifier
ZENDESK_CLIENT_SECRET        # OAuth client secret (confidential clients)
ZENDESK_ACCESS_TOKEN         # Direct access token, bypasses the store
ZENDESK_REFRESH_TOKEN        # Refresh token for non-interactive refresh
ZENDESK_SCOPES               # Comma-separated granular scopes
ZENDESK_EMAIL                # Legacy API token auth
ZENDESK_API_TOKEN            # Legacy API token auth (deprecated)
ZENDESK_PROFILE              # Active profile
ZENDESK_CONFIG               # Config file path
ZENDESK_OUTPUT               # Default output format
ZENDESK_PAGE_SIZE
ZENDESK_MAX_CONCURRENCY
ZENDESK_RATE_LIMIT_STRATEGY
ZENDESK_NO_CACHE
ZENDESK_CREDENTIAL_STORE
ZENDESK_LOG                  # tracing filter, e.g. zdk=debug,reqwest=info
NO_COLOR
```

Precedence: CLI flag → environment variable → profile → `[default]` → built-in default.

---

## 14. Error Handling

### 14.1 Rich diagnostics (miette)

```
Error:   × Zendesk API returned 401 Unauthorized
  ├─▶ Access token expired 3 minutes ago
  ╰─▶ Profile: production (subdomain: oso)

  help: A refresh was attempted and failed — the refresh token may be revoked.
        Run `zdk auth login` to re-authenticate.
```

```
Error:   × Rate limit exceeded (429 Too Many Requests)
  ├─▶ Endpoint budget: GET /api/v2/incremental/tickets — 10 requests/minute
  ├─▶ Retry-After: 34s
  ╰─▶ Account budget: 612/700 remaining this minute

  help: Waiting 34s and resuming automatically (attempt 2 of 6).
        Incremental exports are limited to 10 req/min regardless of plan.
        The High Volume API add-on raises this to 30 req/min.
```

```
Error:   × Missing scope: triggers:write
  ├─▶ `zdk triggers update` requires triggers:write
  ╰─▶ Granted: tickets:read, tickets:write, users:read

  help: Run `zdk auth scopes add triggers:write` (this triggers re-authentication).
        If your OAuth client has Allowed scopes configured, add it there first:
        Admin Center → Apps and integrations → APIs → OAuth clients.
```

```
Error:   × Offset pagination limit reached (400 Bad Request)
  ├─▶ Requested page 101 of GET /api/v2/…
  ╰─▶ Zendesk caps offset pagination at 100 pages / 10,000 records

  help: This endpoint does not support cursor pagination. Use one of:
          zdk search export "type:ticket …" --all
          zdk sync tickets --resume
```

```
Error:   × Bulk update completed with failures
  ├─▶ Job 8f3a… : 1,847 succeeded, 53 failed
  ╰─▶ Most common: "Assignee: does not exist" (41 records)

  help: Full failure list written to ./zdk-failures-8f3a.csv
        Retry only the failures with `zdk jobs retry 8f3a…`
```

```
Warning: API token authentication is deprecated
  ├─▶ New API tokens cannot be created after 27 October 2026
  ╰─▶ All API tokens stop working on 30 April 2027 (232 days remaining)

  help: Run `zdk auth migrate` for a guided move to OAuth.
```

### 14.2 HTTP status handling

| Code | Behaviour |
|---|---|
| 400 | Surface Zendesk's validation details field-by-field; special-case the offset-pagination message with the cursor alternative; special-case `invalid_scope` |
| 401 | Attempt one refresh, retry once; on second failure prompt re-auth. Distinguish expired token from revoked client |
| 403 | Map to the missing granular scope or the missing agent permission; name both |
| 404 | Name the resource and ID; suggest `search` when the ID looks like a name |
| 409 | Conflict — surface the `updated_stamp` mismatch and suggest `--safe-update` |
| 422 | Unprocessable — render per-field errors; recognise the documented search `next_page` failure |
| 429 | Honour `Retry-After`, then jittered backoff; report which budget was exhausted |
| 500/502/503/504 | Retry with backoff up to 6 attempts, then fail with a link to [status.zendesk.com](https://status.zendesk.com) and offer `zdk api GET /api/incidents` |
| `TooManyJobs` | Queue locally, report position, resume — never fail outright |

### 14.3 Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic error |
| 2 | Usage / invalid arguments |
| 3 | Authentication failure |
| 4 | Authorisation / missing scope |
| 5 | Resource not found |
| 6 | Validation error (4xx from Zendesk) |
| 7 | Rate limited and strategy is `fail` |
| 8 | Bulk operation partial failure |
| 9 | Zendesk server error after retries |
| 10 | Local configuration error |
| 11 | Network / TLS error |
| 12 | Pagination limit reached |
| 13 | Job timed out |
| 130 | Interrupted (SIGINT) |

---

## 15. Multi-Instance and Sandbox Support

```bash
zdk config profiles list
┌────────────┬───────────────┬──────────────────────┬────────────┬───────────────┐
│ Profile    │ Subdomain     │ Auth                 │ Plan       │ Token expires │
├────────────┼───────────────┼──────────────────────┼────────────┼───────────────┤
│ production │ oso           │ authorization_code   │ enterprise │ in 22m        │
│ sandbox    │ oso123456789  │ authorization_code   │ enterprise │ in 27m        │
│ ci         │ oso           │ client_credentials   │ enterprise │ in 4m         │
│ client-a   │ acme          │ authorization_code   │ growth     │ expired       │
└────────────┴───────────────┴──────────────────────┴────────────┴───────────────┘

zdk config profiles switch sandbox
zdk --profile client-a tickets list --status open
zdk --all-profiles tickets count --status urgent -o csv
zdk --profile "production,sandbox" macros list
```

Sandbox workflow, addressing that native promotion is one-way, unschedulable, unrevertable, and dependency-capped:

```bash
zdk sandbox list
zdk sandbox create --name "release-test" --type premium
zdk diff --source sandbox --target production --only triggers,macros
zdk promote --from sandbox --to production --only triggers --dry-run
zdk promote --from sandbox --to production --file changeset.yaml --journal ./rollback.json
zdk promote rollback ./rollback.json
```

For teams on plans without sandboxes, `zdk rules plan` against production is the safety net: a full diff before anything is written.

---

## 16. Observability

- `tracing` with `ZENDESK_LOG` filtering; `-vvv` logs full request and response bodies with `Authorization`, `client_secret`, and token values redacted.
- `--audit-log` appends NDJSON: timestamp, profile, method, path, status, duration, rate-limit headers, request ID, bytes, and the invoking command. This doubles as the evidence base for `zdk auth migrate`'s least-privilege scope recommendation.
- `zdk rate-limit history` reads the SQLite ledger to show usage over time and which endpoints dominate.
- Every error includes Zendesk's request correlation ID where returned, so support escalations are actionable.
- `--metrics-out prometheus.txt` writes a textfile-collector-compatible dump after a run, for cron-driven exports.

---

## 17. Testing Strategy

### 17.1 Unit tests

- Model deserialisation for every generated type against committed fixtures.
- Rate limiter: bucket maths, header parsing (all six families), `Retry-After` precedence, sub-budget parsing.
- Pagination: each of the five dialects, including the 100-page guard and the documented `next_page`-failure case.
- Sync engine: cursor advance, dedupe window, exclusion-window handling, resume after simulated crash, and explicit regression tests for the same-`start_time` infinite loop.
- Config-as-code: canonical serialisation determinism, symbolic reference resolution, dependency ordering, plan diff correctness.
- Scope map: every command maps to at least one scope; no command requests an empty scope.
- Output: table width handling, CSV escaping, projection dot-paths, NDJSON streaming.
- Property tests (`proptest`) over cursor sequences and batch chunking.

### 17.2 Integration tests

`wiremock`-backed, no network:

- Full authorization-code + PKCE flow against a mock OAuth server, including the 120-second code expiry.
- Client credentials flow, and the absence of a refresh token.
- Token refresh at 80% TTL, refresh-token rotation, and rotation failure.
- 429 with `Retry-After`, 429 without it, and the conditional-limit case where `remaining` is high.
- Multi-page cursor walks, offset 400 at page 101, Help Center `Link` header walks.
- Incremental export with duplicate records and a mid-stream kill/resume.
- Job submission → polling → partial failure → retry-failures-only.
- `TooManyJobs` queueing.
- Attachment upload token expiry.
- Snapshot tests (`insta`) for every table, error message, and `plan` diff.

### 17.3 E2E tests

- Against a dedicated Zendesk sandbox, gated behind a repository secret and skipped by default.
- Smoke coverage: create ticket → comment → attach → macro → assign → solve → delete; user and org lifecycle; article publish/unpublish; a rules export/plan/apply round trip that must produce an empty second plan (idempotency proof).
- A nightly job that runs `xtask spec-diff` and fails if the committed OAS snapshots drift from upstream, catching Zendesk's 1-month standard change notice window.

### 17.4 Fixtures

```
tests/fixtures/
├── tickets/{list_cursor,list_offset,get,get_sideloaded,create,update_many_job}.json
├── incremental/{tickets_cursor_page1,tickets_cursor_page2,duplicate_record,end_of_stream}.json
├── jobs/{queued,working,completed,completed_with_failures,too_many_jobs}.json
├── errors/{401,403_scope,400_offset_limit,400_invalid_scope,422_search_next_page,429_retry_after,429_conditional}.json
├── headers/{rate_limit_full,tickets_index,inflight_jobs}.txt
├── help_center/{articles_link_header,translations}.json
├── rules/{triggers_export,macros_export,plan_expected.diff}.yaml
└── oauth/{token_response,refresh_rotation,invalid_scope}.json
```

---

## 18. Build and Distribution

### 18.1 Targets

```yaml
targets:
  - x86_64-unknown-linux-musl       # static
  - aarch64-unknown-linux-musl      # static
  - x86_64-unknown-linux-gnu
  - aarch64-apple-darwin
  - x86_64-apple-darwin
  - x86_64-pc-windows-msvc
  - aarch64-pc-windows-msvc
```

`rustls` only — no OpenSSL, so musl static builds are genuinely static and the container image can be `FROM scratch`.

### 18.2 Distribution

- **GitHub Releases** — signed binaries, SHA-256 checksums, SLSA provenance attestation.
- **Homebrew** — `brew install osodevops/tap/zendesk-cli`.
- **Cargo** — `cargo install zendesk-cli`.
- **Container** — `ghcr.io/osodevops/zendesk-cli:latest`, distroless, verified to work with the encrypted-file credential store (no D-Bus, no keychain).
- **Shell installer** — `curl -sSf https://oso.sh/zdk/install.sh | sh`.
- **Nix flake**, **AUR**, **Scoop** (Windows).
- **GitHub Action** — `osodevops/zendesk-cli-action@v1` for CI use.
- Man pages via `clap_mangen`; completions for bash, zsh, fish, PowerShell, elvish shipped in the release archive.

### 18.3 CI

```yaml
on: [push, pull_request]
jobs:
  check:    # fmt, clippy -D warnings, cargo-deny, cargo-audit, typos
  test:     # unit + integration on linux/macos/windows, stable + MSRV
  coverage: # cargo-llvm-cov, fail under 80%
  e2e:      # sandbox, main branch only, secret-gated
  spec:     # xtask spec-diff — PR on upstream OAS change
release:
  on: { push: { tags: ['v*'] } }
  # build matrix → sign → checksum → attest → publish → homebrew bump → crates.io → ghcr
```

---

## 19. Security Considerations

- **Credentials never touch disk in plaintext.** Keychain first; XChaCha20-Poly1305 + Argon2id file fallback at `0600`; environment variables for ephemeral CI.
- **Secrets are never logged**, never printed by `config show` without `--reveal-secrets`, and are redacted from `-vvv` output and from `--audit-log`.
- **Least privilege by default.** Scope presets are minimal; an empty scope request (which grants full read+write) is prohibited by construction.
- **TLS 1.2+ with SNI** enforced, per Zendesk's requirements; certificate pinning optional via config.
- **PKCE always** for public authorization-code clients; `state` always verified.
- **Loopback-only callback** bound to `127.0.0.1`, single-use, with a short timeout.
- **Destructive actions confirm** by default. `delete`, `purge`, `permanently-delete`, `gdpr delete-user`, `redact`, and `rules apply` require `--yes` or an interactive confirmation, and irreversible ones name what cannot be undone.
- **No telemetry.** The binary talks to Zendesk and nothing else. Update checks are opt-in.
- **Supply chain:** `cargo-deny` licence and advisory gates, `cargo-audit` in CI, dependency review on PRs, signed releases with provenance.
- **Multi-tenant safety:** the active profile is printed in the confirmation prompt for every destructive action, so a production instance is never mistaken for a sandbox.

---

## 20. Roadmap

| Version | Scope |
|---|---|
| **v0.1.0** | OAuth (authorization code + PKCE, client credentials), credential store with headless fallback, `zdk api` escape hatch, tickets, comments, users, organizations, search, output formatting, rate governor, cursor/offset pagination, config + profiles, completions |
| **v0.2.0** | Views, groups, macros, triggers, automations, SLA, schedules, fields, forms, custom statuses, tags, suspended/deleted tickets, requests (customer-facing), satisfaction/CSAT |
| **v0.3.0** | Job orchestration, all bulk operations, idempotency keys, attachments (upload + download + redact), side conversations, schema resolver + `--resolve-names` |
| **v0.4.0** | Incremental sync engine with resumable state, `zdk backup`, Help Center + community, docs-as-code article round trip |
| **v0.5.0** | Configuration-as-code: `rules export/plan/apply`, `diff`, `promote`, `refs`, `lint`, sandbox workflow |
| **v0.6.0** | Talk, Chat, omnichannel routing, agent availability, custom objects, webhooks, ZIS, ITAM |
| **v0.7.0** | GDPR/redaction workflows, audit logs, deletion schedules, `zdk doctor`, `zdk auth migrate --report` |
| **v0.8.0** | Generated coverage completion for every remaining resource group; `zdk api ops` parity check in CI |
| **v0.9.0** | MCP server mode, `--watch`, `--exec` hooks |
| **v1.0.0** | Stable command surface, full documented API coverage, 80%+ coverage, published docs, Homebrew/crates.io/ghcr |
| **v1.1.0** | AI Agents API, WFM, Zendesk QA import |
| **v1.2.0** | Sunshine Conversations (separate host and auth model) |
| **Won't do** | Zendesk Sell (retires 31 Aug 2027), app/theme scaffolding (`zcli` owns it) |

**Hard deadline to respect:** v0.1.0's OAuth support must ship before **27 October 2026**, after which no user can create the API token that every competing CLI depends on.

---

## 21. Configuration Reference

### 21.1 Base URLs

| API | Base URL |
|---|---|
| Ticketing / Support | `https://{subdomain}.zendesk.com/api/v2/` |
| Help Center / Guide | `https://{subdomain}.zendesk.com/api/v2/help_center/` and `/api/v2/guide/` |
| Community (Gather) | `https://{subdomain}.zendesk.com/api/v2/community/` |
| Voice / Talk | `https://{subdomain}.zendesk.com/api/v2/channels/voice/` |
| Live Chat | `https://{subdomain}.zendesk.com/api/v2/chat/` |
| Custom Objects | `https://{subdomain}.zendesk.com/api/v2/custom_objects/` |
| Legacy Sunshine (removed July 2026) | `https://{subdomain}.zendesk.com/api/sunshine/` |
| Omnichannel routing / availability | `https://{subdomain}.zendesk.com/api/v2/queues/`, `/api/v2/agent_availabilities/` |
| Webhooks | `https://{subdomain}.zendesk.com/api/v2/webhooks/` |
| ZIS | `https://{subdomain}.zendesk.com/api/services/zis/` |
| IT Asset Management | `https://{subdomain}.zendesk.com/api/v2/it_asset_management/` |
| AI Agents | `https://{subdomain}.zendesk.com/ai-agents/api/` |
| Zendesk QA | `https://{subdomain}.zendesk.com/qa/` |
| OAuth authorize | `https://{subdomain}.zendesk.com/oauth/authorizations/new` |
| OAuth token | `https://{subdomain}.zendesk.com/oauth/tokens` |
| Status | `https://status.zendesk.com/api/incidents` |
| Public IPs | `https://{subdomain}.zendesk.com/ips` |

### 21.2 OpenAPI sources

Zendesk publishes OpenAPI specifications for the Support, Help Center, Voice, and Chat APIs, and a Postman public workspace covering all APIs except Sell and Sunshine Conversations ([API Reference home](https://developer.zendesk.com/api-reference/)). Snapshots live in `specs/` with a `SPEC_VERSIONS.toml` manifest; `xtask spec-refresh` pulls upstream and `xtask spec-diff` fails CI on drift. Generated code is committed so builds never depend on the network.

### 21.3 Resource-group coverage targets

| Product area | Resource groups | v1.0 target |
|---|---:|---|
| Ticketing / Support | 88 | 100% |
| Help Center / Guide | 31 | 100% |
| Voice / Talk | 19 | 100% |
| Live Chat | 18 | 100% (documented REST only) |
| Custom Data / Custom Objects | 17 | 100% current; legacy Sunshine excluded (removed July 2026) |
| Omnichannel routing / availability | 10 | 100% |
| ZIS | 12 | 100% |
| IT Asset Management | 5 | 100% (EAP — flagged as unstable) |
| AI Agents | 5 | v1.1 |
| Webhooks | 1 + event catalogues | 100% |
| Answer Bot | 2 | 100% |
| Status / Reseller / Attachment content | 3 | 100% |
| WFM | 1 GA + 2 EAP | v1.1 |
| Zendesk QA | 2 | v1.1 |
| Sunshine Conversations | 17 | v1.2 |
| Sell (Sales CRM) | 66 | Never — retires 31 Aug 2027 |

`zdk api ops --coverage` prints generated-versus-curated coverage per group, and CI fails if a documented operation is missing from the registry.

---

## 22. Known Zendesk API Limitations (Design Around These)

| Limitation | Impact | CLI mitigation |
|---|---|---|
| API tokens removed (27 Oct 2026 / 30 Apr 2027) | Every token-based tool breaks | OAuth-first; `zdk auth migrate`; countdown warnings |
| Offset pagination capped at 10,000 records | Silent `400` at page 101 | Cursor by default; fail early with the named alternative |
| Incremental exports limited to 10 req/min | Naive exporters collapse | Dedicated bucket, resumable cursors, wall-time estimates |
| Incremental exports return duplicates by design | Double-counting in downstream systems | Dedupe on `(id, generated_timestamp)` |
| Same `start_time` returns the same page | Infinite loops (a real SDK bug) | Cursor mode default; loop-guard with an explicit error |
| Search caps at 1,000 results while `count` overstates | Silent data loss | Warn and route to `search export` |
| Search index is eventually consistent | Just-written records missing | Warn when a search follows a write in-session |
| Bulk endpoints are async with 100-record batches | Partial failures invisible | Job orchestration, per-record reconciliation, exit code 8 |
| Only the last 100 job statuses are retained, briefly | Results lost | Persist job IDs and results locally on submission |
| 30 concurrent jobs max | `TooManyJobs` | Local queue, position reporting, header-aware budget |
| Everything is numeric IDs; only `ticket_fields` exposes titles | Unreadable output, unusable filters | Schema resolver + `--resolve-names` |
| No config-as-code for business rules | No versioning, no diff, no promotion | `zdk rules export/plan/apply`, `diff`, `promote`, `refs`, `lint` |
| Sandbox promotion is one-way, unrevertable, 100-dependency capped; sandboxes are plan-gated | Risky changes | `plan` before `apply`; rollback journal; works without a sandbox |
| Product families differ in base URL, auth, pagination, rate limits | Constant surprises | Per-product clients and budgets behind one uniform command grammar |
| Upload tokens expire after 60 minutes; three attachment storage locations; filename collisions | Broken attachment flows | Upload-and-attach in one operation; `--dedupe-filenames` |
| Attachments are absent from exports | Incomplete backups | `attachments download-all` as part of `zdk backup` |
| Redaction is irreversible, ordered, and impossible on closed tickets | Compliance risk | `zdk gdpr checklist` with per-step feasibility and confirmations |
| No public Explore/reporting API | No server-side aggregation | Expose metrics and metric events; aggregate locally (`csat summary`) |
| Rate limits vary by plan and include undocumented conditional limits | Unpredictable 429s | Auto-detect plan; always trust `Retry-After` over local arithmetic |
| Chat is a flat 200 req/min on every plan | Chat work throttles independently | Separate Chat budget |
| Ticket updates limited to 30 per 10 min per user per ticket | Bulk retries fail confusingly | Per-ticket bucket with a clear error |
| `organization_field` and `custom_objects` lack granular write scopes | Forces global `write` | `zdk auth scopes check` says so explicitly |
| Standard changes get only 1 month notice | Silent breakage | Weekly `spec-diff` CI job with automatic PRs |

---

## 23. Open Questions

- [ ] **Binary name.** `zd` is taken twice (`johanviberg/zd` and `tizzo/zendesk-cli`'s binary), and `zcli` is Zendesk's own.
  - **Recommendation:** ship as crate/repo `zendesk-cli` with binary **`zdk`**. Offer `zdk completions --alias zd` for anyone who wants the shorter form, but never install it by default.
- [ ] **Repo name collision.** Three public repos are already called `zendesk-cli`.
  - **Recommendation:** `osodevops/zendesk-cli` is fine — namespaced and it is the honest name. Differentiate in the README's first paragraph, not the slug.
- [ ] **Generate models from OpenAPI, or hand-write?**
  - **Recommendation:** generate into `src/api/generated/` and commit the output; hand-write the curated layer on top. Reproducible builds, no network at build time, and the weekly spec-diff PR keeps it current.
- [ ] **How aggressive should caching be?**
  - **Recommendation:** cache schema (fields, forms, groups, brands, statuses, locales) with a 1-hour TTL; cache list responses for 60 seconds only; never cache writes. Support ops changes fast, and stale reads are worse than an extra call.
- [ ] **Should `rules apply` support deletes?**
  - **Recommendation:** yes, but behind `--allow-delete` and never as part of `--yes`. Deleting a trigger in production from a YAML file is exactly the accident to guard against.
- [ ] **MCP in the same binary or a separate one?**
  - **Recommendation:** same binary, `zdk mcp serve`, feature-flagged at compile time (`--no-default-features` drops it). One `zdk-core`, two front ends.
- [ ] **Webhook receiver mode?**
  - **Recommendation:** v1.1+. `zdk webhooks listen --port 9000 --verify-signature` for local development is genuinely useful and Zendesk has no equivalent, but it is scope creep for v1.
- [ ] **Interactive TUI inbox?**
  - **Recommendation:** out of scope. If demand appears, ship it as a separate `zdk-tui` binary over the same core.
- [ ] **Should the CLI expose the Apps REST API given `zcli` exists?**
  - **Recommendation:** yes for installation and settings management (`/api/v2/apps`, 18 operations) — that is operations, not development. No scaffolding, no local server.
- [ ] **High Volume add-on detection.**
  - **Recommendation:** infer from the observed `X-Rate-Limit` value rather than asking the user, and let `rate_limit.high_volume_addon` override.

---

## Appendix A: Example Workflows

### A.1 Morning triage

```bash
# What is on fire
zdk tickets list --status new,open --priority urgent --resolve-names \
  --fields id,subject,requester.name,organization.name,created_at

# My queue, oldest first
zdk tickets list --assignee me --status open --sort-by created_at --sort-order asc

# Unassigned and ageing
zdk tickets list --unassigned --older-than 4h --group "Platform Support"

# SLA risk
zdk tickets list --sla-breached -o table

# Read one properly, then answer
zdk tickets show 12345
zdk tickets reply 12345 --editor --public
zdk tickets update 12345 --status pending --add-tag awaiting-customer
```

### A.2 Bulk cleanup with a safety net

```bash
# Find candidates
zdk search export "type:ticket status:pending updated<2026-06-01" --all \
  -o ndjson > stale.ndjson
jq -r .id stale.ndjson > stale-ids.txt

# See exactly what would change
zdk tickets bulk-update --from stale-ids.txt --field status=solved \
  --field 'comment.body=Closing due to inactivity.' --dry-run

# Do it, watching the job
zdk tickets bulk-update --from stale-ids.txt --field status=solved \
  --field 'comment.body=Closing due to inactivity.' --wait

# Fix only what failed
zdk jobs failures $JOB_ID -o csv > failures.csv
zdk jobs retry $JOB_ID
```

### A.3 Nightly incremental export into the lakehouse

```bash
#!/usr/bin/env bash
set -euo pipefail
export ZENDESK_SUBDOMAIN=oso
export ZENDESK_CLIENT_ID=zdk_ci
export ZENDESK_CLIENT_SECRET="${ZENDESK_CLIENT_SECRET}"

zdk auth login --client-credentials --scopes tickets:read,users:read,organizations:read

for stream in tickets users organizations ticket-events ticket-metric-events; do
  zdk sync "$stream" --resume --to "./data/${stream}/" --format ndjson \
    --rate-limit-strategy wait
done

zdk sync status -o json > ./data/_sync_state.json
aws s3 sync ./data/ s3://oso-lakehouse/raw/zendesk/
```

### A.4 Business rules in Git

```bash
# One-off: capture current state
zdk rules export --to ./zendesk-config/ --symbolic-refs
git add zendesk-config && git commit -m "chore: snapshot Zendesk configuration"

# Change a trigger in your editor, then review like code
$EDITOR zendesk-config/triggers/escalate-urgent.yaml
zdk rules plan --from ./zendesk-config/ --profile production

# In CI on merge to main
zdk rules validate ./zendesk-config/
zdk rules plan --from ./zendesk-config/ --profile production -o json > plan.json
zdk rules apply --from ./zendesk-config/ --profile production --yes

# Before deleting a field, find out what breaks
zdk refs field 1900001
```

### A.5 Sandbox to production promotion

```bash
zdk diff --source sandbox --target production --only triggers,macros,views
zdk promote --from sandbox --to production --only triggers --dry-run
zdk promote --from sandbox --to production --only triggers \
  --journal ./rollback-$(date +%F).json
# If it goes wrong
zdk promote rollback ./rollback-2026-09-10.json
```

### A.6 Alert-to-ticket bridge

```bash
# From an alertmanager webhook handler
zdk tickets create \
  --subject "[$SEVERITY] $ALERT_NAME on $SERVICE" \
  --comment "$DESCRIPTION" \
  --requester "monitoring@oso.sh" \
  --type incident --priority urgent \
  --group "Platform Support" \
  --tag automated --tag "$SERVICE" \
  --custom-field "Environment=production" \
  --external-id "$ALERT_FINGERPRINT" \
  -o json | jq -r .id
```

### A.7 Knowledge base as code

```bash
# Pull the KB into a repo as Markdown
zdk hc articles export --to ./kb/ --format markdown --all --locale en-gb

# Edit, review in a PR, push back
zdk hc articles import --from ./kb/ --dry-run
zdk hc articles import --from ./kb/ --publish
```

### A.8 CSAT reporting

```bash
zdk satisfaction list --all --start-time 2026-08-01 -o csv > csat-august.csv
zdk csat summary --from 2026-08-01 --to 2026-08-31
zdk satisfaction list --all --score bad --resolve-names \
  --fields ticket_id,score,comment,assignee.name
```

### A.9 GDPR erasure request

```bash
zdk users search "jane@acme.com"
zdk gdpr export-user 456 --to ./dsar-456/          # subject access first
zdk gdpr checklist 456                              # ordered, feasibility-checked
zdk gdpr delete-user 456 --yes
zdk gdpr status 456
```

### A.10 Migrating off API tokens before the deadline

```bash
zdk auth status                    # shows days remaining on the token deadline
zdk auth migrate --report          # what in the account still uses tokens
zdk auth migrate                   # guided OAuth client setup + least-privilege scopes
zdk auth test                      # verify every product API still answers
```

---

## Appendix B: Competitive Comparison

| Feature | **zendesk-cli (ours)** | zcli (official) | johanviberg/zd | zd-cli (Python) | tizzo (Rust) | xoseperez (Rust) | typer-zendesk-cli |
|---|---|---|---|---|---|---|---|
| Language | Rust | TypeScript | Go | Python | Rust | Rust | Python |
| Single static binary | ✅ | ❌ (Node) | ✅ | ❌ | ✅ | ✅ | ❌ |
| Ticket read | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ |
| Ticket create/update | ✅ | ❌ | ✅ | ✅ | reply only | ❌ | ❌ |
| Users / organizations CRUD | ✅ | ❌ | ❌ | read | ❌ | ❌ | partial |
| Views | ✅ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| Macros / triggers / automations | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | bulk only |
| SLA policies | ✅ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| Side conversations | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Custom objects | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Help Center content | ✅ | themes only | articles | ❌ | ❌ | ❌ | ❌ |
| Talk / Chat | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Incremental exports | ✅ resumable | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Bulk jobs + polling | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | partial |
| Dry-run on writes | ✅ | ❌ | ❌ | ❌ | ❌ | n/a | ❌ |
| Idempotency keys | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Rate-limit governor | ✅ full headers | ❌ | exit code only | ❌ | ❌ | ❌ | ❌ |
| Cursor pagination everywhere | ✅ | n/a | partial | partial | partial | partial | ❌ |
| **OAuth (auth code + PKCE)** | ✅ | ❌ token only | ✅ | ✅ | ❌ | ❌ | ❌ |
| **OAuth client credentials** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Survives Apr 2027 token EOL | ✅ | ❌ | ✅ | ✅ | ❌ | ❌ | ❌ |
| Headless credential store | ✅ | ❌ fails in Docker | ✅ | ✅ | ❌ needs libdbus | ✅ | ✅ |
| Multi-instance profiles | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ single | ❌ |
| **Config-as-code (plan/apply)** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Cross-instance diff / promote** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **ID ↔ name resolution** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| GDPR / redaction workflow | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Backup / restore | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Raw API escape hatch | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| MCP server mode | ✅ | ❌ | 6 tools | ✅ | ❌ | ❌ | ❌ |
| Output formats | table/JSON/NDJSON/CSV/YAML/TSV/raw | n/a | text/JSON/NDJSON | JSON/MD | human/JSON | JSON | CSV |
| Full documented API coverage | ✅ generated | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Actively maintained | ✅ | ⚠️ "not routinely reviewing issues" | ✅ | ✅ | ⚠️ pre-1.0 | ⚠️ | ❌ 2024 |
| Licence | MIT | Apache-2.0 | MIT | varies | MIT | MIT | MIT |

---

## Appendix C: Sources

Every design constraint in this document traces to a primary source.

**Zendesk API reference and platform docs**
- [API Reference home](https://developer.zendesk.com/api-reference/)
- [Introduction](https://developer.zendesk.com/api-reference/introduction/introduction/)
- [Security and authentication](https://developer.zendesk.com/api-reference/introduction/security-and-auth/)
- [Rate limits](https://developer.zendesk.com/api-reference/introduction/rate-limits/)
- [Pagination](https://developer.zendesk.com/api-reference/introduction/pagination/)
- [API changes policy](https://developer.zendesk.com/api-reference/introduction/api_changes/)
- [Changelog](https://developer.zendesk.com/api-reference/changelog/changelog/)
- [Ticketing introduction](https://developer.zendesk.com/api-reference/ticketing/introduction/)
- [Tickets](https://developer.zendesk.com/api-reference/ticketing/tickets/tickets/)
- [Requests](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket-requests/)
- [Incremental Exports](https://developer.zendesk.com/api-reference/ticketing/ticket-management/incremental_exports/)
- [Using the Incremental Exports API](https://developer.zendesk.com/documentation/api-basics/working-with-data/using-the-incremental-export-api/)
- [Job Statuses](https://developer.zendesk.com/api-reference/ticketing/ticket-management/job_statuses/)
- [Help Center API introduction](https://developer.zendesk.com/api-reference/help_center/help-center-api/introduction/)
- [Talk API](https://developer.zendesk.com/api-reference/voice/talk-api/introduction/)
- [Chat API](https://developer.zendesk.com/api-reference/live-chat/chat-api/introduction/)
- [Custom Objects](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_objects/)
- [Agent availability](https://developer.zendesk.com/api-reference/agent-availability/introduction/)
- [Webhooks](https://developer.zendesk.com/api-reference/webhooks/webhooks/)
- [Betas and EAPs](https://developer.zendesk.com/api-reference/betas/introduction/)
- [WFM introduction](https://developer.zendesk.com/api-reference/wfm/introduction/)
- [Status API](https://developer.zendesk.com/api-reference/status_api/status_api/)

**Authentication changes**
- [Announcing the removal of API tokens as an authentication method for API requests](https://support.zendesk.com/hc/en-us/articles/10851263566234-Announcing-the-removal-of-API-tokens-as-an-authentication-method-for-API-requests)
- [Migrating from API tokens to OAuth access tokens](https://developer.zendesk.com/documentation/authentication/oauth-migration/)
- [Creating and using OAuth tokens with the API](https://developer.zendesk.com/documentation/authentication/creating-and-using-oauth-tokens-with-the-api/)
- [Announcing more granular OAuth client scopes](https://support.zendesk.com/hc/en-us/articles/11093380955674-Announcing-more-granular-OAuth-client-scopes)
- [Deprecation of password access for APIs](https://support.zendesk.com/hc/en-us/articles/9941874259354-Deprecation-of-password-access-for-APIs)
- [Retiring API tokens: community discussion](https://community.zendesk.com/platform-and-developer-18/retiring-api-tokens-share-your-questions-and-best-practices-before-our-upcoming-developer-meetup-22439)
- [API token EOL: Microsoft Entra connector has no OAuth path](https://community.zendesk.com/platform-and-developer-18/api-token-eol-april-2027-integration-path-have-no-oauth-migration-option-microsoft-entra-provisioning-connector-22038)
- [OAuth scopes: organization_field and custom_objects](https://community.zendesk.com/platform-and-developer-18/oauth-scopes-organization-field-and-custom-objects-22314)
- [Deprecation of API tokens and local development](https://community.zendesk.com/platform-and-developer-18/deprecation-of-api-tokens-and-local-development-21934)

**Existing tools audited**
- [zendesk/zcli](https://github.com/zendesk/zcli) · [Using ZCLI](https://developer.zendesk.com/documentation/apps/getting-started/using-zcli/) · [zcli #384 OAuth deprecation](https://github.com/zendesk/zcli/issues/384)
- [johanviberg/zd](https://github.com/johanviberg/zd)
- [zd-cli on PyPI](https://pypi.org/project/zd-cli/) · [andmarios/zendesk-skill](https://github.com/andmarios/zendesk-skill)
- [tizzo/zendesk-cli](https://github.com/tizzo/zendesk-cli) · [xoseperez/zendesk-cli](https://github.com/xoseperez/zendesk-cli) · [roger-rodriguez/zendesk-cli](https://github.com/roger-rodriguez/zendesk-cli) · [zakiaziz/zendesk-cli](https://github.com/zakiaziz/zendesk-cli)
- [tbs89/typer-zendesk-cli](https://github.com/tbs89/typer-zendesk-cli) · [formspree/zdesk](https://github.com/formspree/zdesk) · [nvllsvm/zendesk-cli](https://github.com/nvllsvm/zendesk-cli)
- [crates.io: zendesk](https://crates.io/crates/zendesk)

**SDK gaps cited as evidence**
- [zenpy #476 side conversations unsupported](https://github.com/facetoe/zenpy/issues/476) · [zenpy #489 incremental export infinite loop](https://github.com/facetoe/zenpy/issues/489) · [zenpy #672 search pagination broken](https://github.com/facetoe/zenpy/issues/672) · [zenpy #477 no rate-limit exception](https://github.com/facetoe/zenpy/issues/477)
- [node-zendesk #444 attachment upload hangs](https://github.com/blakmatrix/node-zendesk/issues/444) · [node-zendesk #430 rate-limit control](https://github.com/blakmatrix/node-zendesk/issues/430)
- [zendesk_api_client_rb #271 SLA policies missing](https://github.com/zendesk/zendesk_api_client_rb/issues/271) · [#509 attachment handling](https://github.com/zendesk/zendesk_api_client_rb/issues/509) · [#510 idempotency keys](https://github.com/zendesk/zendesk_api_client_rb/issues/510)
- [zendesk_api_client_php #558 custom objects](https://github.com/zendesk/zendesk_api_client_php/issues/558)
- [go-zendesk #311 bulk job support](https://github.com/nukosuke/go-zendesk/issues/311)
- [zendesk-mcp-server #124 CSAT](https://github.com/fruggr/zendesk-mcp-server/issues/124) · [#122 timing metrics](https://github.com/fruggr/zendesk-mcp-server/issues/122) · [#278 followers and email CCs](https://github.com/fruggr/zendesk-mcp-server/issues/278) · [#48 end-user mode](https://github.com/fruggr/zendesk-mcp-server/issues/48)

**Reference implementation whose conventions this project mirrors**
- [osodevops/xero-cli](https://github.com/osodevops/xero-cli)

---

## Appendix D: Full API Coverage Matrix

The complete documented Zendesk REST surface as published at [developer.zendesk.com/api-reference](https://developer.zendesk.com/api-reference/): **279 resource groups, 1,480 operations**. Every group below must appear in the generated operation registry, and `zdk api ops --coverage` must account for all of them. Groups without a curated ergonomic command are still fully callable via `zdk api <METHOD> <path>`.

### Ticketing / Support — 88 groups, 618 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| X (Twitter) Channel | `/api/v2/channels/twitter` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/x_channel/) |
| Access Logs | `/api/v2/access_logs` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/access_logs/) |
| Account Settings | `/api/v2/account` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/account_settings/) |
| App Location Installations | `/api/v2/apps/location_installations` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/apps/app_location_installations/) |
| App Locations | `/api/v2/apps/locations` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/apps/app_locations/) |
| Approval Requests | `/api/v2/approval_requests` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/approvals/approval_requests/) |
| Apps | `/api/v2/apps` | 18 | [docs](https://developer.zendesk.com/api-reference/ticketing/apps/apps/) |
| Attachments | `/api/v2` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket-attachments/) |
| Audit Logs | `/api/v2/audit_logs` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/audit_logs/) |
| Automations | `/api/v2/automations` | 9 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/automations/) |
| Bookmarks | `/api/v2/bookmarks` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/bookmarks/) |
| Brand Agents | `/api/v2` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/brand_agents/) |
| Brands | `/api/v2/brands` | 9 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/brands/) |
| CSAT Survey Responses | `/api/v2/guide` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/csat_survey_responses/) |
| CSAT Surveys | `/api/v2/guide` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/csat_surveys/) |
| Channel Framework | `/api/v2/any_channel` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/channel_framework/) |
| Conversation Log | `/api/v2/tickets` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/conversation_log/) |
| Custom Agent Roles | `/api/v2/custom_roles` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/custom_roles/) |
| Custom Ticket Statuses | `/api/v2` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/custom_ticket_statuses/) |
| Deletion Schedules | `/api/v2/deletion_schedules` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/deletion_schedules/) |
| Dynamic Content Item Variants | `/api/v2/dynamic_content/items` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/dynamic_content_item_variants/) |
| Dynamic Content Items | `/api/v2/dynamic_content/items` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/dynamic_content/) |
| Email Notifications | `/api/v2` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/email_notifications/) |
| Events API | `/api/v2` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/events-api/events-api/) |
| Global OAuth Clients | `/api/v2/oauth/global_clients` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/oauth/global_clients/) |
| Group Memberships | `/api/v2` | 14 | [docs](https://developer.zendesk.com/api-reference/ticketing/groups/group_memberships/) |
| Group SLA Policies | `/api/v2` | 8 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/group_sla_policies/) |
| Groups | `/api/v2` | 12 | [docs](https://developer.zendesk.com/api-reference/ticketing/groups/groups/) |
| Incremental Exports | `/api/v2/incremental` | 8 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/incremental_exports/) |
| Incremental Skill-based Routing | `/api/v2/incremental/routing` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/incremental_skill_based_routing/) |
| JIRA Integration Links (V2) | `/api/v2/integrations/jira` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/jira_v2/jira_integration_links_v2/) |
| JIRA Links | `/api` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/jira/jira_links/) |
| Job Statuses | `/api/v2/job_statuses` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/job_statuses/) |
| Locales | `/api/v2/locales` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/locales/) |
| Lookup Relationships | `/api/v2` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/lookup_relationships/lookup_relationships/) |
| Macros | `/api/v2` | 22 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/macros/) |
| OAuth Clients | `/api/v2` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/oauth/oauth_clients/) |
| OAuth Tokens | `/api/v2/oauth/tokens` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/oauth/oauth_tokens/) |
| OAuth Tokens for Grant Types | `/oauth/tokens` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/oauth/grant_type_tokens/) |
| Organization Fields | `/api/v2/organization_fields` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/organizations/organization_fields/) |
| Organization Memberships | `/api/v2` | 14 | [docs](https://developer.zendesk.com/api-reference/ticketing/organizations/organization_memberships/) |
| Organization Subscriptions | `/api/v2` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/organizations/organization_subscriptions/) |
| Organizations | `/api/v2` | 19 | [docs](https://developer.zendesk.com/api-reference/ticketing/organizations/organizations/) |
| Profiles API | `/api/v2` | 9 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/profiles_api/profiles_api/) |
| Push Notification Devices | `/api/v2/push_notification_devices/destroy_many` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/push_notification_devices/) |
| Remote Authentications | `/api/v2/remote_authentications` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/remote_authentications/) |
| Requests | `/api/v2` | 12 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket-requests/) |
| Resource Collections | `/api/v2/resource_collections` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/resource_collections/) |
| SLA Policies | `/api/v2` | 8 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/sla_policies/) |
| Satisfaction Ratings | `/api/v2` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/satisfaction_ratings/) |
| Satisfaction Reasons | `/api/v2/satisfaction_reasons` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/satisfaction_reasons/) |
| Schedules | `/api/v2/business_hours/schedules` | 11 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/schedules/) |
| Search | `/api/v2` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/search/) |
| Security Settings | `/api/v2/security_settings` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/security_settings/) |
| Sessions | `/api/v2` | 8 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/sessions/) |
| Sharing Agreements | `/api/v2/sharing_agreements` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/sharing_agreements/) |
| Side Conversation Attachment | `/api/v2/tickets` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/side_conversation/side_conversation_attachment/) |
| Side Conversation Events | `/api/v2/tickets` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/side_conversation/side_conversation_event/) |
| Side Conversations | `/api/v2/tickets` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/side_conversation/side_conversation/) |
| Skill-based Routing | `/api/v2/routing` | 19 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/skill_based_routing/) |
| Support Addresses | `/api/v2/recipient_addresses` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/support_addresses/) |
| Suspended Tickets | `/api/v2/suspended_tickets` | 9 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/suspended_tickets/) |
| Tags | `/api/v2` | 16 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/tags/) |
| Target Failures | `/api/v2/target_failures` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/targets/target_failures/) |
| Targets | `/api/v2/targets` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/targets/targets/) |
| Task List Events | `/api/v2/task_lists` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/tasks/task_list_events/) |
| Task List Templates | `/api/v2/task_list_templates` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/tasks/task_list_templates/) |
| Task Lists | `/api/v2` | 9 | [docs](https://developer.zendesk.com/api-reference/ticketing/tasks/task_lists/) |
| Ticket Activities | `/api/v2/activities` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/activity_stream/) |
| Ticket Audits | `/api/v2` | 5 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_audits/) |
| Ticket Comments | `/api/v2` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_comments/) |
| Ticket Fields | `/api/v2/ticket_fields` | 12 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_fields/) |
| Ticket Form Statuses | `/api/v2` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_form_statuses/) |
| Ticket Forms | `/api/v2/ticket_forms` | 12 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_forms/) |
| Ticket Import | `/api/v2/imports/tickets` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_import/) |
| Ticket Metric Events | `/api/v2` | 2 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_metric_events/) |
| Ticket Metrics | `/api/v2` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_metrics/) |
| Ticket Skips | `/api/v2` | 4 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/ticket_skips/) |
| Tickets | `/api/v2` | 36 | [docs](https://developer.zendesk.com/api-reference/ticketing/tickets/tickets/) |
| Trigger Categories | `/api/v2/trigger_categories` | 6 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/trigger_categories/) |
| Triggers | `/api/v2/triggers` | 15 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/triggers/) |
| User Fields | `/api/v2/user_fields` | 11 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/user_fields/) |
| User Identities | `/api/v2` | 14 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/user_identities/) |
| User Passwords | `/api/v2/users` | 3 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/user_passwords/) |
| Users | `/api/v2` | 32 | [docs](https://developer.zendesk.com/api-reference/ticketing/users/users/) |
| Views | `/api/v2/views` | 20 | [docs](https://developer.zendesk.com/api-reference/ticketing/business-rules/views/) |
| Workspaces | `/api/v2/workspaces` | 7 | [docs](https://developer.zendesk.com/api-reference/ticketing/ticket-management/workspaces/) |
| Zendesk Public IPs | `/ips` | 1 | [docs](https://developer.zendesk.com/api-reference/ticketing/account-configuration/public_ips/) |

### Help Center / Guide — 31 groups, 273 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Account Custom Claims | `/api/v2/help_center/integration/account_custom_claims` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/account_custom_claims/) |
| Article Attachments | `/api/v2/help_center` | 11 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/article_attachments/) |
| Article Comments | `/api/v2/help_center` | 11 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/article_comments/) |
| Article Labels | `/api/v2/help_center` | 9 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/article_labels/) |
| Articles | `/api/v2/help_center` | 28 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/articles/) |
| Badge Assignments | `/api/v2/gather/badge_assignments` | 3 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/badge_assignments/) |
| Badge Categories | `/api/v2/gather/badge_categories` | 4 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/badge_categories/) |
| Badges | `/api/v2/gather/badges | /api/v2/gather/badges/icon_uploads | /api/v2/gather/badges/{id}` | 7 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/badges/) |
| Categories | `/api/v2/help_center` | 14 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/categories/) |
| Content Subscriptions | `/api/v2` | 26 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/content_subscriptions/) |
| Content Tags | `/api/v2/guide/content_tags` | 7 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/content_tags/) |
| External Content Records | `/api/v2/guide/external_content/records` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/federated-search/external_content_records/) |
| External Content Sources | `/api/v2/guide/external_content/sources` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/federated-search/external_content_sources/) |
| External Content Types | `/api/v2/guide/external_content/types` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/federated-search/external_content_types/) |
| Guide Medias | `/api/v2/guide/medias` | 7 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/guide_medias/) |
| Help Center JWTs | `/api/v2/help_center/integration` | 2 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/help_center_jwts/) |
| Help Center Search | `/api/v2/guide/search?filter[locales]={filter[locales]} | /api/v2/help_center/articles/embeddable_search | /api/v2/help_center/articles/search` | 10 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/help_center_search/) |
| Help Center Sessions | `/api/v2/help_center/sessions` | 1 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/help_center_sessions/) |
| Management Permission Groups | `/api/v2/guide` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/permission_groups/) |
| Post Comments | `/api/v2` | 7 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/post_comments/) |
| Posts | `/api/v2/community` | 13 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/posts/) |
| Redirect Rules | `/api/v2/guide/redirect_rules` | 4 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/redirect_rules/) |
| Sections | `/api/v2/help_center` | 18 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/sections/) |
| Service Catalog Items | `/api/v2/help_center/service_catalog/items` | 3 | [docs](https://developer.zendesk.com/api-reference/help_center/employee-services/service_catalog_items/) |
| Themes | `/api/v2/guide/theming` | 8 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/theming/) |
| Topics | `/api/v2/community/topics` | 5 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/topics/) |
| Translations | `/api/v2/help_center` | 17 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/translations/) |
| User Images | `/api/v2/guide/user_images` | 2 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/user_images/) |
| User Segments | `/api/v2/help_center` | 9 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/user_segments/) |
| User Subscriptions | `/api/v2/help_center/users` | 3 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/user_subscriptions/) |
| Votes | `/api/v2` | 19 | [docs](https://developer.zendesk.com/api-reference/help_center/help-center-api/votes/) |

### Voice / Talk — 19 groups, 68 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Addresses | `/api/v2/channels/voice/addresses` | 5 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/addresses/) |
| Availabilities | `/api/v2/channels/voice/availabilities` | 2 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/availabilities/) |
| Basics | `/api/v2/channels/voice` | 3 | [docs](https://developer.zendesk.com/api-reference/voice/talk-partner-edition-api/basics/) |
| Call Console | `/api/v2/channels/voice/call_console` | 1 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/call_console/) |
| Callback Requests | `/api/v2/channels/voice/callback_requests` | 1 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/callback_requests/) |
| Calls | `/api/v2/calls` | 4 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/calls/) |
| Calls | `/api/v2/calls` | 4 | [docs](https://developer.zendesk.com/api-reference/voice/talk-partner-edition-api/calls/) |
| Dashboard | `/api/v2/channels/voice/dashboard` | 3 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/dashboard/) |
| Digital lines | `/api/v2/channels/voice/digital_lines` | 4 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/digital_lines/) |
| Greetings | `/api/v2/channels/voice` | 8 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/greetings/) |
| IVR Menus | `/api/v2/channels/voice/ivr` | 5 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/ivr_menus/) |
| IVR Routes | `/api/v2/channels/voice/ivr` | 5 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/ivr_routes/) |
| IVRs | `/api/v2/channels/voice/ivr` | 5 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/ivrs/) |
| Incremental Exports | `/api/v2/channels/voice/stats/incremental` | 3 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/incremental_exports/) |
| Lines | `/api/v2/channels/voice/lines` | 1 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/lines/) |
| Phone numbers | `/api/v2/channels/voice/phone_numbers` | 6 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/phone_numbers/) |
| Recordings | `/api/v2/channels/voice/calls` | 2 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/recordings/) |
| Stats | `/api/v2/channels/voice/stats` | 4 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/stats/) |
| Voice Settings | `/api/v2/channels/voice/settings` | 2 | [docs](https://developer.zendesk.com/api-reference/voice/talk-api/voice_settings/) |

### Live Chat — 18 groups, 95 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Accounts | `/api/v2/chat/account` | 2 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/accounts/) |
| Agent Events | `/api/v2/chat/incremental/agent_events` | 1 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/incremental_agent_events_api/) |
| Agents | `/api/v2/chat/agents` | 9 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/agents/) |
| Bans | `/api/v2/chat/bans` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/bans/) |
| Chats | `/api/v2/chat` | 7 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/chats/) |
| Departments | `/api/v2/chat/departments` | 8 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/departments/) |
| Goals | `/api/v2/chat/goals` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/goals/) |
| Incremental Exports | `/api/v2/chat/incremental` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/incremental_export/) |
| Live Chat | `/api/v2/chat/agents` | 1 | [docs](https://developer.zendesk.com/api-reference/live-chat/introduction/) |
| OAuth Clients | `/api/v2/chat/oauth/clients` | 6 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/oauth_clients/) |
| OAuth Tokens | `/api/v2/chat/oauth/tokens` | 2 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/oauth_tokens/) |
| REST API | `/stream` | 12 | [docs](https://developer.zendesk.com/api-reference/live-chat/real-time-chat-api/rest/) |
| Roles | `/api/v2/chat/roles` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/roles/) |
| Routing Settings | `/api/v2/chat/routing_settings` | 6 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/routing_settings/) |
| Shortcuts | `/api/v2/chat/shortcuts` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/shortcuts/) |
| Skills | `/api/v2/chat/skills` | 8 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/skills/) |
| Triggers | `/api/v2/chat/triggers` | 5 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/triggers/) |
| Visitors | `/api/v2/chat/visitors` | 3 | [docs](https://developer.zendesk.com/api-reference/live-chat/chat-api/visitors/) |

### Custom Data — 17 groups, 86 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Custom Object Fields | `/api/v2/custom_objects` | 7 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_object_fields/) |
| Custom Object Permissions | `/api/v2/custom_objects` | 9 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_object_permissions/) |
| Custom Object Record Attachments | `/api/v2/custom_objects` | 5 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_object_record_attachments/) |
| Custom Object Record Events | `/api/v2/custom_objects` | 1 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_object_record_events/) |
| Custom Object Records | `/api/v2` | 14 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_object_records/) |
| Custom Objects | `/api/v2/custom_objects` | 6 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/custom_objects/) |
| Legacy Custom Objects Events | `/api/sunshine/objects/events` | 1 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/object_events/) |
| Legacy Custom Objects Jobs | `/api/sunshine/jobs` | 3 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/jobs/) |
| Legacy Custom Objects Limits | `/api/sunshine/limits` | 1 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/limits/) |
| Legacy Custom Objects Permissions | `/api/sunshine` | 4 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/permissions/) |
| Legacy Custom Objects Search | `/api/sunshine/objects/query` | 1 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/custom_objects_search/) |
| Legacy Custom Objects Search | `/api/sunshine/objects/query` | 1 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/search/) |
| Legacy Object Records | `/api/sunshine/objects` | 8 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/resources/) |
| Legacy Object Types | `/api/sunshine/objects/types` | 6 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/resource_types/) |
| Legacy Relationship Records | `/api/sunshine` | 5 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/relationships/) |
| Legacy Relationship Types | `/api/sunshine/relationships/types` | 4 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects-api/relationship_types/) |
| Object Triggers | `/api/v2/custom_objects` | 10 | [docs](https://developer.zendesk.com/api-reference/custom-data/custom-objects/object_triggers/) |

### Omnichannel / Agent Availability — 10 groups, 32 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Capacity Rules | `/api/v2/capacity/rules` | 6 | [docs](https://developer.zendesk.com/api-reference/agent-availability/capacity-rules/capacity_rules_api/) |
| Account Groups Availability | `/api/v2/account_groups/availability` | 1 | [docs](https://developer.zendesk.com/api-reference/agent-availability/account-groups-availability/account_groups_availability/) |
| Agent Availabilities | `/api/v2/agent_availabilities` | 5 | [docs](https://developer.zendesk.com/api-reference/agent-availability/agent-availability-api/agent_availabilities/) |
| Assignees API | `/api/v2/capacity/rules` | 2 | [docs](https://developer.zendesk.com/api-reference/agent-availability/capacity-rules/assignees_api/) |
| Job Status | `/api/v2/agent_availabilities/agent_statuses/job_statuses` | 1 | [docs](https://developer.zendesk.com/api-reference/agent-availability/unified-agent-status-api/job_status/) |
| Omnichannel Engagements | `/api/v2/engagements` | 2 | [docs](https://developer.zendesk.com/api-reference/agent-availability/omnichannel-engagements/omnichannel_engagements/) |
| Omnichannel Routing Percentage-based Queues API | `/api/v2/queues` | 2 | [docs](https://developer.zendesk.com/api-reference/agent-availability/omnichannel_routing_queues/omnichannel_routing_percentage_queues/) |
| Omnichannel Routing Queues | `/api/v2/queues` | 7 | [docs](https://developer.zendesk.com/api-reference/agent-availability/omnichannel_routing_queues/omnichannel_routing_queues/) |
| Queue Events | `/api/v2/queue_events` | 1 | [docs](https://developer.zendesk.com/api-reference/agent-availability/queue-events/queue_events/) |
| Unified Agent Statuses | `/api/v2/agent_availabilities/agent_statuses` | 5 | [docs](https://developer.zendesk.com/api-reference/agent-availability/unified-agent-status-api/unified_agent_statuses/) |

### ZIS — 0 groups, 0 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|

### Webhooks — 1 groups, 11 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Webhooks API | `/api/v2/webhooks` | 11 | [docs](https://developer.zendesk.com/api-reference/webhooks/webhooks-api/webhooks/) |

### Answer Bot — 2 groups, 4 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Article Feedback | `/api/v2/answer_bot` | 3 | [docs](https://developer.zendesk.com/api-reference/answer-bot/answer-bot-api/article_feedback/) |
| Article Recommendations | `/api/v2/answer_bot/answers/articles` | 1 | [docs](https://developer.zendesk.com/api-reference/answer-bot/answer-bot-api/article_recommendations/) |

### AI Agents — 5 groups, 5 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| AI Agents Chat API | `/converse/chat` | 1 | [docs](https://developer.zendesk.com/api-reference/ai-agents/chat/chat/) |
| AI Agents Data Export API | `/data-export/v3/get-signed-urls` | 1 | [docs](https://developer.zendesk.com/api-reference/ai-agents/data-export/data-export/) |
| AI Agents Ticket API | `/converse/ticket` | 1 | [docs](https://developer.zendesk.com/api-reference/ai-agents/ticket/ticket/) |
| Delete User Data API | `/gdpr/delete-user-data` | 1 | [docs](https://developer.zendesk.com/api-reference/ai-agents/delete-user-data/delete-user-data/) |
| Widget Escalation API | `/agent/converse/send-message-to-widget` | 1 | [docs](https://developer.zendesk.com/api-reference/ai-agents/widget-escalation/widget-escalation/) |

### IT Asset Management — 5 groups, 28 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Asset Fields | `/api/v2/it_asset_management/asset_types` | 5 | [docs](https://developer.zendesk.com/api-reference/it-asset-management/asset_fields/) |
| Asset Locations | `/api/v2/it_asset_management/locations` | 5 | [docs](https://developer.zendesk.com/api-reference/it-asset-management/asset_locations/) |
| Asset Statuses | `/api/v2/it_asset_management/statuses` | 5 | [docs](https://developer.zendesk.com/api-reference/it-asset-management/asset_statuses/) |
| Asset Types | `/api/v2/it_asset_management/asset_types` | 5 | [docs](https://developer.zendesk.com/api-reference/it-asset-management/asset_types/) |
| Assets | `/api/v2/it_asset_management/assets` | 8 | [docs](https://developer.zendesk.com/api-reference/it-asset-management/assets/) |

### WFM — 1 groups, 8 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Teams | `/v2/teams` | 8 | [docs](https://developer.zendesk.com/api-reference/wfm/teams/) |

### Sunshine Conversations — 0 groups, 0 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|

### Status API — 1 groups, 3 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Status API | `/api/incidents` | 3 | [docs](https://developer.zendesk.com/api-reference/status_api/status_api/) |

### Reseller — 1 groups, 2 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Reseller API | `/api/v2/accounts` | 2 | [docs](https://developer.zendesk.com/api-reference/reseller/api_reference/) |

### Integration Services — 12 groups, 48 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| API Key Connections | `/api/services/zis/integrations` | 4 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/api_key_connections/) |
| All Connections | `/api/services/zis/integrations` | 1 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/all_connections/) |
| Basic Authentication Connections | `/api/services/zis/integrations` | 4 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/basic_authentication_connections/) |
| Bearer Token Connections | `/api/services/zis/integrations` | 4 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/bearer_token_connections/) |
| Bundles | `/api/services/zis/registry` | 4 | [docs](https://developer.zendesk.com/api-reference/integration-services/registry/bundles/) |
| Configs | `/api/services/zis/integrations` | 5 | [docs](https://developer.zendesk.com/api-reference/integration-services/configs/configs/) |
| Inbound Webhooks | `/api/services/zis/inbound_webhooks/generic` | 3 | [docs](https://developer.zendesk.com/api-reference/integration-services/inbound-webhooks/inbound_webhooks/) |
| Integrations | `/api/services/zis/registry` | 2 | [docs](https://developer.zendesk.com/api-reference/integration-services/registry/integrations/) |
| Job Specs | `/api/services/zis/registry` | 3 | [docs](https://developer.zendesk.com/api-reference/integration-services/registry/jobspecs/) |
| Links | `/api/services/zis/links` | 4 | [docs](https://developer.zendesk.com/api-reference/integration-services/links/links/) |
| OAuth Clients | `/api/services/zis/connections/oauth/clients` | 5 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/oauth_clients/) |
| OAuth Connections | `/api/services/zis` | 9 | [docs](https://developer.zendesk.com/api-reference/integration-services/connections/oauth_connections/) |

### Betas / EAP — 0 groups, 0 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|

### Sell (Sales CRM) — 66 groups, 196 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Account | `/v2/accounts/self` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/account/) |
| Aggregations | `/v3/deals/search` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/aggregations/) |
| App Locations | `/api/sell/apps` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/apps/app-locations/) |
| Appointments | `/v3/appointments` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/appointments/) |
| Apps | `/api` | 14 | [docs](https://developer.zendesk.com/api-reference/sales-crm/apps/apps/) |
| Associated Contacts | `/v2/deals/:deal_id/associated_contacts` | 3 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/associated-contacts/) |
| Associated Contacts | `/v3/associated_contacts` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/associated-contacts/) |
| Call Outcomes | `/v2/call_outcomes` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/call-outcomes/) |
| Calls | `/v2/calls` | 6 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/calls/) |
| Calls | `/v3/calls` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/calls/) |
| Collaboration | `/v3/collaborations` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/collaborations/) |
| Collaboration Request | `/v3/collaboration_requests` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/collaboration-requests/) |
| Collaborations | `/v2/collaborations` | 4 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/collaborations/) |
| Contacts | `/v2/contacts` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/contacts/) |
| Contacts | `/v3/contacts` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/contacts/) |
| Custom Fields | `/v2/:resource_type/custom_fields` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/custom-fields/) |
| Custom Fields | `/v3/custom_fields` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/custom-fields/) |
| Custom Fields Mapping | `/v3/deals/search` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/custom-fields-mapping/) |
| Deal Sources | `/v2/deal_sources` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/deal-sources/) |
| Deal Unqualified Reasons | `/v2/deal_unqualified_reasons` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/deal-unqualified-reasons/) |
| Deals | `/v2/deals` | 6 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/deals/) |
| Deals | `/v3/deals` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/deals/) |
| Documents | `/v2/documents` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/documents/) |
| Documents | `/v3/documents` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/documents/) |
| Filtering | `/v3/contacts/search` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/filtering/) |
| Graph Expressions | `/v3/contacts/search` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/graph-expressions/) |
| GraphQL | `/v3/graphql` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/graphql/) |
| Lead Conversions | `/v2/lead_conversions` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/lead-conversions/) |
| Lead Sources | `/v2/lead_sources` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/lead-sources/) |
| Lead Unqualified Reasons | `/v2/lead_unqualified_reasons` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/lead-unqualified-reasons/) |
| Leads | `/v2/leads` | 6 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/leads/) |
| Leads | `/v3/leads` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/leads/) |
| Line Items | `/v2/orders/:order_id/line_items` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/line-items/) |
| Line Items | `/v3/line_items` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/line-items/) |
| Loss Reasons | `/v2/loss_reasons` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/loss-reasons/) |
| Loss Reasons | `/v3/loss_reasons` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/loss-reasons/) |
| Notes | `/v2/notes` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/notes/) |
| Notes | `/v3/notes` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/notes/) |
| OAuth Reference | `/oauth2` | 4 | [docs](https://developer.zendesk.com/api-reference/sales-crm/authentication/reference/) |
| Orders | `/v2/orders` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/orders/) |
| Orders | `/v3/orders` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/orders/) |
| Pipelines | `/v2/pipelines` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/pipelines/) |
| Products | `/v2/products` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/products/) |
| Products | `/v3/products` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/products/) |
| Projections | `/v3/contacts` | 3 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/projections/) |
| Query Language | `/v3` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/query-language/) |
| Request Batching | `/v3/deals/search` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/batching/) |
| Schemas | `/v3` | 7 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/schemas/) |
| Sequence Enrollments | `/v2/sequence_enrollments` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/sequence-enrollments/) |
| Sequences | `/v2/sequences` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/sequences/) |
| Sorting | `/v3/deals/search` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/search/sorting/) |
| Sources | `/v3/sources` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/sources/) |
| Stages | `/v2/stages` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/stages/) |
| Stages | `/v3/stages` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/stages/) |
| Sync Reference | `/v2/sync` | 3 | [docs](https://developer.zendesk.com/api-reference/sales-crm/sync/reference/) |
| Tags | `/v2/tags` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/tags/) |
| Tags | `/v3/tags` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/tags/) |
| Tasks | `/v2/tasks` | 5 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/tasks/) |
| Tasks | `/v3/tasks` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/tasks/) |
| Text Messages | `/v2/text_messages` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/text-messages/) |
| Unqualified Reasons | `/v3/unqualified_reasons` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/unqualified-reasons/) |
| Users | `/v2/users` | 3 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/users/) |
| Users | `/v3/users` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/users/) |
| Visit Outcomes | `/v2/visit_outcomes` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/visit-outcomes/) |
| Visits | `/v2/visits` | 1 | [docs](https://developer.zendesk.com/api-reference/sales-crm/resources/visits/) |
| Visits | `/v3/visits` | 2 | [docs](https://developer.zendesk.com/api-reference/sales-crm/firehose/visits/) |

### Attachments — 1 groups, 1 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Attachment Content | `/api/v2/attachment_content` | 1 | [docs](https://developer.zendesk.com/api-reference/attachments/content/) |

### Introduction — 1 groups, 2 operations

| Resource group | Base path | Ops | Reference |
|---|---|---:|---|
| Documentation conventions | `/api/v2` | 2 | [docs](https://developer.zendesk.com/api-reference/introduction/doc-conventions/) |

---

*End of document.*
