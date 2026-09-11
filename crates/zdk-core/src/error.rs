//! The single error type for `zdk-core` and the process exit-code contract (PRD §14.3).

use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

/// Result alias used throughout `zdk-core`.
pub type Result<T, E = ZdkError> = std::result::Result<T, E>;

/// Why authentication is not usable right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthFailure {
    /// No credential is configured for the active profile.
    NotLoggedIn { profile: String },
    /// The access token expired and could not be refreshed.
    Expired { profile: String, detail: String },
    /// Zendesk rejected the refresh token (revoked, rotated elsewhere, or expired).
    Revoked { profile: String, detail: String },
    /// A client-credentials profile needs its secret to mint a token.
    ClientSecretRequired { profile: String },
    /// A requested scope is outside the OAuth client's allowed scopes.
    InvalidScope { scope: String, detail: String },
    /// The token endpoint returned an error we do not classify further.
    TokenEndpoint {
        status: u16,
        error: String,
        description: String,
    },
    /// The authorization code was not exchanged within Zendesk's 120 s window (or is otherwise invalid).
    CodeExpired,
    /// The interactive flow was abandoned (timeout, state mismatch, user cancelled).
    FlowAborted { detail: String },
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLoggedIn { profile } => write!(f, "not authenticated for profile '{profile}'"),
            Self::Expired { profile, detail } => {
                write!(
                    f,
                    "access token for profile '{profile}' expired and could not be refreshed: {detail}"
                )
            }
            Self::Revoked { profile, detail } => {
                write!(
                    f,
                    "refresh token for profile '{profile}' was rejected: {detail}"
                )
            }
            Self::ClientSecretRequired { profile } => {
                write!(
                    f,
                    "profile '{profile}' uses client credentials but no client secret is available"
                )
            }
            Self::InvalidScope { scope, detail } => {
                write!(f, "scope '{scope}' was rejected: {detail}")
            }
            Self::TokenEndpoint {
                status,
                error,
                description,
            } => {
                write!(f, "token endpoint returned {status} {error}: {description}")
            }
            Self::CodeExpired => write!(f, "authorization code expired or was already used"),
            Self::FlowAborted { detail } => write!(f, "login flow aborted: {detail}"),
        }
    }
}

/// Which rate-limit budget was exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateBudget {
    /// Human name, e.g. `account`, `incremental exports`, `ticket update (per ticket)`.
    pub name: String,
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
}

/// Field-level validation detail extracted from a Zendesk error body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationDetail {
    pub field: String,
    pub message: String,
}

/// Everything that can go wrong. Variants map 1:1 onto exit codes via [`ZdkError::exit_code`].
#[derive(Debug, Error, Diagnostic)]
pub enum ZdkError {
    #[error("{0}")]
    #[diagnostic(code(zdk::usage))]
    Usage(String),

    #[error("{0}")]
    #[diagnostic(code(zdk::auth))]
    Auth(AuthFailure),

    #[error("forbidden: {message}")]
    #[diagnostic(code(zdk::forbidden))]
    Forbidden {
        message: String,
        required_scope: Option<String>,
        granted: Vec<String>,
        request_id: Option<String>,
    },

    #[error("{resource} '{id}' not found")]
    #[diagnostic(code(zdk::not_found))]
    NotFound {
        resource: String,
        id: String,
        request_id: Option<String>,
    },

    #[error("Zendesk rejected the request ({status}): {message}")]
    #[diagnostic(code(zdk::validation))]
    Validation {
        status: u16,
        message: String,
        details: Vec<ValidationDetail>,
        request_id: Option<String>,
    },

    #[error("rate limit exceeded ({budget})", budget = budget.name)]
    #[diagnostic(code(zdk::rate_limited))]
    RateLimited {
        budget: RateBudget,
        retry_after: Option<std::time::Duration>,
        request_id: Option<String>,
    },

    #[error("bulk operation completed with failures: {failed} of {total} records failed")]
    #[diagnostic(code(zdk::partial_failure))]
    PartialFailure {
        total: usize,
        failed: usize,
        report: Option<std::path::PathBuf>,
    },

    #[error("Zendesk server error {status} after {attempts} attempt(s)")]
    #[diagnostic(code(zdk::server))]
    Server {
        status: u16,
        attempts: u32,
        request_id: Option<String>,
        body: String,
    },

    #[error("configuration error: {0}")]
    #[diagnostic(code(zdk::config))]
    Config(String),

    #[error("credential store error: {0}")]
    #[diagnostic(code(zdk::credential_store))]
    CredentialStore(String),

