use std::{sync::Arc, time::Duration};

#[cfg(target_os = "linux")]
use std::process::Stdio;

use async_trait::async_trait;
use reqwest::{Client, Response, redirect::Policy};
use serde::Deserialize;
#[cfg(target_os = "linux")]
use tokio::{io::AsyncWriteExt, process::Command};
use tokio::{sync::Mutex, time::Instant};
use url::Url;
use zeroize::Zeroizing;

use crate::{
    config::{Config, Identity},
    error::{AppError, Result},
};

const AUTHORITY: &str = "https://login.microsoftonline.com/";
const GRAPH_ME: &str = "https://graph.microsoft.com/v1.0/me?$select=id";
const MAX_AUTH_RESPONSE: usize = 64 * 1024;

pub fn http_client() -> Result<Client> {
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("teams-mcp-rs/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::Network)
}

pub async fn bounded_body(mut response: Response, max_bytes: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(AppError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AppError::Network)? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn load(&self, identity: &Identity) -> Result<Zeroizing<String>>;
    async fn save(&self, identity: &Identity, refresh_token: &str) -> Result<()>;
    async fn clear(&self, identity: &Identity) -> Result<()>;
}

/// Uses a dedicated Secret Service entry; it never reads the desktop client's entry.
pub struct SystemSecretStore;

#[cfg(target_os = "linux")]
async fn secret_tool(action: &str, identity: &Identity, value: Option<&str>) -> Result<Vec<u8>> {
    let mut command = Command::new("secret-tool");
    command.arg(action);
    if action == "store" {
        command.args(["--label", "Teams MCP session"]);
    }
    let tenant = identity.tenant_id.to_string();
    let client = identity.client_id.to_string();
    let account = identity.account_id.to_string();
    command
        .args([
            "application",
            "teams-mcp-rs",
            "tenant",
            &tenant,
            "client",
            &client,
            "account",
            &account,
        ])
        .stdin(if value.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| AppError::CredentialStore)?;
    if let Some(value) = value {
        let mut stdin = child.stdin.take().ok_or(AppError::CredentialStore)?;
        tokio::time::timeout(Duration::from_secs(15), stdin.write_all(value.as_bytes()))
            .await
            .map_err(|_| AppError::CredentialStore)?
            .map_err(|_| AppError::CredentialStore)?;
        drop(stdin);
    }
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .map_err(|_| AppError::CredentialStore)?
        .map_err(|_| AppError::CredentialStore)?;
    if !output.status.success() {
        if action == "lookup" && output.stderr.is_empty() {
            return Err(AppError::AuthenticationRequired);
        }
        return Err(AppError::CredentialStore);
    }
    if output.stdout.len() > MAX_AUTH_RESPONSE {
        return Err(AppError::CredentialStore);
    }
    Ok(output.stdout)
}

#[async_trait]
impl SecretStore for SystemSecretStore {
    async fn load(&self, identity: &Identity) -> Result<Zeroizing<String>> {
        #[cfg(target_os = "linux")]
        {
            let output = secret_tool("lookup", identity, None).await?;
            let mut secret =
                Zeroizing::new(String::from_utf8(output).map_err(|_| AppError::CredentialStore)?);
            while secret.ends_with(['\n', '\r']) {
                secret.pop();
            }
            if secret.is_empty() {
                return Err(AppError::AuthenticationRequired);
            }
            Ok(secret)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = identity;
            Err(AppError::UnsupportedPlatform)
        }
    }

    async fn save(&self, identity: &Identity, refresh_token: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            secret_tool("store", identity, Some(refresh_token)).await?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (identity, refresh_token);
            Err(AppError::UnsupportedPlatform)
        }
    }

    async fn clear(&self, identity: &Identity) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            secret_tool("clear", identity, None).await?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = identity;
            Err(AppError::UnsupportedPlatform)
        }
    }
}

#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn access_token(&self) -> Result<Zeroizing<String>>;
}

