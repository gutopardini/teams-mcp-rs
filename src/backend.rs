use std::sync::Arc;

use async_trait::async_trait;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::{Config, valid_id},
    error::{AppError, Result},
    locale,
};

pub const MAX_PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Channel { team_id: String, channel_id: String },
    Chat { chat_id: String },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessagesRequest {
    pub target: Target,
    #[schemars(description = "Number of messages, from 1 to 50; defaults to 25")]
    pub limit: Option<u32>,
    #[schemars(description = "Opaque cursor returned for this exact resource")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadRequest {
    pub team_id: String,
    pub channel_id: String,
    pub root_message_id: String,
    #[schemars(description = "Number of replies, from 1 to 50; defaults to 25")]
    pub limit: Option<u32>,
    #[schemars(description = "Opaque reply cursor returned for this exact thread")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub target: Target,
    #[schemars(description = "Case-insensitive text query, at most 256 characters")]
    pub query: String,
    #[schemars(description = "Maximum pages to scan, from 1 to 3; defaults to 1")]
    pub max_pages: Option<u32>,
    #[schemars(description = "Opaque cursor returned for this exact resource")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Author {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub id: String,
    pub target: Target,
    pub root_message_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub author: Option<Author>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub deleted: bool,
    pub text: Option<String>,
    pub text_truncated: bool,
    pub attachment_count: usize,
    pub web_url: Option<String>,
    pub content_trust: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Channel listings contain root posts; replies require the thread tool.
    pub coverage: &'static str,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub notice: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadPage {
    pub root: Message,
    pub replies: Vec<Message>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub notice: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchPage {
    pub hits: Vec<Message>,
    pub coverage: &'static str,
    pub pages_scanned: u32,
    pub messages_scanned: usize,
    pub results_truncated: bool,
    pub source_text_truncated: bool,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub notice: String,
}

#[async_trait]
pub trait MessageSource: Send + Sync {
    fn name(&self) -> &'static str;

    async fn messages(
        &self,
        target: &Target,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<MessagePage>;

    async fn thread(
        &self,
        target: &Target,
        root_message_id: &str,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<ThreadPage>;
}

#[derive(Clone)]
pub struct TeamsService {
    pub config: Arc<Config>,
    source: Arc<dyn MessageSource>,
}

impl TeamsService {
    pub fn new(config: Config, source: Arc<dyn MessageSource>) -> Self {
        Self {
            config: Arc::new(config),
            source,
        }
    }

    pub fn source_name(&self) -> &'static str {
        self.source.name()
    }

    pub async fn messages(&self, request: MessagesRequest) -> Result<MessagePage> {
        let limit =
            self.validate_request(&request.target, request.limit, request.cursor.as_deref())?;
        let mut page = self
            .source
            .messages(&request.target, limit, request.cursor.as_deref())
            .await?;
        page.notice = locale::text_for(&self.config.language, "notice-untrusted");
        Ok(page)
    }

    pub async fn thread(&self, request: ThreadRequest) -> Result<ThreadPage> {
        let target = Target::Channel {
            team_id: request.team_id,
            channel_id: request.channel_id,
        };
        let limit = self.validate_request(&target, request.limit, request.cursor.as_deref())?;
        if !valid_id(&request.root_message_id) {
            return Err(AppError::InvalidInput);
        }
        let mut page = self
            .source
            .thread(
                &target,
                &request.root_message_id,
                limit,
                request.cursor.as_deref(),
            )
            .await?;
        page.notice = locale::text_for(&self.config.language, "notice-untrusted");
        Ok(page)
    }

    pub async fn search(&self, request: SearchRequest) -> Result<SearchPage> {
        let query = request.query.trim();
        let max_pages = request.max_pages.unwrap_or(1);
        if query.is_empty()
            || query.chars().count() > 256
            || query.chars().any(char::is_control)
            || !(1..=3).contains(&max_pages)
        {
            return Err(AppError::InvalidInput);
        }
        let query = query.to_lowercase();
        let mut cursor = request.cursor;
        let mut hits = Vec::new();
        let mut scanned = 0;
        let mut pages_scanned = 0;
        let mut truncated = false;
        let mut source_text_truncated = false;
        let coverage = message_coverage(&request.target);
        for _ in 0..max_pages {
            let page = self
                .messages(MessagesRequest {
                    target: request.target.clone(),
                    limit: Some(25),
                    cursor,
                })
                .await?;
            pages_scanned += 1;
            scanned += page.messages.len();
            for message in page.messages {
                source_text_truncated |= message.text_truncated;
                if message
                    .text
                    .as_ref()
                    .is_some_and(|text| text.to_lowercase().contains(&query))
                {
                    if hits.len() < 20 {
                        hits.push(message);
                    } else {
                        truncated = true;
                    }
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(SearchPage {
            hits,
            coverage,
            pages_scanned,
            messages_scanned: scanned,
            results_truncated: truncated,
            source_text_truncated,
            complete: cursor.is_none() && !truncated && !source_text_truncated,
            next_cursor: cursor,
            notice: locale::text_for(&self.config.language, "notice-untrusted"),
        })
    }

    fn validate_request(
        &self,
        target: &Target,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<u32> {
        let limit = limit.unwrap_or(25);
        if !(1..=MAX_PAGE_SIZE).contains(&limit)
            || cursor.is_some_and(|cursor| cursor.is_empty() || cursor.len() > 4096)
        {
            return Err(AppError::InvalidInput);
        }
        match target {
            Target::Chat { chat_id } if valid_id(chat_id) => {
                if !self.config.allows_chat(chat_id) {
                    return Err(AppError::Forbidden);
                }
            }
            Target::Channel {
                team_id,
                channel_id,
            } if Uuid::parse_str(team_id).is_ok() && valid_id(channel_id) => {
                if !self.config.allows_channel(team_id, channel_id) {
                    return Err(AppError::Forbidden);
                }
            }
            _ => return Err(AppError::InvalidInput),
        }
        Ok(limit)
    }
}

pub fn untrusted_notice() -> String {
    locale::text("notice-untrusted")
}

pub fn message_coverage(target: &Target) -> &'static str {
    match target {
        Target::Chat { .. } => "chat_messages",
        Target::Channel { .. } => "channel_root_posts_only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::demo::demo_config;

    struct BulkSource {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MessageSource for BulkSource {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn messages(
            &self,
            target: &Target,
            _limit: u32,
            _cursor: Option<&str>,
        ) -> Result<MessagePage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let messages = (0..25)
                .map(|index| Message {
                    id: index.to_string(),
                    target: target.clone(),
                    root_message_id: None,
                    reply_to_id: None,
                    author: None,
                    created_at: None,
                    modified_at: None,
                    deleted: false,
                    text: Some("Fictional matching text".into()),
                    text_truncated: index == 0,
                    attachment_count: 0,
                    web_url: None,
                    content_trust: "untrusted",
                })
                .collect();
            Ok(MessagePage {
                messages,
                coverage: message_coverage(target),
                next_cursor: None,
                complete: true,
                notice: untrusted_notice(),
            })
        }

        async fn thread(
            &self,
            _target: &Target,
            _root: &str,
            _limit: u32,
            _cursor: Option<&str>,
        ) -> Result<ThreadPage> {
            Err(AppError::InvalidInput)
        }
    }

    #[tokio::test]
    async fn scoped_search_bounds_results_and_reports_truncation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = TeamsService::new(
            demo_config(),
            Arc::new(BulkSource {
                calls: calls.clone(),
            }),
        );
        let target = Target::Chat {
            chat_id: "19:fictional-chat@thread.v2".into(),
        };
        let page = service
            .search(SearchRequest {
                target: target.clone(),
                query: "fictional".into(),
                max_pages: Some(1),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(page.hits.len(), 20);
        assert_eq!(page.messages_scanned, 25);
        assert!(page.results_truncated);
        assert!(!page.complete);
        let missing = service
            .search(SearchRequest {
                target: target.clone(),
                query: "not present".into(),
                max_pages: Some(1),
                cursor: None,
            })
            .await
            .unwrap();
        assert!(missing.hits.is_empty());
        assert!(missing.source_text_truncated);
        assert!(!missing.complete);
        let invalid = service
            .search(SearchRequest {
                target,
                query: "fictional".into(),
                max_pages: Some(4),
                cursor: None,
            })
            .await;
        assert!(matches!(invalid, Err(AppError::InvalidInput)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
