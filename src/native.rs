//! Native Teams adapter. Reads are available in every mode. Plain-text sends
//! exist only for the assisted confirmation flow.

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
    backend::{
        Author, Message, MessagePage, MessageSource, OpenChatMatch, OpenChatPage, SendReceipt,
        Target, ThreadPage, title_matches_query, untrusted_notice,
    },
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
    middle_tier: Option<Url>,
    expires_at: Instant,
}

pub struct NativeSource {
    auth: Arc<dyn NativeTokenProvider>,
    account_id: String,
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
    #[serde(default)]
    middle_tier: Option<String>,
}

#[derive(Deserialize)]
struct DirectoryResponse {
    conversations: Vec<DirectoryEntry>,
    #[serde(default, rename = "_metadata")]
    metadata: Option<DirectoryMetadata>,
}
#[derive(Deserialize)]
struct DirectoryMetadata {
    #[serde(rename = "backwardLink")]
    backward_link: Option<String>,
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
            auth: auth.clone(),
            account_id: auth.account_id().to_string(),
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
        let middle_tier = data
            .region_gtms
            .middle_tier
            .as_deref()
            .map(|base| self.parse_profile_base(base))
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
            middle_tier,
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

    fn parse_profile_base(&self, value: &str) -> Result<Url> {
        let base = Url::parse(value).map_err(|_| AppError::InvalidResponse)?;
        let valid = safe_profile_base(&base);
        #[cfg(test)]
        let valid = valid
            || self
                .fixture_origin
                .as_ref()
                .is_some_and(|origin| base.origin() == origin.origin());
        if !valid {
            eprintln!("Native Teams stage: unrecognized profile service URL.");
            return Err(AppError::InvalidResponse);
        }
        Ok(base)
    }

