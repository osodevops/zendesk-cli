# Migrating from Zendesk API tokens to OAuth

Zendesk is retiring API tokens. `zdk` is OAuth-first, and everything below works with the commands that ship in v0.1.0. (An interactive migration assistant is planned for a later release; it is not needed to migrate.)

## Why now: the deadlines

| Date | What happens |
|---|---|
| 28 July 2026 | Tokens unused for 30 days are deactivated; deactivated tokens are deleted after 60 days; accounts created on or after this date cannot create or use API tokens |
| **27 October 2026** | **No account can create new API tokens** (UI or API) |
| **30 April 2027** | **All remaining API tokens stop working**; token management pages are removed from Admin Center; webhook auth via API token stops |

Affected: Ticketing (Support), Help Center and Voice APIs. Not affected: Messaging and Chat product tokens. Email + password auth for the API already ended on 12 January 2026.

Sources: Zendesk's [announcement](https://support.zendesk.com/hc/en-us/articles/10851263566234-Announcing-the-removal-of-API-tokens-as-an-authentication-method-for-API-requests) and [migration guide](https://developer.zendesk.com/documentation/authentication/oauth-migration/).

`zdk` prints one stderr line per process whenever an API token is used, with the live countdown, for example:

```
warning: API token auth is deprecated. New tokens cannot be created after 27 Oct 2026 (46 days); all tokens stop working on 30 Apr 2027 (231 days). Run `zdk auth login` to switch to OAuth.
```

`--quiet` or `auth.suppress_deprecation = true` silences it; `zdk auth status` and `zdk doctor` report the same countdown (`api_token_days_remaining`, `api_token_days_until_creation_cutoff`). It never prints on stdout.

## Step 1 — create an OAuth client in Admin Center

Admin Center → **Apps and integrations** → **APIs** → **OAuth clients** → **Add OAuth client**.

| Field | Interactive use (laptops, SSH) | CI / automation |
|---|---|---|
| Client kind | **Public** (no secret; `zdk` uses PKCE) | **Confidential** (a secret is generated; store it in your CI secret manager) |
| Unique identifier | e.g. `zdk_local` — this is the `--client-id` | e.g. `zdk_ci` |
| Redirect URLs | `http://127.0.0.1/callback` — see below for ports | not used by the client-credentials grant |
| Allowed scopes | least privilege, e.g. `tickets:read tickets:write users:read organizations:read` | e.g. `tickets:read users:read` |

**Redirect URL and ports.** In the default browser flow `zdk` binds an *ephemeral* loopback port and sends `redirect_uri=http://127.0.0.1:<port>/callback`. Zendesk accepts any port for a registered `http://127.0.0.1` loopback redirect, so `http://127.0.0.1/callback` is enough. If your instance insists on an exact match, register a fixed port (say `http://127.0.0.1:9876/callback`) and pass the same one with `zdk auth login --port 9876` (or set `callback_port = 9876` in the profile or `[auth]`).

With `--no-browser` (no listener runs: you paste the code back) the redirect is `http://127.0.0.1:8484/callback` unless `--port` / `callback_port` says otherwise — register that URL too if you use the paste flow. With `--redirect-uri https://localhost` `zdk` sends exactly the URL you give, for clients registered with a non-loopback redirect: after signing in, copy the `code=` from the browser's address bar (or the whole URL) and paste it.

**Choosing scopes.** `zdk auth scopes list` prints the catalogue. The presets are a good starting point for *Allowed scopes*:

| Preset | Scopes |
|---|---|
| `agent` | `read tickets:read tickets:write users:read organizations:read hc:read` |
| `admin` | `agent` + `triggers:write automations:write macros:write webhooks:write` |
| `readonly` | every granular `*:read` scope |
| `exporter` | `tickets:read users:read organizations:read auditlogs:read` |

`zdk auth scopes preset agent` prints exactly what a preset expands to. Requesting a scope the client does not allow fails at login with `AUTH_INVALID_SCOPE` (exit 3) and names the scope. Never leave the requested scope empty — Zendesk treats that as full read+write, and `zdk` refuses to send it.

## Step 2 — log in interactively

```bash
zdk config init --subdomain acme --client-id zdk_local     # writes the profile; --non-interactive in scripts
zdk auth login                                            # browser + PKCE; requests the profile's scopes
zdk auth login --no-browser                               # SSH / containers: print the URL, paste the code back
zdk auth status                                           # grant, scopes, expiry, store backend
zdk auth whoami
zdk auth scopes check tickets update                      # {"required":["tickets:write"],"satisfied":true,…}
```

Keep the browser step prompt: authorization codes expire after 120 seconds, and `zdk` exchanges the code the instant the redirect arrives.

## Step 3 — CI and automation: a Confidential client with client credentials

