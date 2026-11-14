use std::sync::Arc;

use async_trait::async_trait;
use percent_encoding::percent_decode_str;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::{
    auth::{TokenProvider, bounded_body, http_client},
    backend::{
        Author, Message, MessagePage, MessageSource, SendReceipt, Target, ThreadPage,
        message_coverage, record_image, untrusted_notice,
    },
    config::valid_id,
    error::{AppError, Result},
    images::{self, ImageBudget},
};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0/";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 8_000;

pub struct GraphSource {
    client: Client,
    token_provider: Arc<dyn TokenProvider>,
    base: Url,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMessage {
    id: String,
    #[serde(default)]
    reply_to_id: Option<String>,
    #[serde(default)]
    from: Option<GraphFrom>,
    #[serde(default)]
    created_date_time: Option<String>,
    #[serde(default)]
    last_modified_date_time: Option<String>,
    #[serde(default)]
    deleted_date_time: Option<String>,
    #[serde(default)]
    body: Option<GraphBody>,
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    web_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphFrom {
    #[serde(default)]
    user: Option<GraphIdentity>,
    #[serde(default)]
    application: Option<GraphIdentity>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphIdentity {
    id: Option<String>,
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphBody {
    content_type: String,
    content: String,
}

#[derive(Deserialize)]
struct GraphPage {
    value: Vec<GraphMessage>,
    #[serde(default, rename = "@odata.nextLink")]
    next_link: Option<String>,
}

impl GraphSource {
    pub fn new(token_provider: Arc<dyn TokenProvider>) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            token_provider,
            base: Url::parse(GRAPH_BASE).expect("valid fixed Graph base"),
        })
    }

    fn messages_url(&self, target: &Target) -> Result<Url> {
        let mut url = self.base.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AppError::InvalidInput)?;
        segments.pop_if_empty();
        match target {
            Target::Chat { chat_id } => {
                segments.push("chats").push(chat_id).push("messages");
            }
            Target::Channel {
                team_id,
                channel_id,
            } => {
                segments
                    .push("teams")
                    .push(team_id)
                    .push("channels")
                    .push(channel_id)
                    .push("messages");
            }
        }
        drop(segments);
        Ok(url)
    }

    async fn fetch_hosted(
        &self,
        token: &str,
        target: &Target,
        message_id: &str,
        hosted_id: &str,
    ) -> std::result::Result<(String, Vec<u8>), &'static str> {
        if !images::valid_hosted_id(hosted_id) || !valid_id(message_id) {
            return Err("unavailable");
        }
        let mut url = self.messages_url(target).map_err(|_| "unavailable")?;
        url.path_segments_mut()
            .map_err(|_| "unavailable")?
            .push(message_id)
            .push("hostedContents")
            .push(hosted_id)
            .push("$value");
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| "unavailable")?;
        images::read_image_response(response).await
    }

    fn thread_urls(&self, target: &Target, root_id: &str) -> Result<(Url, Url)> {
        if !matches!(target, Target::Channel { .. }) {
            return Err(AppError::InvalidInput);
        }
        let mut root = self.messages_url(target)?;
        root.path_segments_mut()
            .map_err(|_| AppError::InvalidInput)?
            .push(root_id);
        let mut replies = root.clone();
        replies
            .path_segments_mut()
            .map_err(|_| AppError::InvalidInput)?
            .push("replies");
        Ok((root, replies))
    }

    fn page_url(&self, endpoint: &Url, cursor: Option<&str>, limit: u32) -> Result<Url> {
        if let Some(cursor) = cursor {
            return validate_cursor(endpoint, cursor);
        }
        let mut url = endpoint.clone();
        url.query_pairs_mut()
            .append_pair("$top", &limit.to_string());
        Ok(url)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &Url) -> Result<T> {
        let token = self.token_provider.access_token().await?;
        let response = self
            .client
            .get(url.clone())
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(|_| AppError::Network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Upstream(status.as_u16()));
        }
        let body = bounded_body(response, MAX_RESPONSE_BYTES).await?;
        serde_json::from_slice(&body).map_err(|_| AppError::InvalidResponse)
    }

    fn normalize_page(
        &self,
        endpoint: &Url,
        target: &Target,
        page: GraphPage,
        limit: u32,
        thread_root: Option<&str>,
    ) -> Result<MessagePage> {
        if page.value.len() > limit as usize {
            return Err(AppError::InvalidResponse);
        }
        if let Some(next) = &page.next_link {
            validate_cursor(endpoint, next)?;
        }
        let messages = page
            .value
            .into_iter()
            .map(|message| normalize(message, target, thread_root))
            .collect::<Result<Vec<_>>>()?;
        Ok(MessagePage {
            messages,
            coverage: message_coverage(target),
            complete: page.next_link.is_none(),
            next_cursor: page.next_link,
            notice: untrusted_notice(),
        })
    }
}