    async fn first_directory(
        &self,
        session: &NativeSession,
    ) -> Result<(DirectoryResponse, Url)> {
        let mut endpoint = self.directory.clone();
        endpoint
            .query_pairs_mut()
            .append_pair("view", "mychats")
            .append_pair("pageSize", "100");
        let mut response = self
            .client
            .get(endpoint.clone())
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
                endpoint = directory_url(base, &["api", "v2", "users", "ME", "conversations"])?;
                response = self.chat_directory_request(endpoint.clone(), session).await?;
            }
            if retry_directory(response.status()) {
                endpoint =
                    directory_url(&session.chat_base, &["v1", "users", "ME", "conversations"])?;
                response = self.chat_directory_request(endpoint.clone(), session).await?;
            }
        }
        if !response.status().is_success() {
            eprintln!(
                "Native Teams stage: chat discovery returned HTTP {}.",
                response.status().as_u16()
            );
            return Err(AppError::Upstream(response.status().as_u16()));
        }
        endpoint.set_query(None);
        Ok((read_directory(response).await?, endpoint))
    }

    pub async fn discover_chats(&self) -> Result<Vec<ChatCandidate>> {
        let session = self.session(true).await?;
        let (directory, _) = self.first_directory(&session).await?;
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

    async fn find_open_chats(&self, query: &str) -> Result<OpenChatPage> {
        let session = self.session(false).await?;
        let (first, endpoint) = self.first_directory(&session).await?;
        let mut backward = next_directory_link(&first);
        let mut entries = first.conversations;
        let mut pages = 1;
        let mut complete = backward.is_none();
        while let Some(cursor) = backward.take() {
            if pages == 3 {
                complete = false;
                break;
            }
            let Ok(next) = cursor_url(&endpoint, &cursor) else {
                complete = false;
                break;
            };
            let response = self.chat_directory_request(next, &session).await?;
            if !response.status().is_success() {
                return Err(AppError::Upstream(response.status().as_u16()));
            }
            let page = read_directory(response).await?;
            backward = next_directory_link(&page);
            entries.extend(page.conversations);
            pages += 1;
            complete = backward.is_none();
        }
        let mut peers = Vec::new();
        let mut rows = Vec::new();
        for entry in entries {
            let Some(id) = entry.id.filter(|id| {
                valid_id(id) && id.starts_with("19:") && !id.contains("@thread.tacv2")
            }) else {
                continue;
            };
            if direct_peer_oid(&id, &self.account_id).is_some() {
                if let Some(peer) = direct_peer_oid(&id, &self.account_id) {
                    peers.push(peer);
                }
                rows.push((id, None));
            } else if let Some(topic) = entry
                .thread_properties
                .and_then(|properties| properties.topic)
                .map(|topic| safe_display(&topic))
                .filter(|topic| !topic.is_empty())
            {
                rows.push((id, Some(topic)));
            }
        }
        peers.sort();
        peers.dedup();
        let names = self.peer_names(&session, &peers).await?;
        let mut matches = Vec::new();
        for (id, topic) in rows {
            let label = match direct_peer_oid(&id, &self.account_id) {
                Some(peer) => names.get(&peer).cloned(),
                None => topic,
            };
            let Some(label) = label else {
                continue;
            };
            if title_matches_query(&label, query) {
                matches.push(OpenChatMatch {
                    chat_id: id,
                    label,
                });
            }
        }
        Ok(OpenChatPage { matches, complete })
    }

    async fn peer_names(
        &self,
        session: &NativeSession,
        peers: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut names = std::collections::HashMap::new();
        let Some(base) = &session.middle_tier else {
            return Ok(names);
        };
        if peers.is_empty() {
            return Ok(names);
        }
        let url = profile_url(base)?;
        let bearer = self.auth.token(false).await?;
        for chunk in peers.chunks(100) {
            let mris: Vec<String> = chunk.iter().map(|oid| format!("8:orgid:{oid}")).collect();
            let response = self
                .client
                .post(url.clone())
                .bearer_auth(bearer.as_str())
                .json(&mris)
                .send()
                .await
                .map_err(|_| AppError::Network)?;
            if !response.status().is_success() {
                break;
            }
            let Ok(body) = bounded_body(response, MAX_RESPONSE).await else {
                break;
            };
            let Ok(profiles) = serde_json::from_slice::<ShortProfiles>(&body) else {
                break;
            };
            for profile in profiles.value {
                let Some(oid) = profile_oid(&profile) else {
                    continue;
                };
                if !chunk.iter().any(|peer| peer == &oid) {
                    continue;
                }
                let Some(name) = profile.display_name.as_deref() else {
                    continue;
                };
                let name = safe_display(name);
                if !name.is_empty() {
                    names.entry(oid).or_insert(name);
                }
            }
        }
        Ok(names)
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

fn next_directory_link(directory: &DirectoryResponse) -> Option<String> {
    directory
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.backward_link.clone())
        .filter(|link| !link.is_empty())
}

async fn read_directory(response: reqwest::Response) -> Result<DirectoryResponse> {
    let body = bounded_body(response, MAX_RESPONSE).await?;
    serde_json::from_slice(&body).map_err(|_| {
        eprintln!("Native Teams stage: chat directory response format.");
        AppError::InvalidResponse
    })
}

fn profile_url(base: &Url) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| AppError::InvalidResponse)?
        .pop_if_empty()
        .extend(["beta", "users", "fetchShortProfile"]);
    url.query_pairs_mut()
        .append_pair("isMailAddress", "false")
        .append_pair("enableGuest", "true")
        .append_pair("skypeTeamsInfo", "true");
    Ok(url)
}

fn safe_profile_base(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("");
    url.scheme() == "https"
        && (host == "teams.microsoft.com" || host.ends_with(".teams.microsoft.com"))
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().starts_with("/api/mt/")
}

