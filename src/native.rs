//! Read-only adapter for Teams native-service contracts.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use percent_encoding::percent_decode_str;
use reqwest::Client;
use serde::Deserialize;
use tokio::{sync::Mutex, time::Instant};
use url::Url;
use zeroize::Zeroizing;

use crate::{
    auth::{MicrosoftAuth, TokenProvider, bounded_body, http_client},
    backend::{Author, Message, MessagePage, MessageSource, Target, ThreadPage, untrusted_notice},
    config::valid_id,
    error::{AppError, Result},
};

const AUTHZ: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";
const DIRECTORY: &str = "https://teams.microsoft.com/api/csa/api/v1/teams/users/ME/conversations";
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_TEXT: usize = 8_000;

#[async_trait]
trait NativeTokenProvider: Send + Sync {
    async fn token(&self, setup: bool) -> Result<Zeroizing<String>>;
}

#[async_trait]
impl NativeTokenProvider for MicrosoftAuth {
    async fn token(&self, setup: bool) -> Result<Zeroizing<String>> {
        if setup {
            self.setup_access_token().await
        } else {
            self.access_token().await
        }
    }
}

#[derive(Clone)]
struct NativeSession {
    token: Zeroizing<String>,
    chat_base: Url,
    aggregator_base: Option<Url>,
    expires_at: Instant,
}