/// Only the exact resource endpoint and origin may receive a bearer token.
fn validate_cursor(endpoint: &Url, cursor: &str) -> Result<Url> {
    if cursor.is_empty() || cursor.len() > 4096 {
        return Err(AppError::InvalidCursor);
    }
    let url = Url::parse(cursor).map_err(|_| AppError::InvalidCursor)?;
    if url.origin() != endpoint.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || decoded_segments(&url)? != decoded_segments(endpoint)?
    {
        return Err(AppError::InvalidCursor);
    }
    Ok(url)
}

fn decoded_segments(url: &Url) -> Result<Vec<String>> {
    url.path_segments()
        .ok_or(AppError::InvalidCursor)?
        .map(|segment| {
            let segment = percent_decode_str(segment)
                .decode_utf8()
                .map_err(|_| AppError::InvalidCursor)?;
            if segment.contains('/')
                || segment.contains('\\')
                || segment.chars().any(char::is_control)
            {
                return Err(AppError::InvalidCursor);
            }
            Ok(segment.into_owned())
        })
        .collect()
}

fn normalize(value: GraphMessage, target: &Target, thread_root: Option<&str>) -> Result<Message> {
    if value.id.is_empty() {
        return Err(AppError::InvalidResponse);
    }
    let deleted = value.deleted_date_time.is_some();
    let hosted_content_ids = if deleted {
        Vec::new()
    } else {
        value
            .body
            .as_ref()
            .filter(|body| body.content_type.eq_ignore_ascii_case("html"))
            .map(|body| crate::images::hosted_content_ids(&body.content))
            .unwrap_or_default()
    };
    let (text, text_truncated) = if deleted {
        (None, false)
    } else if let Some(body) = value.body {
        let content = match body.content_type.to_ascii_lowercase().as_str() {
            "text" => body.content,
            "html" => html2text::config::plain_no_decorate()
                .string_from_read(body.content.as_bytes(), 120)
                .map_err(|_| AppError::InvalidResponse)?,
            _ => return Err(AppError::InvalidResponse),
        };
        let mut chars = content.chars();
        let text: String = chars.by_ref().take(MAX_TEXT_CHARS).collect();
        (Some(text), chars.next().is_some())
    } else {
        (None, false)
    };
    let author = value
        .from
        .and_then(|from| {
            from.user.map(|user| (user, "user")).or_else(|| {
                from.application
                    .map(|application| (application, "application"))
            })
        })
        .map(|(identity, kind)| Author {
            id: identity.id,
            display_name: identity.display_name,
            kind: kind.into(),
        });
    Ok(Message {
        id: value.id,
        target: target.clone(),
        root_message_id: thread_root.map(str::to_owned),
        reply_to_id: value.reply_to_id,
        author,
        created_at: value.created_date_time,
        modified_at: value.last_modified_date_time,
        deleted: Some(deleted),
        text,
        text_truncated,
        attachment_count: Some(value.attachments.map_or(0, |attachments| attachments.len())),
        images: Vec::new(),
        web_url: value.web_url,
        content_trust: "untrusted",
        image_bytes: Vec::new(),
        pending_image_urls: Vec::new(),
        hosted_content_ids,
    })
}

