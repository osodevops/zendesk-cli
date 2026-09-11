# Migrating from Zendesk API tokens to OAuth

> **Status:** placeholder. The interactive assistant `zdk auth migrate` (and `zdk auth migrate --report`) is planned for **v0.7**. Until then this page holds the dated facts and the manual path using the auth commands that ship in v0.1.0.

## The deadlines

| Date | What happens |
|---|---|
| 28 July 2026 | Tokens unused for 30 days are deactivated; deactivated tokens are deleted after 60 days; accounts created on or after this date cannot create or use API tokens |
| **27 October 2026** | **No account can create new API tokens** (UI or API) |
| **30 April 2027** | **All remaining API tokens stop working**; token management pages are removed from Admin Center; webhook auth via API token stops |

Affected: Ticketing (Support), Help Center and Voice APIs. Not affected: Messaging and Chat product tokens. Email + password auth for the API already ended on 12 January 2026.

Sources: Zendesk's [announcement](https://support.zendesk.com/hc/en-us/articles/10851263566234-Announcing-the-removal-of-API-tokens-as-an-authentication-method-for-API-requests) and [migration guide](https://developer.zendesk.com/documentation/authentication/oauth-migration/).

`zdk` prints a one-line stderr warning with the live countdown on every API-token invocation (`--quiet` or `auth.suppress_deprecation = true` silences it). It never prints on stdout.

## How Zendesk OAuth works (what `zdk` implements)

| Item | Fact |
|---|---|
| Endpoints | `GET https://{subdomain}.zendesk.com/oauth/authorizations/new`, `POST https://{subdomain}.zendesk.com/oauth/tokens` |
| `authorization_code` grant | Interactive default. `client_secret` is **optional**, so public clients work with PKCE (`code_challenge_method=S256`, `code_verifier` on exchange). `zdk` always sends and verifies a random `state`. Authorization codes expire after **120 seconds**; `zdk` exchanges immediately and names the expiry when the window is missed. |
| `refresh_token` grant | **Rotates**: the previous access *and* refresh token are invalidated on every refresh. `zdk` persists the new pair before using it and never retries with a burned token. |
| `client_credentials` grant | **Confidential clients only; no refresh token is issued.** `zdk` re-mints a token from the client secret when the current one expires. Intended for CI and automation, never for desktop use. |
| Password grant | Deprecated; not implemented. |
| Access-token lifetime | `expires_in` defaults to **1800 s** (30 minutes) for OAuth clients created on or after **30 April 2026** (older clients: no expiry), configurable 300–172 800 s. `zdk` refreshes pre-emptively at 80 % of the TTL. |
| Refresh-token lifetime | `refresh_token_expires_in` defaults to 30 days, range 7–90 days. |
| Scopes | Space-separated. 52 granular scopes (`tickets:read`, `tickets:write`, `users:*`, `organizations:*`, `hc:*`, `triggers:*`, `macros:*`, `custom_objects:*`, `auditlogs:read`, `security:read`, …) plus the legacy blanket `read` / `write`. Requesting a scope outside the client's *Allowed scopes* fails with `400 invalid_scope`; `zdk` names the offending scope and the Admin Center screen to fix it. **An empty scope grants full read+write**, so `zdk` refuses to request one. Known gap: `organization_field` and `custom_objects` have no granular write scope and need global `write`. |

## Manual migration with zdk v0.1.0

1. **Create an OAuth client** in Admin Center → *Apps and integrations* → *APIs* → *OAuth clients*.
   - For interactive use: a **Public** client (no secret; PKCE). Redirect URL `http://127.0.0.1/callback` (add the port, e.g. `http://127.0.0.1:9876/callback`, if your instance requires an exact match and pass the same port with `--port 9876`).
   - For CI: a **Confidential** client (client credentials). Store the secret in your CI secret manager as `ZENDESK_CLIENT_SECRET`.
   - Set *Allowed scopes* to the least privilege you need, e.g. `tickets:read tickets:write users:read organizations:read`.
2. **Log in with OAuth** and verify:

   ```bash
   zdk auth login --subdomain acme --client-id zdk_local --preset agent --profile acme
   zdk auth status
   zdk auth whoami
   zdk auth scopes check tickets update     # "requires tickets:write — granted"
   ```

   Headless / CI:

   ```bash
   ZENDESK_CLIENT_SECRET=… zdk auth login --client-credentials --subdomain acme --client-id zdk_ci --scopes tickets:read,users:read --profile ci
   ```
3. **Keep the API-token profile in parallel** until you are confident (both work until 30 April 2027), then remove it: `zdk config profiles remove legacy` and `zdk auth logout --profile legacy`.
4. **Audit other integrations**: anything else using `{email}/token:{token}` Basic auth — webhooks, scripts, other CLIs — needs the same treatment before 30 April 2027.

## TODO (v0.7 — `zdk auth migrate`)

- Detect the current auth method and, where the account permits, list API tokens with `last_used` and *Deactivates on*.
- Print the exact Admin Center path and the client kind to choose.
- Recommend an *Allowed scopes* set derived from the local `--audit-log` — least privilege by evidence.
- Run the new flow, verify with `GET /api/v2/users/me`, store the credential, keep the old token configured until the user confirms.
- `zdk auth migrate --report`: markdown report of every integration hostname / user-agent seen in the account's API-token usage report.
