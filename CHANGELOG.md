# Changelog

All notable changes to `zendesk-cli` (`zdk`) are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

Releases are version-bump driven: a release is created when `[workspace.package] version`
in the root `Cargo.toml` changes on `main`. The matching `## [X.Y.Z]` section below becomes
the GitHub Release body, so every release PR must add one. Unreleased work goes under
`## [Unreleased]` and is moved into the versioned section by the release PR.

## [Unreleased]

### Added

- **Authentication** (`zdk auth`): OAuth authorization code with PKCE on a single-use loopback listener (`--port`, `--no-browser` with a paste-back flow, `--redirect-uri`, `--expires-in`), client credentials for CI (`--client-credentials`, `--store-secret`), and the legacy API-token ramp (`--api-token`) that prints a one-line countdown to the 27 Oct 2026 / 30 Apr 2027 deadlines. `status`, `whoami`, `refresh [--force]`, `logout [--no-revoke]`, `test`, `token --format raw|bearer|json`, and `scopes list|show|check|preset` over the 52-scope catalogue with the `agent`, `admin`, `readonly` and `exporter` presets. Pre-emptive refresh at 80 % of the token lifetime with rotation persisted before use; local scope pre-flight (`SCOPE_MISSING`, exit 4) before any request.
- **Credential stores**: OS keyring (service `zendesk-cli`), encrypted file `credentials.enc` (XChaCha20-Poly1305 with an Argon2id passphrase or a machine key file), environment (`ZENDESK_ACCESS_TOKEN`, `ZENDESK_EMAIL` + `ZENDESK_API_TOKEN`) and in-memory (`none`); `auto` probes the keyring once and caches the decision.
- **Configuration** (`zdk config`): `init` (commented starter file, `--non-interactive`), `show [--effective [--reveal-secrets]]`, `get`, `set`, `edit`, `validate`, `path`, and `profiles list|add|remove|rename|switch`; precedence flag → `ZENDESK_*` → profile → `[default]` → built-in; XDG directories honoured on every OS; no secret is ever a config field.
- **HTTP core**: one `ZendeskClient::execute` pipeline with scope pre-flight, `--dry-run` (exact request as JSON or a `curl` line, zero quota), a rate governor that learns the account limit from response headers and enforces twelve per-endpoint rules keyed the way Zendesk keys them (`--rate-limit-strategy wait|fail|burst`, `--max-concurrency`), `Retry-After`-aware retries with full-jitter backoff (`--retries`, `--timeout`, `[retry]`), a stable `Idempotency-Key` on every POST/PUT/PATCH (`--idempotency-key`), secret redaction at `-vvv`, and `--audit-log` NDJSON records.
- **Pagination**: cursor by default with `--all`/`--paginate`, `--limit`, `--page-size` and resumable `--checkpoint` files; an early guard that stops offset walks before Zendesk's 100-page / 10,000-record wall (`PAGINATION_LIMIT`, exit 12) and a warning at the search API's 1,000-result cap.
- **Operation registry**: 887 operations generated from Zendesk's Support, Help Center and Talk OpenAPI specs (`cargo xtask codegen`, `spec-refresh`, `spec-diff`), browsable with `zdk api ops` and `zdk api describe [--schema]`, and used for pagination dialect, items key and scope inference.
- **API escape hatch** (`zdk api`): any method and path (or absolute URL on the profile's host, plus `status.zendesk.com`) with `--data @file|-|json`, `--field k=v|k:=json`, `--query`, `--header`, `--items-key`, `--raw` and `--no-preflight`, through the same auth, governor, retry and pagination stack.
- **Curated commands**: `tickets` (list with a filter compiler that targets search or search/export, get/show with embedded comments, count, recent, create/update with `--file`/`--field` merging and `--safe-update`, reply/note/solve/close/reopen/assign, delete/restore/permanently-delete), `comments` (list/get/count/make-private/redact), `users` (list/search/autocomplete/get by id, email or `me`/me/related/create/create-or-update/update/delete), `orgs` (list/get/search/autocomplete/count/related/tickets/users/create/update/delete) and `search` (results/count/export/explain).
- **Diagnostics**: `zdk doctor [--json]` checks config, profile, credential store, credentials, the API-token deadline, auth, clock skew, rate-limit headers and the registry.
- **Output**: `table` on a terminal, `json` when piped, plus `ndjson`, `csv`, `tsv`, `yaml` and `raw`; `--fields`/`--exclude` projection, `--compact`, `--sideload` joins, an embedded jq filter (`--jq`), and one-line JSON errors on stderr with stable `code` strings and exit codes; `zdk --help-json` for tool-definition generation.
- **Shell integration**: `zdk completions bash|zsh|fish|powershell|elvish` and `zdk man --to DIR` (one page per subcommand), both shipped in release archives.
- **Distribution**: 7-target release pipeline (macOS arm64/x64, Linux musl arm64/x64 and glibc x64, Windows x64/arm64) with build-provenance attestations, `checksums-sha256.txt`, Homebrew tap and Scoop bucket updates.