#[async_trait]
impl MessageSource for GraphSource {
    fn name(&self) -> &'static str {
        "microsoft_graph"
    }

    async fn messages(
        &self,
        target: &Target,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        let endpoint = self.messages_url(target)?;
        let url = self.page_url(&endpoint, cursor, limit)?;
        let page = self.get::<GraphPage>(&url).await?;
        self.normalize_page(&endpoint, target, page, limit, None)
    }

    async fn thread(
        &self,
        target: &Target,
        root_id: &str,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<ThreadPage> {
        let (root_endpoint, reply_endpoint) = self.thread_urls(target, root_id)?;
        let reply_url = self.page_url(&reply_endpoint, cursor, limit)?;
        let root = normalize(
            self.get::<GraphMessage>(&root_endpoint).await?,
            target,
            None,
        )?;
        if root.id != root_id {
            return Err(AppError::InvalidResponse);
        }
        let replies = self.normalize_page(
            &reply_endpoint,
            target,
            self.get::<GraphPage>(&reply_url).await?,
            limit,
            Some(root_id),
        )?;
        Ok(ThreadPage {
            root,
            replies: replies.messages,
            next_cursor: replies.next_cursor,
            complete: replies.complete,
            notice: replies.notice,
        })
    }

    async fn load_images(&self, messages: &mut [Message]) {
        if messages
            .iter()
            .all(|message| message.hosted_content_ids.is_empty())
        {
            return;
        }
        let Ok(token) = self.token_provider.access_token().await else {
            for message in messages.iter_mut() {
                for _ in std::mem::take(&mut message.hosted_content_ids) {
                    record_image(message, Err("unavailable"));
                }
            }
            return;
        };
        let mut budget = ImageBudget::default();
        for message in messages.iter_mut() {
            let target = message.target.clone();
            let message_id = message.id.clone();
            for hosted_id in std::mem::take(&mut message.hosted_content_ids) {
                if !budget.allow() {
                    record_image(message, Err("response_budget"));
                    continue;
                }
                budget.note_attempt();
                let outcome = self
                    .fetch_hosted(token.as_str(), &target, &message_id, &hosted_id)
                    .await;
                if outcome.is_ok() {
                    budget.note_attached();
                }
                record_image(message, outcome);
            }
        }
    }

    fn supports_send(&self, _target: &Target) -> bool {
        true
    }

    async fn send_text(
        &self,
        target: &Target,
        text: &str,
        client_message_id: &str,
    ) -> Result<SendReceipt> {
        let url = self.messages_url(target)?;
        let token = self.token_provider.access_token().await?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "body": { "contentType": "text", "content": text }
            }))
            .send()
            .await
            .map_err(|_| AppError::OutcomeUnknown)?;
        accepted_receipt(response, client_message_id).await
    }
}