struct CachedToken {
    value: Zeroizing<String>,
    expires_at: Instant,
}

pub struct MicrosoftAuth {
    config: Config,
    store: Arc<dyn SecretStore>,
    client: Client,
    authority: Url,
    graph_me: Url,
    cached: Mutex<Option<CachedToken>>,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
}

#[derive(Deserialize)]
struct GraphMe {
    id: uuid::Uuid,
}

fn default_interval() -> u64 {
    5
}

impl MicrosoftAuth {
    pub fn new(config: Config, store: Arc<dyn SecretStore>) -> Result<Self> {
        Ok(Self {
            config,
            store,
            client: http_client()?,
            authority: Url::parse(AUTHORITY).expect("valid fixed authority"),
            graph_me: Url::parse(GRAPH_ME).expect("valid fixed Graph profile URL"),
            cached: Mutex::new(None),
        })
    }

    fn endpoint(&self, endpoint: &str) -> Result<Url> {
        let mut url = self.authority.clone();
        url.path_segments_mut()
            .map_err(|_| AppError::InvalidConfig)?
            .pop_if_empty()
            .push(&self.config.identity.tenant_id.to_string())
            .push("oauth2")
            .push("v2.0")
            .push(endpoint);
        Ok(url)
    }

    /// Called only by the explicit CLI login command, not by the MCP server.
    pub async fn login(&self) -> Result<()> {
        let scope = self.config.scopes()?.join(" ");
        let client_id = self.config.identity.client_id.to_string();
        let response = self
            .client
            .post(self.endpoint("devicecode")?)
            .form(&[("client_id", client_id.as_str()), ("scope", scope.as_str())])
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        if !response.status().is_success() {
            return Err(AppError::AuthenticationFailed);
        }
        let data = Zeroizing::new(bounded_body(response, MAX_AUTH_RESPONSE).await?);
        let device: DeviceCode =
            serde_json::from_slice(&data).map_err(|_| AppError::InvalidResponse)?;
        if !safe_verification(&device.verification_uri, &device.user_code)
            || device.expires_in == 0
            || device.expires_in > 1800
        {
            return Err(AppError::InvalidResponse);
        }
        println!(
            "Open {} and enter code {}",
            device.verification_uri, device.user_code
        );
        let device_code = Zeroizing::new(device.device_code);
        let mut interval = device.interval.clamp(1, 60);
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        loop {
            if Instant::now() + Duration::from_secs(interval) >= deadline {
                return Err(AppError::AuthenticationFailed);
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let response = self
                .client
                .post(self.endpoint("token")?)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", client_id.as_str()),
                    ("device_code", device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|_| AppError::Network)?;
            let success = response.status().is_success();
            let data = Zeroizing::new(bounded_body(response, MAX_AUTH_RESPONSE).await?);
            if success {
                let token: TokenResponse =
                    serde_json::from_slice(&data).map_err(|_| AppError::InvalidResponse)?;
                let access = Zeroizing::new(token.access_token);
                let refresh = token.refresh_token.ok_or(AppError::AuthenticationFailed)?;
                let refresh = Zeroizing::new(refresh);
                if access.is_empty() || refresh.is_empty() || token.expires_in == 0 {
                    return Err(AppError::InvalidResponse);
                }
                self.verify_account(access.as_str()).await?;
                self.store
                    .save(&self.config.identity, refresh.as_str())
                    .await?;
                return Ok(());
            }
            let error: TokenError =
                serde_json::from_slice(&data).map_err(|_| AppError::AuthenticationFailed)?;
            match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval = interval.saturating_add(5).min(60),
                _ => return Err(AppError::AuthenticationFailed),
            }
        }
    }

    pub async fn logout(&self) -> Result<()> {
        self.store.clear(&self.config.identity).await?;
        *self.cached.lock().await = None;
        Ok(())
    }