fn is_guid(value: &str) -> bool {
    let mut parts = value.split('-');
    let widths = [8, 4, 4, 4, 12];
    widths.iter().all(|width| {
        parts.next().is_some_and(|part| {
            part.len() == *width && part.chars().all(|ch| ch.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

/// Peer object id encoded in a 1:1 thread id. Group and meeting ids do not match.
fn direct_peer_oid(chat_id: &str, owner: &str) -> Option<String> {
    let rest = chat_id.strip_prefix("19:")?;
    if rest.contains("@thread") || rest.to_ascii_lowercase().contains("meeting") {
        return None;
    }
    let (pair, _) = rest.split_once('@')?;
    let (left, right) = pair.split_once('_')?;
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    if !is_guid(&left) || !is_guid(&right) {
        return None;
    }
    let owner = owner.to_ascii_lowercase();
    if left == owner {
        Some(right)
    } else if right == owner {
        Some(left)
    } else {
        None
    }
}

#[derive(Deserialize)]
struct ShortProfiles {
    value: Vec<ShortProfile>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortProfile {
    display_name: Option<String>,
    object_id: Option<String>,
    mri: Option<String>,
}

fn profile_oid(profile: &ShortProfile) -> Option<String> {
    if let Some(id) = profile.object_id.as_deref() {
        let id = id.trim().to_ascii_lowercase();
        if is_guid(&id) {
            return Some(id);
        }
    }
    let mri = profile.mri.as_deref()?.trim().to_ascii_lowercase();
    let id = mri.strip_prefix("8:orgid:")?;
    is_guid(id).then(|| id.to_owned())
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

    async fn search_open_chats(&self, query: &str) -> Result<OpenChatPage> {
        self.find_open_chats(query).await
    }

    fn supports_send(&self, target: &Target) -> bool {
        matches!(target, Target::Chat { .. })
    }

    async fn send_text(
        &self,
        target: &Target,
        text: &str,
        client_message_id: &str,
    ) -> Result<SendReceipt> {
        let Target::Chat { chat_id } = target else {
            return Err(AppError::UnsupportedOperation);
        };
        let session = self.session(false).await?;
        let url = Self::messages_url(&session, chat_id)?;
        let response = self
            .client
            .post(url)
            .header(
                "Authentication",
                format!("skypetoken={}", session.token.as_str()),
            )
            .json(&serde_json::json!({
                "content": plain_teams_html(text),
                "messagetype": "RichText/Html",
                "contenttype": "text",
                "clientmessageid": client_message_id,
            }))
            .send()
            .await
            .map_err(|_| AppError::OutcomeUnknown)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(AppError::InvalidResponse);
        }
        if !status.is_success() {
            return Err(AppError::Upstream(status.as_u16()));
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(location_message_id);
        let body = bounded_body(response, 64 * 1024).await.unwrap_or_default();
        let message_id = location.or_else(|| {
            serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("id").and_then(|id| id.as_str()).map(str::to_owned))
                .filter(|id| valid_id(id))
        });
        Ok(SendReceipt {
            message_id,
            client_message_id: client_message_id.to_owned(),
        })
    }
}

fn plain_teams_html(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\n' => escaped.push_str("<br/>"),
            _ => escaped.push(ch),
        }
    }
    format!("<p>{escaped}</p>")
}

fn location_message_id(location: &str) -> Option<String> {
    let path = location.split('?').next().unwrap_or(location);
    let last = path.rsplit('/').next().unwrap_or("").trim();
    if !last.is_empty() && last.chars().all(|ch| ch.is_ascii_digit()) && valid_id(last) {
        Some(last.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::{MessagesRequest, ProposeReplyRequest, TeamsService},
        config::{Backend, Interaction},
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
    fn plain_text_replies_are_escaped_before_they_are_posted() {
        assert_eq!(
            plain_teams_html("Use <tag> & \"quotes\"\nnext"),
            "<p>Use &lt;tag&gt; &amp; \"quotes\"<br/>next</p>"
        );
        assert_eq!(location_message_id("/v1/messages/1793400000001"), Some("1793400000001".into()));
        assert_eq!(location_message_id("/v1/messages/not-numeric"), None);
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
    async fn setup_reads_any_chat_and_replies_stay_on_the_allowlist() {
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
            account_id: "aaaaaaaa-0000-0000-0000-000000000001".into(),
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
        let service = TeamsService::new(config, source.clone());
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
        let outside = service
            .messages(MessagesRequest {
                target: Target::Chat {
                    chat_id: "19:other-chat@thread.v2".into(),
                },
                limit: Some(10),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(outside.messages.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let mut assisted = demo_config();
        assisted.backend = Backend::NativeTeams;
        assisted.interaction = Interaction::Assisted;
        let reply_service = TeamsService::new(assisted, source);
        let denied = reply_service
            .prepare_reply(ProposeReplyRequest {
                target: Target::Chat {
                    chat_id: "19:other-chat@thread.v2".into(),
                },
                text: "A fictional reply that must not be sent.".into(),
                evidence: Vec::new(),
            })
            .await;
        assert!(matches!(denied, Err(AppError::Forbidden)));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        task.abort();
    }

    #[test]
    fn direct_peer_comes_only_from_the_one_to_one_id() {
        let owner = "aaaaaaaa-0000-0000-0000-000000000001";
        let peer = "bbbbbbbb-0000-0000-0000-000000000002";
        let chat = format!("19:{owner}_{peer}@unq.gbl.spaces");
        assert_eq!(direct_peer_oid(&chat, owner).as_deref(), Some(peer));
        assert_eq!(
            direct_peer_oid("19:fictional-chat@thread.v2", owner),
            None
        );
        assert!(safe_profile_base(
            &Url::parse("https://teams.microsoft.com/api/mt/part/amer-03").unwrap()
        ));
        assert!(!safe_profile_base(
            &Url::parse("https://teams.microsoft.com/api/csa").unwrap()
        ));
    }

    #[tokio::test]
    async fn open_chat_search_uses_titles_and_does_not_read_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let fixture_base = base.clone();
        let owner = "aaaaaaaa-0000-0000-0000-000000000001";
        let peer = "bbbbbbbb-0000-0000-0000-000000000002";
        let direct = format!("19:{owner}_{peer}@unq.gbl.spaces");
        let app = Router::new().fallback(any(move |request: Request| {
            let base = fixture_base.clone();
            let direct = direct.clone();
            async move {
                match (request.method(), request.uri().path()) {
                    (&Method::POST, "/authz") => axum::Json(json!({
                        "tokens": {"skypeToken": "fixture-skype-token", "expiresIn": 3600},
                        "regionGtms": {"chatService": base, "middleTier": base}
                    }))
                    .into_response(),
                    (&Method::GET, "/directory") => StatusCode::UNAUTHORIZED.into_response(),
                    (&Method::GET, "/v1/users/ME/conversations") => axum::Json(json!({
                        "conversations": [
                            {"id": direct},
                            {"id": "19:allan@thread.v2", "threadProperties": {"topic": "Allan and José"}},
                            {"id": "19:fixture@thread.v2", "threadProperties": {"topic": "Fixture group"}}
                        ]
                    }))
                    .into_response(),
                    (&Method::POST, "/beta/users/fetchShortProfile") => {
                        assert_eq!(
                            request.headers()["authorization"],
                            "Bearer fixture-teams-token"
                        );
                        axum::Json(json!({
                            "value": [
                                {"objectId": "bbbbbbbb-0000-0000-0000-000000000002", "displayName": "José Almeida"},
                                {"objectId": "cccccccc-0000-0000-0000-000000000003", "displayName": "Wrong person"}
                            ]
                        }))
                        .into_response()
                    }
                    (&Method::GET, path) if path.ends_with("/messages") => {
                        panic!("open chat search must not read messages");
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = Url::parse(&base).unwrap();
        let source = NativeSource {
            auth: Arc::new(FixtureAuth),
            account_id: owner.into(),
            client: http_client().unwrap(),
            authz: base.join("authz").unwrap(),
            directory: base.join("directory").unwrap(),
            fixture_origin: Some(base),
            session: Mutex::new(None),
        };
        let jose = source.find_open_chats("josé").await.unwrap();
        assert!(jose.complete);
        assert_eq!(jose.matches.len(), 2);
        assert_eq!(jose.matches[0].label, "José Almeida");
        assert_eq!(jose.matches[1].label, "Allan and José");
        let one = source.find_open_chats("almeida").await.unwrap();
        assert_eq!(one.matches.len(), 1);
        assert!(one.matches[0].chat_id.contains(peer));
        let group = source.find_open_chats("fixture").await.unwrap();
        assert_eq!(group.matches.len(), 1);
        assert_eq!(group.matches[0].label, "Fixture group");
        task.abort();
    }
}