async fn accepted_receipt(response: reqwest::Response, client_message_id: &str) -> Result<SendReceipt> {
    let status = response.status();
    if status.is_redirection() {
        return Err(AppError::InvalidResponse);
    }
    if !status.is_success() {
        return Err(AppError::Upstream(status.as_u16()));
    }
    let body = bounded_body(response, 64 * 1024).await.unwrap_or_default();
    let message_id = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("id").and_then(|id| id.as_str()).map(str::to_owned))
        .filter(|id| crate::config::valid_id(id));
    Ok(SendReceipt {
        message_id,
        client_message_id: client_message_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use serde_json::json;
    use tokio::net::TcpListener;
    use zeroize::Zeroizing;

    use crate::{
        backend::{
            MessagesRequest, ProposeReplyRequest, SearchRequest, TeamsService, ThreadRequest,
        },
        config::Interaction,
        demo::demo_config,
    };

    struct FakeToken {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TokenProvider for FakeToken {
        async fn access_token(&self) -> Result<Zeroizing<String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Zeroizing::new("fictional-access-token".into()))
        }
    }

    #[test]
    fn cursors_are_bound_to_the_exact_origin_and_resource() {
        let endpoint =
            Url::parse("https://graph.microsoft.com/v1.0/chats/19:allowed@thread.v2/messages")
                .unwrap();
        assert!(validate_cursor(&endpoint, "https://graph.microsoft.com/v1.0/chats/19%3Aallowed%40thread.v2/messages?$skiptoken=abc").is_ok());
        for cursor in [
            "https://graph.microsoft.com.evil.example/v1.0/chats/19:allowed@thread.v2/messages?$skiptoken=abc",
            "http://graph.microsoft.com/v1.0/chats/19:allowed@thread.v2/messages",
            "https://user@graph.microsoft.com/v1.0/chats/19:allowed@thread.v2/messages",
            "https://graph.microsoft.com/v1.0/chats/19:other@thread.v2/messages",
            "https://graph.microsoft.com/v1.0/chats/19:allowed@thread.v2/messages/other",
            "https://graph.microsoft.com/v1.0/chats/19:allowed%2Fother@thread.v2/messages",
            "https://graph.microsoft.com/v1.0/chats/19:allowed@thread.v2/messages#fragment",
        ] {
            assert!(validate_cursor(&endpoint, cursor).is_err(), "{cursor}");
        }
    }

    #[test]
    fn normalizes_html_and_suppresses_deleted_content() {
        let target = Target::Chat {
            chat_id: "19:allowed@thread.v2".into(),
        };
        let input = serde_json::json!({
            "id": "m1", "from": {"user": {"id": "u1", "displayName": "Ada"}},
            "body": {"contentType": "html", "content": "<p>Hello <b>world</b></p>"},
            "attachments": [{"id": "a1"}]
        });
        let result = normalize(serde_json::from_value(input).unwrap(), &target, None).unwrap();
        assert!(result.text.unwrap().contains("Hello world"));
        assert_eq!(result.attachment_count, Some(1));
        assert_eq!(result.content_trust, "untrusted");
        let deleted = serde_json::json!({
            "id": "m2", "deletedDateTime": "2026-01-01T00:00:00Z",
            "body": {"contentType": "text", "content": "must not be returned"}
        });
        assert!(
            normalize(serde_json::from_value(deleted).unwrap(), &target, None)
                .unwrap()
                .text
                .is_none()
        );
    }

    #[test]
    fn bounds_unicode_text_without_splitting_characters() {
        let target = Target::Chat {
            chat_id: "19:allowed@thread.v2".into(),
        };
        let input = json!({
            "id": "long", "body": {"contentType": "text", "content": "é🌎".repeat(5000)},
            "attachments": null
        });
        let result = normalize(serde_json::from_value(input).unwrap(), &target, None).unwrap();
        assert!(result.text_truncated);
        assert_eq!(result.text.unwrap().chars().count(), MAX_TEXT_CHARS);
        assert_eq!(result.attachment_count, Some(0));
    }

    #[tokio::test]
    async fn reads_any_chat_on_local_graph_and_invalid_cursors_skip_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let base_for_handler = base.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let count = count.clone();
            let base = base_for_handler.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(request.method(), axum::http::Method::GET);
                assert_eq!(request.headers().get("authorization").unwrap(), "Bearer fictional-access-token");
                let path = request.uri().path();
                let body = if path.ends_with("/messages/root-1/replies") {
                    json!({"value": [{"id": "reply-1", "replyToId": "root-1", "body": {"contentType": "text", "content": "Fictional reply"}}]})
                } else if path.ends_with("/messages/root-1") {
                    json!({"id": "root-1", "body": {"contentType": "text", "content": "Fictional root"}})
                } else if path.contains("other-chat") {
                    json!({"value": [{"id": "outside-1", "body": {"contentType": "text", "content": "Fictional outside chat"}}]})
                } else if request.uri().query().is_some_and(|query| query.contains("skiptoken")) {
                    json!({"value": [{"id": "page-2", "body": {"contentType": "text", "content": "Fictional second page"}}]})
                } else {
                    json!({
                        "value": [{"id": "page-1", "body": {"contentType": "html", "content": "<p>Fictional <b>message</b></p>"}}],
                        "@odata.nextLink": format!("{base}chats/19%3Afictional-chat%40thread.v2/messages?$skiptoken=next")
                    })
                };
                axum::Json(body).into_response()
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let token_calls = Arc::new(AtomicUsize::new(0));
        let source = GraphSource {
            client: http_client().unwrap(),
            token_provider: Arc::new(FakeToken {
                calls: token_calls.clone(),
            }),
            base: Url::parse(&base).unwrap(),
        };
        let service = TeamsService::new(demo_config(), Arc::new(source));
        let allowed = Target::Chat {
            chat_id: "19:fictional-chat@thread.v2".into(),
        };
        let denied = Target::Chat {
            chat_id: "19:other-chat@thread.v2".into(),
        };

        let response = service
            .messages(MessagesRequest {
                target: allowed.clone(),
                limit: None,
                cursor: Some("https://attacker.example/steal".into()),
            })
            .await;
        assert!(matches!(response, Err(AppError::InvalidCursor)));
        assert_eq!(token_calls.load(Ordering::SeqCst), 0);
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let outside = service
            .messages(MessagesRequest {
                target: denied.clone(),
                limit: None,
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(outside.messages[0].id, "outside-1");

        let first = service
            .messages(MessagesRequest {
                target: allowed.clone(),
                limit: Some(2),
                cursor: None,
            })
            .await
            .unwrap();
        assert!(!first.complete);
        assert!(
            first.messages[0]
                .text
                .as_ref()
                .unwrap()
                .contains("Fictional message")
        );
        let second = service
            .messages(MessagesRequest {
                target: allowed.clone(),
                limit: Some(2),
                cursor: first.next_cursor,
            })
            .await
            .unwrap();
        assert!(second.complete);
        assert_eq!(second.messages[0].id, "page-2");

        let search = service
            .search(SearchRequest {
                target: allowed,
                query: "FICTIONAL".into(),
                max_pages: Some(2),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(search.pages_scanned, 2);
        assert_eq!(search.hits.len(), 2);
        assert!(search.complete);

        let thread = service
            .thread(ThreadRequest {
                team_id: "00000000-0000-0000-0000-000000000003".into(),
                channel_id: "19:fictional-channel@thread.tacv2".into(),
                root_message_id: "root-1".into(),
                limit: Some(2),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(thread.root.id, "root-1");
        assert_eq!(thread.replies[0].root_message_id.as_deref(), Some("root-1"));
        assert_eq!(thread.replies[0].reply_to_id.as_deref(), Some("root-1"));
        assert_eq!(requests.load(Ordering::SeqCst), 7);
        assert_eq!(token_calls.load(Ordering::SeqCst), 7);
        let mut assisted = demo_config();
        assisted.interaction = Interaction::Assisted;
        let reply_service = TeamsService::new(
            assisted,
            Arc::new(GraphSource {
                client: http_client().unwrap(),
                token_provider: Arc::new(FakeToken {
                    calls: token_calls.clone(),
                }),
                base: Url::parse(&base).unwrap(),
            }),
        );
        let denied_reply = reply_service
            .prepare_reply(ProposeReplyRequest {
                target: denied,
                text: "A fictional reply that must not be sent.".into(),
                evidence: Vec::new(),
            })
            .await;
        assert!(matches!(denied_reply, Err(AppError::Forbidden)));
        assert_eq!(token_calls.load(Ordering::SeqCst), 7);
        task.abort();
    }

    #[tokio::test]
    async fn hosted_images_are_fetched_from_the_graph_host_for_that_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let png: &'static [u8] = b"\x89PNG\r\n\x1a\nhosted";
        let app = Router::new().fallback(any(move |request: Request| {
            let png = png.to_vec();
            async move {
                assert_eq!(
                    request.headers().get("authorization").unwrap(),
                    "Bearer fictional-access-token"
                );
                let path = request.uri().path();
                assert!(!path.contains("evil"));
                if path.contains("/hostedContents/") {
                    assert!(path.contains("/hostedContents/abc/"));
                    (
                        [(axum::http::header::CONTENT_TYPE, "image/png")],
                        png,
                    )
                        .into_response()
                } else {
                    axum::Json(json!({
                        "value": [{
                            "id": "m1",
                            "body": {
                                "contentType": "html",
                                "content": "<p>caption</p><img src=\"https://evil.example/hostedContents/abc/$value\"><img src=\"../hostedContents/abc/$value\">"
                            }
                        }]
                    }))
                    .into_response()
                }
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let source = GraphSource {
            client: http_client().unwrap(),
            token_provider: Arc::new(FakeToken {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            base: Url::parse(&base).unwrap(),
        };
        let service = TeamsService::new(demo_config(), Arc::new(source));
        let page = service
            .messages(MessagesRequest {
                target: crate::backend::Target::Chat {
                    chat_id: "19:image@thread.v2".into(),
                },
                limit: Some(5),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(page.messages[0].text.as_deref().unwrap().contains("caption"));
        assert_eq!(page.messages[0].images.len(), 1);
        assert!(page.messages[0].images[0].attached);
        assert_eq!(page.messages[0].image_bytes[0].mime_type, "image/png");
        assert!(page.messages[0].image_bytes[0].data.starts_with(b"\x89PNG"));
        task.abort();
    }
}