```bash
ZENDESK_CLIENT_SECRET=… zdk --profile ci auth login --client-credentials --client-id zdk_ci --scopes tickets:read,users:read
```

The client-credentials grant issues **no refresh token**; `zdk` re-mints an access token from the client secret when the current one expires. The secret comes from `ZENDESK_CLIENT_SECRET`, `--client-secret`, or — with `--store-secret` at login — the credential store, so later jobs on the same host need no environment variable at all.

For fully stateless jobs, skip the store: a token minted elsewhere can be supplied as `ZENDESK_ACCESS_TOKEN`, which bypasses the credential store and is never refreshed.

## Step 4 — headless credential storage

Where the token lands is chosen by `--credential-store` / `ZENDESK_CREDENTIAL_STORE` / `[auth].credential_store`:

- `auto` (default) probes the OS keyring once and falls back to the encrypted file when there is none (Docker, CI runners, SSH sessions, Linux without a Secret Service); the decision is cached in `store-decision.json` under the state directory and re-probed by `zdk auth login` and `zdk doctor`.
- The encrypted file is `credentials.enc` beside `config.toml` (`~/.config/zendesk-cli/` on Linux), XChaCha20-Poly1305, mode `0600`. Its key is derived from `ZENDESK_CREDENTIALS_PASSPHRASE` with Argon2id when that variable is set, otherwise read from a machine key file `credentials.key` (32 random bytes, `0600`) created next to it on first use.

So on a headless host either export `ZENDESK_CREDENTIALS_PASSPHRASE` for every `zdk` invocation (log in and run with the same value), or keep `credentials.key` next to `credentials.enc` on that host. A file encrypted one way refuses to open the other way and says which key it needs. `--credential-store file` forces the file on a machine that also has a keyring; `none` keeps tokens in memory for the current process only (what the test suite uses).

## Step 5 — retire the API token

1. Keep the API-token profile in parallel until you are confident (both work until 30 April 2027).
2. Remove it: `zdk config profiles remove legacy` (the config entry) and `zdk --profile legacy auth logout` (the stored credential).
3. Audit other integrations: anything else using `{email}/token:{token}` Basic auth — webhooks, scripts, other CLIs — needs the same treatment before 30 April 2027.

## How Zendesk OAuth works (what `zdk` implements)

| Item | Fact |
|---|---|
| Endpoints | `GET https://{subdomain}.zendesk.com/oauth/authorizations/new`, `POST https://{subdomain}.zendesk.com/oauth/tokens` |
| `authorization_code` grant | Interactive default. `client_secret` is **optional**, so public clients work with PKCE (`code_challenge_method=S256`, `code_verifier` on exchange). `zdk` always sends and verifies a random `state`. Authorization codes expire after **120 seconds**; a late exchange is `AUTH_CODE_EXPIRED`. |
| `refresh_token` grant | **Rotates**: the previous access *and* refresh token are invalidated on every refresh. `zdk` persists the new pair before using it and never retries with a burned token; a save failure is exit 10 after the running command completes. Refresh is pre-emptive at 80 % of the access-token lifetime (`[auth].refresh_at_percent`). |
| `client_credentials` grant | **Confidential clients only; no refresh token is issued.** `zdk` re-mints a token from the client secret when the current one expires. Intended for CI and automation, never for desktop use. |
| Password grant | Deprecated; not implemented. |
| Access-token lifetime | `expires_in` defaults to **1800 s** (30 minutes) for OAuth clients created on or after **30 April 2026** (older clients: no expiry), configurable 300–172,800 s with `zdk auth login --expires-in`. |
| Refresh-token lifetime | `refresh_token_expires_in` defaults to 30 days, range 7–90 days. |
| Scopes | Space-separated. 52 granular scopes (`tickets:read`, `tickets:write`, `users:*`, `organizations:*`, `hc:*`, `triggers:*`, `macros:*`, `custom_objects:*`, `auditlogs:read`, `security:read`, …) plus the legacy blanket `read` / `write`. Implications: `write` covers everything, `read` covers every `*:read`, `X:write` covers `X:read`. |

## Known gaps

- Unified search (`GET /api/v2/search`) has no granular scope; `zdk search` requires the legacy `read` scope (a token with only `tickets:read` gets `SCOPE_MISSING` locally, or 403 from Zendesk).
- `organization_field` and `custom_objects` writes have no granular write scope on some accounts and need the global `write` scope; `custom_objects:write` exists in the catalogue but Zendesk may still answer 403 for writes without `write`.
- A static `ZENDESK_ACCESS_TOKEN` (and any API token) has unknown grants, so `zdk` cannot pre-flight scopes for it: a missing scope surfaces as `FORBIDDEN` from the server rather than `SCOPE_MISSING` locally.