    #[error("network error: {0}")]
    #[diagnostic(code(zdk::network))]
    Network(#[from] reqwest::Error),

    #[error("offset pagination limit reached on {endpoint}")]
    #[diagnostic(code(zdk::pagination_limit))]
    PaginationLimit {
        endpoint: String,
        alternatives: Vec<String>,
    },

    #[error("job {job_id} did not finish within {timeout_secs}s")]
    #[diagnostic(code(zdk::job_timeout))]
    JobTimeout { job_id: String, timeout_secs: u64 },

    #[error("interrupted")]
    #[diagnostic(code(zdk::interrupted))]
    Interrupted,

    /// `--dry-run` rendered the request instead of sending it. Maps to exit 0.
    #[error("dry run: request not sent")]
    #[diagnostic(code(zdk::dry_run))]
    DryRun,

    #[error("I/O error: {0}")]
    #[diagnostic(code(zdk::io))]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    #[diagnostic(code(zdk::other))]
    Other(String),
}

impl ZdkError {
    /// Process exit code (PRD §14.3).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::DryRun => 0,
            Self::Other(_) | Self::Io(_) => 1,
            Self::Usage(_) => 2,
            Self::Auth(_) => 3,
            Self::Forbidden { .. } => 4,
            Self::NotFound { .. } => 5,
            Self::Validation { .. } => 6,
            Self::RateLimited { .. } => 7,
            Self::PartialFailure { .. } => 8,
            Self::Server { .. } => 9,
            Self::Config(_) | Self::CredentialStore(_) => 10,
            Self::Network(_) => 11,
            Self::PaginationLimit { .. } => 12,
            Self::JobTimeout { .. } => 13,
            Self::Interrupted => 130,
        }
    }

    /// Stable machine-readable code for the JSON error line on stderr.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Usage(_) => "USAGE",
            Self::Auth(AuthFailure::NotLoggedIn { .. }) => "AUTH_NOT_LOGGED_IN",
            Self::Auth(AuthFailure::Expired { .. }) => "AUTH_EXPIRED",
            Self::Auth(AuthFailure::Revoked { .. }) => "AUTH_REVOKED",
            Self::Auth(AuthFailure::ClientSecretRequired { .. }) => "AUTH_CLIENT_SECRET_REQUIRED",
            Self::Auth(AuthFailure::InvalidScope { .. }) => "AUTH_INVALID_SCOPE",
            Self::Auth(AuthFailure::TokenEndpoint { .. }) => "AUTH_TOKEN_ENDPOINT",
            Self::Auth(AuthFailure::CodeExpired) => "AUTH_CODE_EXPIRED",
            Self::Auth(AuthFailure::FlowAborted { .. }) => "AUTH_FLOW_ABORTED",
            Self::Forbidden {
                required_scope: Some(_),
                ..
            } => "SCOPE_MISSING",
            Self::Forbidden { .. } => "FORBIDDEN",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Validation { .. } => "VALIDATION",
            Self::RateLimited { .. } => "RATE_LIMITED",
            Self::PartialFailure { .. } => "PARTIAL_FAILURE",
            Self::Server { .. } => "SERVER_ERROR",
            Self::Config(_) => "CONFIG",
            Self::CredentialStore(_) => "CREDENTIAL_STORE",
            Self::Network(_) => "NETWORK",
            Self::PaginationLimit { .. } => "PAGINATION_LIMIT",
            Self::JobTimeout { .. } => "JOB_TIMEOUT",
            Self::Interrupted => "INTERRUPTED",
            Self::DryRun => "DRY_RUN",
            Self::Io(_) => "IO",
            Self::Other(_) => "ERROR",
        }
    }

    /// Zendesk's request correlation id, when the failure came from an HTTP response.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Forbidden { request_id, .. }
            | Self::NotFound { request_id, .. }
            | Self::Validation { request_id, .. }
            | Self::RateLimited { request_id, .. }
            | Self::Server { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    /// Human guidance shown under the error (miette `help`).
    #[must_use]
    pub fn help_text(&self) -> Option<String> {
        match self {
            Self::Auth(AuthFailure::NotLoggedIn { .. }) => {
                Some("Run `zdk auth login` (browser), `zdk auth login --no-browser` (SSH), or `zdk auth login --client-credentials` (CI).".into())
            }
            Self::Auth(AuthFailure::Expired { .. } | AuthFailure::Revoked { .. }) => {
                Some("A refresh was attempted and failed — the refresh token may be revoked or expired. Run `zdk auth login` to re-authenticate.".into())
            }
            Self::Auth(AuthFailure::ClientSecretRequired { .. }) => Some(
                "Set ZENDESK_CLIENT_SECRET, pass --client-secret, or re-run `zdk auth login --client-credentials --store-secret`.".into(),
            ),
            Self::Auth(AuthFailure::InvalidScope { scope, .. }) => Some(format!(
                "'{scope}' is outside this OAuth client's Allowed scopes. Add it in Admin Center → Apps and integrations → APIs → OAuth clients, or request fewer scopes with --scopes."
            )),
            Self::Auth(AuthFailure::CodeExpired) => Some(
                "Zendesk authorization codes are valid for 120 seconds. Run `zdk auth login` again and complete the browser step promptly.".into(),
            ),
            Self::Forbidden { required_scope: Some(scope), granted, .. } => Some(format!(
                "This command requires {scope}. Granted: {}. Run `zdk auth login --scopes <list>` to re-authenticate with it (add it to the client's Allowed scopes first if needed).",
                if granted.is_empty() { "(unknown)".to_string() } else { granted.join(", ") }
            )),
            Self::Forbidden { .. } => Some("Check the agent's role and permissions in Zendesk Admin Center, then retry.".into()),
            Self::NotFound { .. } => Some("If that value is a name rather than an id, try the resource's `search` command first.".into()),
            Self::RateLimited { retry_after, .. } => Some(match retry_after {
                Some(d) => format!("Zendesk asked us to wait {}s. Use --rate-limit-strategy wait to pause and resume automatically.", d.as_secs()),
                None => "Use --rate-limit-strategy wait to pause and resume automatically, or lower --max-concurrency.".into(),
            }),
            Self::PaginationLimit { alternatives, .. } => Some(format!(
                "Zendesk caps offset pagination at 100 pages / 10,000 records. Use one of:\n  {}",
                alternatives.join("\n  ")
            )),
            Self::Server { .. } => Some(
                "Check https://status.zendesk.com (or `zdk api GET https://status.zendesk.com/api/incidents`) and retry later.".into(),
            ),
            Self::Validation { status: 409, .. } => Some("The ticket changed since you read it. Re-read it, or use --safe-update with a fresh --updated-stamp.".into()),
            Self::Config(_) => Some("Run `zdk config validate` to see every problem, or `zdk config init` to start over.".into()),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ZdkError {
    fn from(e: serde_json::Error) -> Self {
        Self::Other(format!("JSON error: {e}"))
    }
}

impl From<url::ParseError> for ZdkError {
    fn from(e: url::ParseError) -> Self {
        Self::Usage(format!("invalid URL: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_prd_table() {
        let cases: Vec<(ZdkError, i32)> = vec![
            (ZdkError::Other("x".into()), 1),
            (ZdkError::Usage("x".into()), 2),
            (
                ZdkError::Auth(AuthFailure::NotLoggedIn {
                    profile: "p".into(),
                }),
                3,
            ),
            (
                ZdkError::Forbidden {
                    message: "m".into(),
                    required_scope: None,
                    granted: vec![],
                    request_id: None,
                },
                4,
            ),
            (
                ZdkError::NotFound {
                    resource: "ticket".into(),
                    id: "1".into(),
                    request_id: None,
                },
                5,
            ),
            (
                ZdkError::Validation {
                    status: 422,
                    message: "m".into(),
                    details: vec![],
                    request_id: None,
                },
                6,
            ),
            (
                ZdkError::RateLimited {
                    budget: RateBudget {
                        name: "account".into(),
                        limit: None,
                        remaining: None,
                    },
                    retry_after: None,
                    request_id: None,
                },
                7,
            ),
            (
                ZdkError::PartialFailure {
                    total: 2,
                    failed: 1,
                    report: None,
                },
                8,
            ),
            (
                ZdkError::Server {
                    status: 503,
                    attempts: 6,
                    request_id: None,
                    body: String::new(),
                },
                9,
            ),
            (ZdkError::Config("c".into()), 10),
            (ZdkError::CredentialStore("c".into()), 10),
            (
                ZdkError::PaginationLimit {
                    endpoint: "e".into(),
                    alternatives: vec![],
                },
                12,
            ),
            (
                ZdkError::JobTimeout {
                    job_id: "j".into(),
                    timeout_secs: 1,
                },
                13,
            ),
            (ZdkError::Interrupted, 130),
            (ZdkError::DryRun, 0),
        ];
        for (err, code) in cases {
            assert_eq!(err.exit_code(), code, "{err}");
        }
    }

    #[test]
    fn error_codes_are_stable_strings() {
        assert_eq!(ZdkError::Interrupted.error_code(), "INTERRUPTED");
        assert_eq!(
            ZdkError::Forbidden {
                message: String::new(),
                required_scope: Some("tickets:write".into()),
                granted: vec![],
                request_id: None
            }
            .error_code(),
            "SCOPE_MISSING"
        );
    }
}
