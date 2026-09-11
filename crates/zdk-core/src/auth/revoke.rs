//! Best-effort token revocation for `zdk auth logout`:
//! `GET /api/v2/oauth/tokens/current` → id → `DELETE /api/v2/oauth/tokens/{id}`.

use http::HeaderValue;
use serde::Deserialize;
use url::Url;

use super::oauth::endpoint;
use crate::{Result, ZdkError};

#[derive(Deserialize)]
struct CurrentToken {
    token: TokenRecord,
}

#[derive(Deserialize)]
struct TokenRecord {
    id: u64,
}

/// Revoke the token behind `bearer`. Returns the revoked token's id. Errors are returned,
/// not swallowed — the caller (logout) decides whether they matter.
pub async fn revoke_current_at(
    http: &reqwest::Client,
    base: &Url,
    bearer: &HeaderValue,
) -> Result<u64> {
    let current_url = endpoint(base, "api/v2/oauth/tokens/current")?;
    let response = http
        .get(current_url)
        .header(http::header::AUTHORIZATION, bearer.clone())
        .header(http::header::ACCEPT, "application/json")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(ZdkError::Other(format!(
            "could not look up the current token: GET /api/v2/oauth/tokens/current returned {status}"
        )));
    }
    let current: CurrentToken = response.json().await.map_err(|e| {
        ZdkError::Other(format!(
            "could not read the current token: unexpected response shape ({e})"
        ))
    })?;

    let delete_url = endpoint(base, &format!("api/v2/oauth/tokens/{}", current.token.id))?;
    let response = http
        .delete(delete_url)
        .header(http::header::AUTHORIZATION, bearer.clone())
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(ZdkError::Other(format!(
            "could not revoke token {}: DELETE /api/v2/oauth/tokens/{} returned {status}",
            current.token.id, current.token.id
        )));
    }
    Ok(current.token.id)
}
