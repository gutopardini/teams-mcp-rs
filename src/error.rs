use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Resource is outside the configured access policy")]
    Forbidden,
    #[error("Invalid request")]
    InvalidInput,
    #[error("Invalid or unsafe pagination cursor")]
    InvalidCursor,
    #[error("Response exceeded the configured size limit")]
    ResponseTooLarge,
    #[error("Sign-in is required")]
    AuthenticationRequired,
    #[error("Authentication failed")]
    AuthenticationFailed,
    #[error("The application ID cannot request these permissions")]
    IncompatibleClient,
    #[error("Authentication was cancelled")]
    AuthenticationCancelled,
    #[error("The sign-in window is unavailable")]
    BrowserUnavailable,
    #[error("The signed-in account does not match the configured account")]
    AccountMismatch,
    #[error("The secure credential store is unavailable")]
    CredentialStore,
    #[error("The credential store is not supported on this platform yet")]
    UnsupportedPlatform,
    #[error("The Microsoft service could not be reached")]
    Network,
    #[error("The Microsoft service returned an error")]
    Upstream(u16),
    #[error("The Microsoft service returned an invalid response")]
    InvalidResponse,
    #[error("Invalid configuration")]
    InvalidConfig,
    #[error("No read resources have been configured")]
    NoResources,
    #[error("The MCP transport failed")]
    Protocol,
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::InvalidInput => "invalid_input",
            Self::InvalidCursor => "invalid_cursor",
            Self::ResponseTooLarge => "response_too_large",
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthenticationFailed => "authentication_failed",
            Self::IncompatibleClient => "incompatible_client",
            Self::AuthenticationCancelled => "authentication_cancelled",
            Self::BrowserUnavailable => "browser_unavailable",
            Self::AccountMismatch => "account_mismatch",
            Self::CredentialStore => "credential_store_unavailable",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::Network => "network_error",
            Self::Upstream(_) => "upstream_error",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidConfig => "invalid_config",
            Self::NoResources => "no_resources",
            Self::Protocol => "protocol_error",
        }
    }

    pub fn message_key(&self) -> &'static str {
        match self {
            Self::Forbidden => "error-forbidden",
            Self::InvalidInput => "error-invalid-input",
            Self::InvalidCursor => "error-invalid-cursor",
            Self::ResponseTooLarge => "error-response-too-large",
            Self::AuthenticationRequired => "error-authentication-required",
            Self::AuthenticationFailed => "error-authentication-failed",
            Self::IncompatibleClient => "error-incompatible-client",
            Self::AuthenticationCancelled => "error-authentication-cancelled",
            Self::BrowserUnavailable => "error-browser-unavailable",
            Self::AccountMismatch => "error-account-mismatch",
            Self::CredentialStore => "error-credential-store",
            Self::UnsupportedPlatform => "error-unsupported-platform",
            Self::Network => "error-network",
            Self::Upstream(_) => "error-upstream",
            Self::InvalidResponse => "error-invalid-response",
            Self::InvalidConfig => "error-invalid-config",
            Self::NoResources => "error-no-resources",
            Self::Protocol => "error-protocol",
        }
    }
}