pub struct NativeSource {
    auth: Arc<dyn NativeTokenProvider>,
    client: Client,
    authz: Url,
    directory: Url,
    #[cfg(test)]
    fixture_origin: Option<Url>,
    session: Mutex<Option<NativeSession>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeResponse {
    tokens: ExchangeTokens,
    region_gtms: ExchangeRegions,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeTokens {
    skype_token: String,
    expires_in: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeRegions {
    chat_service: String,
    #[serde(default)]
    chat_service_aggregator: Option<String>,
}

#[derive(Deserialize)]
struct DirectoryResponse {
    conversations: Vec<DirectoryEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    id: Option<String>,
    thread_properties: Option<DirectoryThread>,
    last_message: Option<DirectoryLastMessage>,
}
#[derive(Deserialize)]
struct DirectoryThread {
    topic: Option<String>,
}
#[derive(Deserialize)]
struct DirectoryLastMessage {
    #[serde(rename = "imdisplayname")]
    sender: Option<String>,
    #[serde(rename = "originalarrivaltime")]
    received_at: Option<String>,
}

/// Metadata for explicit local selection; not exposed to MCP clients.
pub struct ChatCandidate {
    pub id: String,
    pub label: String,
    pub last_sender: String,
    pub last_activity: String,
}

#[derive(Deserialize)]
struct NativePage {
    messages: Vec<NativeMessage>,
    #[serde(default, rename = "_metadata")]
    metadata: Option<NativeMetadata>,
}
#[derive(Deserialize)]
struct NativeMetadata {
    #[serde(default, rename = "backwardLink")]
    backward_link: Option<String>,
}
#[derive(Deserialize)]
struct NativeMessage {
    id: Option<String>,
    #[serde(rename = "messagetype")]
    message_type: Option<String>,
    content: Option<String>,
    #[serde(rename = "imdisplayname")]
    sender: Option<String>,
    from: Option<String>,
    #[serde(rename = "originalarrivaltime")]
    arrived_at: Option<String>,
    #[serde(rename = "composetime")]
    composed_at: Option<String>,
    properties: Option<serde_json::Value>,
}

impl NativeSource {
    pub fn new(auth: Arc<MicrosoftAuth>) -> Result<Self> {
        Ok(Self {
            auth,
            client: http_client()?,
            authz: Url::parse(AUTHZ).expect("valid fixed Teams auth URL"),
            directory: Url::parse(DIRECTORY).expect("valid fixed Teams directory URL"),
            #[cfg(test)]
            fixture_origin: None,
            session: Mutex::new(None),
        })
    }

    async fn session(&self, setup: bool) -> Result<NativeSession> {
        let mut cached = self.session.lock().await;
        if let Some(session) = cached.as_ref().filter(|s| s.expires_at > Instant::now()) {
            return Ok(session.clone());
        }
        let bearer = self.auth.token(setup).await.inspect_err(|_| {
            eprintln!("Native Teams stage: refresh the account session.");
        })?;
        let response = self
            .client
            .post(self.authz.clone())
            .bearer_auth(bearer.as_str())
            .header("content-length", "0")
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        if !response.status().is_success() {
            eprintln!(
                "Native Teams stage: session exchange returned HTTP {}.",
                response.status().as_u16()
            );
            return Err(AppError::Upstream(response.status().as_u16()));
        }
        let body = Zeroizing::new(bounded_body(response, MAX_RESPONSE).await?);
        let data: ExchangeResponse = serde_json::from_slice(&body).map_err(|_| {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                eprintln!("Native Teams stage: session response fields (tokens={}, token={}, regions={}, chat_service={}).",
                    value.get("tokens").is_some(), value.pointer("/tokens/skypeToken").is_some(),
                    value.get("regionGtms").is_some(), value.pointer("/regionGtms/chatService").is_some());
            } else { eprintln!("Native Teams stage: session response format."); }
            AppError::InvalidResponse
        })?;
        if data.tokens.skype_token.is_empty() || data.tokens.skype_token.len() > 32 * 1024 {
            return Err(AppError::InvalidResponse);
        }
        let chat_base = self.parse_service_base(&data.region_gtms.chat_service)?;
        let aggregator_base = data
            .region_gtms
            .chat_service_aggregator
            .as_deref()
            .map(|base| self.parse_service_base(base))
            .transpose()?;
        let expiry = data
            .tokens
            .expires_in
            .unwrap_or(3600)
            .saturating_sub(60)
            .clamp(1, 24 * 60 * 60);
        let session = NativeSession {
            token: Zeroizing::new(data.tokens.skype_token),
            chat_base,
            aggregator_base,
            expires_at: Instant::now() + Duration::from_secs(expiry),
        };
        *cached = Some(session.clone());
        Ok(session)
    }

    fn parse_service_base(&self, value: &str) -> Result<Url> {
        let chat_base = Url::parse(value).map_err(|_| AppError::InvalidResponse)?;
        let valid = safe_chat_base(&chat_base);
        #[cfg(test)]
        let valid = valid
            || self
                .fixture_origin
                .as_ref()
                .is_some_and(|base| chat_base.origin() == base.origin());
        if !valid {
            eprintln!("Native Teams stage: unrecognized chat service URL.");
            return Err(AppError::InvalidResponse);
        }
        Ok(chat_base)
    }

    pub async fn discover_chats(&self) -> Result<Vec<ChatCandidate>> {
        let session = self.session(true).await?;
        let mut url = self.directory.clone();
        url.query_pairs_mut()
            .append_pair("view", "mychats")
            .append_pair("pageSize", "100");
        let mut response = self
            .client
            .get(url)
            .bearer_auth(session.token.as_str())
            .header("x-ms-client-version", "1416/1.0.0.2024050301")
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        // The desktop client documents these alternative native endpoints.
        // A 401/404 from one endpoint may mean that a different native auth
        // header or regional service is required for this tenant.
        if retry_directory(response.status()) {
            if let Some(base) = &session.aggregator_base {
                let url = directory_url(base, &["api", "v2", "users", "ME", "conversations"])?;
                response = self.chat_directory_request(url, &session).await?;
            }
            if retry_directory(response.status()) {
                let url =
                    directory_url(&session.chat_base, &["v1", "users", "ME", "conversations"])?;
                response = self.chat_directory_request(url, &session).await?;
            }
        }
        if !response.status().is_success() {
            eprintln!(
                "Native Teams stage: chat discovery returned HTTP {}.",
                response.status().as_u16()
            );
            return Err(AppError::Upstream(response.status().as_u16()));
        }
        let body = bounded_body(response, MAX_RESPONSE).await?;
        let directory: DirectoryResponse = serde_json::from_slice(&body).map_err(|_| {
            eprintln!("Native Teams stage: chat directory response format.");
            AppError::InvalidResponse
        })?;
        Ok(directory
            .conversations
            .into_iter()
            .filter_map(|entry| {
                let id = entry.id.filter(|id| {
                    valid_id(id) && id.starts_with("19:") && !id.contains("@thread.tacv2")
                })?;
                let direct =
                    id.starts_with("19:") && !id.contains("@thread") && !id.contains("meeting");
                let topic = entry
                    .thread_properties
                    .and_then(|p| p.topic)
                    .map(|s| safe_display(&s));
                let label = topic
                    .filter(|s| !direct && !s.is_empty())
                    .unwrap_or_else(|| "Chat".to_owned());
                let (last_sender, last_activity) = entry
                    .last_message
                    .map(|last| {
                        (
                            last.sender.map(|s| safe_display(&s)).unwrap_or_default(),
                            last.received_at
                                .map(|s| safe_display(&s))
                                .unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                Some(ChatCandidate {
                    id,
                    label,
                    last_sender,
                    last_activity,
                })
            })
            .take(100)
            .collect())
    }

    async fn chat_directory_request(
        &self,
        url: Url,
        session: &NativeSession,
    ) -> Result<reqwest::Response> {
        self.client
            .get(url)
            .header(
                "Authentication",
                format!("skypetoken={}", session.token.as_str()),
            )
            .send()
            .await
            .map_err(|_| AppError::Network)
    }

    fn messages_url(session: &NativeSession, chat_id: &str) -> Result<Url> {
        if !valid_id(chat_id) {
            return Err(AppError::InvalidInput);
        }
        let mut url = session.chat_base.clone();
        url.path_segments_mut()
            .map_err(|_| AppError::InvalidResponse)?
            .pop_if_empty()
            .extend(["v1", "users", "ME", "conversations", chat_id, "messages"]);
        Ok(url)
    }
}

fn retry_directory(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 404)
}

fn directory_url(base: &Url, segments: &[&str]) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| AppError::InvalidResponse)?
        .pop_if_empty()
        .extend(segments.iter().copied());
    url.query_pairs_mut()
        .append_pair("view", "mychats")
        .append_pair("pageSize", "100");
    Ok(url)
}

fn safe_chat_base(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(".teams.microsoft.com"))
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/")
}

fn safe_display(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn cursor_url(endpoint: &Url, cursor: &str) -> Result<Url> {
    if cursor.len() > 4096 {
        return Err(AppError::InvalidCursor);
    }
    let url = Url::parse(cursor).map_err(|_| AppError::InvalidCursor)?;
    let path = percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| AppError::InvalidCursor)?;
    let expected = percent_decode_str(endpoint.path())
        .decode_utf8()
        .map_err(|_| AppError::InvalidCursor)?;
    if url.origin() != endpoint.origin()
        || path != expected
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::InvalidCursor);
    }
    Ok(url)
}