    async fn verify_account(&self, access_token: &str) -> Result<()> {
        let response = self
            .client
            .get(self.graph_me.clone())
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        if !response.status().is_success() {
            return Err(AppError::AuthenticationFailed);
        }
        let data = Zeroizing::new(bounded_body(response, MAX_AUTH_RESPONSE).await?);
        let profile: GraphMe =
            serde_json::from_slice(&data).map_err(|_| AppError::InvalidResponse)?;
        if profile.id != self.config.identity.account_id {
            return Err(AppError::AccountMismatch);
        }
        Ok(())
    }
}

fn safe_verification(uri: &str, code: &str) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("microsoft.com" | "www.microsoft.com" | "login.microsoftonline.com")
        )
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
        && (4..=32).contains(&code.len())
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[async_trait]
impl TokenProvider for MicrosoftAuth {
    async fn access_token(&self) -> Result<Zeroizing<String>> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.expires_at > Instant::now())
        {
            return Ok(token.value.clone());
        }
        let refresh = self.store.load(&self.config.identity).await?;
        let client_id = self.config.identity.client_id.to_string();
        let scope = self.config.scopes()?.join(" ");
        let response = self
            .client
            .post(self.endpoint("token")?)
            .form(&[
                ("client_id", client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("scope", scope.as_str()),
            ])
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        if !response.status().is_success() {
            return Err(AppError::AuthenticationFailed);
        }
        let data = Zeroizing::new(bounded_body(response, MAX_AUTH_RESPONSE).await?);
        let token: TokenResponse =
            serde_json::from_slice(&data).map_err(|_| AppError::InvalidResponse)?;
        let value = Zeroizing::new(token.access_token);
        if value.is_empty() || token.expires_in == 0 {
            return Err(AppError::InvalidResponse);
        }
        self.verify_account(value.as_str()).await?;
        if let Some(refresh) = token.refresh_token {
            let refresh = Zeroizing::new(refresh);
            if refresh.is_empty() {
                return Err(AppError::InvalidResponse);
            }
            self.store
                .save(&self.config.identity, refresh.as_str())
                .await?;
        }
        let duration = token.expires_in.saturating_sub(60).clamp(1, 24 * 60 * 60);
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(duration),
        });
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, extract::Request, http::StatusCode, response::IntoResponse, routing::any};
    use serde_json::json;
    use tokio::net::TcpListener;

    use crate::demo::demo_config;

    struct MemoryStore {
        value: Mutex<Option<String>>,
        saves: AtomicUsize,
    }

    #[async_trait]
    impl SecretStore for MemoryStore {
        async fn load(&self, _identity: &Identity) -> Result<Zeroizing<String>> {
            self.value
                .lock()
                .await
                .clone()
                .map(Zeroizing::new)
                .ok_or(AppError::AuthenticationRequired)
        }

        async fn save(&self, _identity: &Identity, value: &str) -> Result<()> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            *self.value.lock().await = Some(value.to_owned());
            Ok(())
        }

        async fn clear(&self, _identity: &Identity) -> Result<()> {
            *self.value.lock().await = None;
            Ok(())
        }
    }

    async fn fake_auth(
        wrong_account: bool,
        pending_once: bool,
    ) -> (
        MicrosoftAuth,
        Arc<MemoryStore>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let token_calls = Arc::new(AtomicUsize::new(0));
        let profile_calls = Arc::new(AtomicUsize::new(0));
        let token_count = token_calls.clone();
        let profile_count = profile_calls.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let token_count = token_count.clone();
            let profile_count = profile_count.clone();
            async move {
                let path = request.uri().path();
                if path.ends_with("/devicecode") {
                    (StatusCode::OK, axum::Json(json!({
                        "device_code": "fictional-device-code", "user_code": "ABCD-EFGH",
                        "verification_uri": "https://microsoft.com/devicelogin", "expires_in": 20, "interval": 1
                    }))).into_response()
                } else if path.ends_with("/token") {
                    let call = token_count.fetch_add(1, Ordering::SeqCst);
                    if pending_once && call == 0 {
                        (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "authorization_pending"}))).into_response()
                    } else {
                        (StatusCode::OK, axum::Json(json!({
                            "access_token": "fictional-access-token", "refresh_token": "rotated-fictional-refresh-token", "expires_in": 3600
                        }))).into_response()
                    }
                } else if path == "/me" {
                    profile_count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(request.headers().get("authorization").unwrap(), "Bearer fictional-access-token");
                    let id = if wrong_account {
                        "00000000-0000-0000-0000-000000000099"
                    } else {
                        "00000000-0000-0000-0000-000000000004"
                    };
                    (StatusCode::OK, axum::Json(json!({"id": id}))).into_response()
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Arc::new(MemoryStore {
            value: Mutex::new(Some("fictional-refresh-token".into())),
            saves: AtomicUsize::new(0),
        });
        let auth = MicrosoftAuth {
            config: demo_config(),
            store: store.clone(),
            client: http_client().unwrap(),
            authority: Url::parse(&base).unwrap(),
            graph_me: Url::parse(&format!("{base}me")).unwrap(),
            cached: Mutex::new(None),
        };
        (auth, store, token_calls, profile_calls, task)
    }

    #[test]
    fn verification_link_and_code_are_strictly_validated() {
        assert!(safe_verification(
            "https://microsoft.com/devicelogin",
            "ABCD-EFGH"
        ));
        for uri in [
            "http://microsoft.com/devicelogin",
            "https://microsoft.com.evil.example/devicelogin",
            "https://name@microsoft.com/devicelogin",
            "https://microsoft.com:444/devicelogin",
            "https://microsoft.com/devicelogin#fragment",
        ] {
            assert!(!safe_verification(uri, "ABCD-EFGH"), "{uri}");
        }
        assert!(!safe_verification(
            "https://microsoft.com/devicelogin",
            "BAD\nCODE"
        ));
    }

    #[tokio::test]
    async fn refresh_is_serialized_and_bound_to_the_configured_account() {
        let (auth, store, token_calls, profile_calls, task) = fake_auth(false, false).await;
        let (first, second) = tokio::join!(auth.access_token(), auth.access_token());
        assert_eq!(first.unwrap().as_str(), "fictional-access-token");
        assert_eq!(second.unwrap().as_str(), "fictional-access-token");
        assert_eq!(token_calls.load(Ordering::SeqCst), 1);
        assert_eq!(profile_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.value.lock().await.as_deref(),
            Some("rotated-fictional-refresh-token")
        );
        task.abort();
    }

    #[tokio::test]
    async fn rejects_a_different_account_before_storing_or_returning_a_token() {
        let (auth, store, _, profile_calls, task) = fake_auth(true, false).await;
        assert!(matches!(
            auth.access_token().await,
            Err(AppError::AccountMismatch)
        ));
        assert_eq!(profile_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.saves.load(Ordering::SeqCst), 0);
        assert_eq!(
            store.value.lock().await.as_deref(),
            Some("fictional-refresh-token")
        );
        task.abort();
    }

    #[tokio::test]
    async fn device_login_waits_for_authorization_and_stores_only_after_identity_check() {
        let (auth, store, token_calls, profile_calls, task) = fake_auth(false, true).await;
        auth.login().await.unwrap();
        assert_eq!(token_calls.load(Ordering::SeqCst), 2);
        assert_eq!(profile_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.saves.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.value.lock().await.as_deref(),
            Some("rotated-fictional-refresh-token")
        );
        task.abort();
    }

    #[tokio::test]
    async fn logout_removes_the_stored_session_and_cached_access_token() {
        let (auth, store, _, _, task) = fake_auth(false, false).await;
        auth.access_token().await.unwrap();
        auth.logout().await.unwrap();
        assert!(store.value.lock().await.is_none());
        assert!(matches!(
            auth.access_token().await,
            Err(AppError::AuthenticationRequired)
        ));
        task.abort();
    }
}