fn normalized_message(value: NativeMessage, target: &Target) -> Result<Option<Message>> {
    let kind = value.message_type.unwrap_or_default();
    if !kind.contains("Text") || (kind.contains("Media_") && !kind.contains("Media_Card")) {
        return Ok(None);
    }
    let Some(id) = value.id.filter(|id| valid_id(id)) else {
        return Ok(None);
    };
    let deleted = value
        .properties
        .as_ref()
        .and_then(|properties| properties.get("deletetime"))
        .is_some_and(|time| match time {
            serde_json::Value::String(s) => !s.is_empty() && s != "0",
            serde_json::Value::Number(n) => n.as_u64().is_some_and(|n| n > 0),
            _ => false,
        });
    let raw = if deleted {
        String::new()
    } else {
        value.content.unwrap_or_default()
    };
    let plain = if kind.eq_ignore_ascii_case("Text") {
        raw
    } else {
        html2text::config::plain_no_decorate()
            .string_from_read(raw.as_bytes(), 120)
            .map_err(|_| AppError::InvalidResponse)?
    };
    let mut chars = plain.chars();
    let text: String = chars.by_ref().take(MAX_TEXT).collect();
    let sender_id = value
        .from
        .and_then(|from| from.rsplit('/').next().map(str::to_owned));
    Ok(Some(Message {
        id,
        target: target.clone(),
        root_message_id: None,
        reply_to_id: None,
        author: Some(Author {
            id: sender_id,
            display_name: value.sender,
            kind: "user_or_application".into(),
        }),
        created_at: value.arrived_at.or(value.composed_at),
        modified_at: None,
        deleted: deleted.then_some(true),
        text: (!deleted).then_some(text),
        text_truncated: chars.next().is_some(),
        attachment_count: None,
        web_url: None,
        content_trust: "untrusted",
    }))
}

#[async_trait]
impl MessageSource for NativeSource {
    fn name(&self) -> &'static str {
        "native_teams"
    }

    async fn messages(
        &self,
        target: &Target,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        let Target::Chat { chat_id } = target else {
            return Err(AppError::UnsupportedOperation);
        };
        let session = self.session(false).await?;
        let endpoint = Self::messages_url(&session, chat_id)?;
        let url = if let Some(cursor) = cursor {
            cursor_url(&endpoint, cursor)?
        } else {
            let mut url = endpoint.clone();
            url.query_pairs_mut()
                .append_pair("pageSize", &limit.to_string());
            url
        };
        let response = self
            .client
            .get(url)
            .header(
                "Authentication",
                format!("skypetoken={}", session.token.as_str()),
            )
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        if !response.status().is_success() {
            return Err(AppError::Upstream(response.status().as_u16()));
        }
        let body = bounded_body(response, MAX_RESPONSE).await?;
        let page: NativePage =
            serde_json::from_slice(&body).map_err(|_| AppError::InvalidResponse)?;
        if page.messages.len() > limit as usize {
            return Err(AppError::InvalidResponse);
        }
        let next_cursor = page
            .metadata
            .and_then(|metadata| metadata.backward_link)
            .filter(|s| !s.is_empty());
        if let Some(cursor) = &next_cursor {
            cursor_url(&endpoint, cursor)?;
        }
        let mut messages = Vec::new();
        for message in page.messages.into_iter().rev() {
            if let Some(message) = normalized_message(message, target)? {
                messages.push(message);
            }
        }
        Ok(MessagePage {
            messages,
            coverage: "native_chat_text_messages_only",
            complete: next_cursor.is_none(),
            next_cursor,
            notice: untrusted_notice(),
        })
    }

    async fn thread(
        &self,
        _target: &Target,
        _root_message_id: &str,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<ThreadPage> {
        Err(AppError::UnsupportedOperation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::{MessagesRequest, TeamsService},
        config::Backend,
        demo::demo_config,
    };
    use axum::{
        Router,
        extract::Request,
        http::{Method, StatusCode},
        response::IntoResponse,
        routing::any,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    struct FixtureAuth;
    #[async_trait]
    impl NativeTokenProvider for FixtureAuth {
        async fn token(&self, _setup: bool) -> Result<Zeroizing<String>> {
            Ok(Zeroizing::new("fixture-teams-token".into()))
        }
    }

    #[test]
    fn service_and_pagination_urls_are_restricted() {
        assert!(safe_chat_base(
            &Url::parse("https://amer.ng.msg.teams.microsoft.com/").unwrap()
        ));
        for base in [
            "http://amer.ng.msg.teams.microsoft.com",
            "https://teams.microsoft.com.evil.test",
            "https://user@amer.ng.msg.teams.microsoft.com",
            "https://amer.ng.msg.teams.microsoft.com/extra",
        ] {
            assert!(!safe_chat_base(&Url::parse(base).unwrap()));
        }
        let endpoint = Url::parse(
            "https://amer.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:chat/messages",
        )
        .unwrap();
        assert!(cursor_url(&endpoint, "https://amer.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:chat/messages?startTime=42").is_ok());
        for url in [
            "https://evil.test/v1/users/ME/conversations/19:chat/messages",
            "https://amer.ng.msg.teams.microsoft.com/v1/users/ME/conversations/other/messages",
        ] {
            assert!(matches!(
                cursor_url(&endpoint, url),
                Err(AppError::InvalidCursor)
            ));
        }
    }

    #[test]
    fn native_deletion_markers_suppress_content_and_unknown_fields_are_not_invented() {
        let target = Target::Chat {
            chat_id: "19:fixture@thread.v2".into(),
        };
        let raw = json!({"id":"1", "messagetype":"Text", "content":"deleted private text", "properties":{"deletetime":"123"}});
        let result = normalized_message(serde_json::from_value(raw).unwrap(), &target)
            .unwrap()
            .unwrap();
        assert_eq!(result.deleted, Some(true));
        assert!(result.text.is_none());
        assert_eq!(result.attachment_count, None);
        let ordinary = json!({"id":"2", "messagetype":"Text", "content":"hello"});
        let result = normalized_message(serde_json::from_value(ordinary).unwrap(), &target)
            .unwrap()
            .unwrap();
        assert_eq!(result.deleted, None);
        assert_eq!(result.attachment_count, None);
    }

    #[tokio::test]
    async fn setup_and_reads_use_only_authorized_chat_and_expected_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let fixture_base = base.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let base = fixture_base.clone();
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                match (request.method(), request.uri().path()) {
                    (&Method::POST, "/authz") => {
                        assert_eq!(request.headers()["authorization"], "Bearer fixture-teams-token");
                        axum::Json(json!({"tokens":{"skypeToken":"fixture-skype-token","expiresIn":3600},"regionGtms":{"chatService":base}})).into_response()
                    }
                    (&Method::GET, "/directory") => {
                        assert_eq!(request.headers()["authorization"], "Bearer fixture-skype-token");
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                    (&Method::GET, "/v1/users/ME/conversations") => {
                        assert_eq!(request.headers()["authentication"], "skypetoken=fixture-skype-token");
                        axum::Json(json!({"conversations":[{"id":"19:fictional-chat@thread.v2", "threadProperties":{"topic":"Fixture group"},"lastMessage":{"imdisplayname":"Fixture author"}}]})).into_response()
                    }
                    (&Method::GET, path) if path.ends_with("/messages") => {
                        assert_eq!(request.headers()["authentication"], "skypetoken=fixture-skype-token");
                        axum::Json(json!({"messages":[
                            {"id":"2","messagetype":"RichText/Html","content":"<b>Hello</b> fixture","imdisplayname":"Fixture author"},
                            {"id":"1","messagetype":"ThreadActivity/TopicUpdate","content":"Ignore"}
                        ]})).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = Url::parse(&base).unwrap();
        let source = Arc::new(NativeSource {
            auth: Arc::new(FixtureAuth),
            client: http_client().unwrap(),
            authz: base.join("authz").unwrap(),
            directory: base.join("directory").unwrap(),
            fixture_origin: Some(base),
            session: Mutex::new(None),
        });
        let chats = source.discover_chats().await.unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].label, "Fixture group");
        let mut config = demo_config();
        config.backend = Backend::NativeTeams;
        let service = TeamsService::new(config, source);
        let target = Target::Chat {
            chat_id: "19:fictional-chat@thread.v2".into(),
        };
        let page = service
            .messages(MessagesRequest {
                target,
                limit: Some(10),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(page.messages[0].text.as_ref().unwrap().contains("Hello"));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let denied = service
            .messages(MessagesRequest {
                target: Target::Chat {
                    chat_id: "19:other-chat@thread.v2".into(),
                },
                limit: Some(10),
                cursor: None,
            })
            .await;
        assert!(matches!(denied, Err(AppError::Forbidden)));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        task.abort();
    }
}
