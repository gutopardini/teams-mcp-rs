use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::{Config, Interaction, valid_id},
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
    /// None when the backend cannot determine deletion state reliably.
    pub deleted: Option<bool>,
    pub text: Option<String>,
    pub text_truncated: bool,
    /// None when the backend cannot determine the count reliably.
    pub attachment_count: Option<usize>,
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    #[schemars(description = "Chat or channel label, or an exact conversation ID")]
    pub query: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindRequest {
    pub target: Target,
    #[schemars(description = "Case-insensitive author display name, from 2 to 128 characters")]
    pub author: String,
    #[schemars(description = "Optional case-insensitive text filter, at most 256 characters")]
    pub query: Option<String>,
    #[schemars(description = "Maximum pages to scan, from 1 to 3; defaults to 1")]
    pub max_pages: Option<u32>,
    #[schemars(description = "Opaque cursor returned for this exact resource")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCite {
    pub message_id: String,
    pub excerpt: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeReplyRequest {
    pub target: Target,
    #[schemars(description = "Exact reply text to show the user. It is not sent unless they confirm.")]
    pub text: String,
    #[schemars(description = "One to five quotes that must match fetched messages")]
    pub evidence: Vec<EvidenceCite>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PollRequest {
    #[schemars(description = "Watch label. Omit only when one watch is configured.")]
    pub label: Option<String>,
    #[schemars(description = "Previous baseline_id. Omit to use the baseline remembered by this process.")]
    pub since_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolveCandidate {
    pub target: Target,
    pub label: String,
    pub match_quality: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolveOutcome {
    pub status: &'static str,
    pub selected: Option<Target>,
    pub candidates: Vec<ResolveCandidate>,
    pub notice: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenChatQuery {
    #[schemars(
        description = "Text from the open-chat search box. It matches chat titles only and does not read messages."
    )]
    pub query: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenChatMatch {
    pub chat_id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenChatPage {
    pub matches: Vec<OpenChatMatch>,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenChatCandidate {
    pub target: Target,
    pub label: String,
    pub authorized: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenChatOutcome {
    pub status: &'static str,
    pub selected: Option<Target>,
    pub candidates: Vec<OpenChatCandidate>,
    pub complete: bool,
    pub notice: String,
}

/// Case-insensitive substring of a chat title, matching the open-chat list.
/// The query is matched as typed aside from case. Accents stay significant.
pub fn title_matches_query(title: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    if title.is_ascii() && query.is_ascii() {
        let needle = query.as_bytes();
        return title.len() >= needle.len()
            && title.as_bytes().windows(needle.len()).any(|window| {
                window
                    .iter()
                    .zip(needle)
                    .all(|(byte, expected)| byte.to_ascii_lowercase() == *expected)
            });
    }
    title.to_lowercase().contains(&query)
}

#[derive(Clone, Debug, Serialize)]
pub struct FindOutcome {
    pub status: &'static str,
    pub authors: Vec<String>,
    pub messages: Vec<Message>,
    pub coverage: &'static str,
    pub pages_scanned: u32,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub notice: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifiedEvidence {
    pub message_id: String,
    pub excerpt: String,
    pub author: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PreparedReply {
    pub target: Target,
    pub destination_label: String,
    pub text: String,
    pub evidence: Vec<VerifiedEvidence>,
    pub confirmation_message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SendReceipt {
    pub message_id: Option<String>,
    pub client_message_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PollOutcome {
    pub status: &'static str,
    pub label: Option<String>,
    pub purpose: Option<&'static str>,
    pub baseline_id: Option<String>,
    pub messages: Vec<Message>,
    pub truncated: bool,
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

    fn supports_send(&self, _target: &Target) -> bool {
        false
    }

    async fn send_text(
        &self,
        _target: &Target,
        _text: &str,
        _client_message_id: &str,
    ) -> Result<SendReceipt> {
        Err(AppError::UnsupportedOperation)
    }

    /// Titles of chats already open in Teams. The default does not invent a directory.
    async fn search_open_chats(&self, _query: &str) -> Result<OpenChatPage> {
        Err(AppError::UnsupportedOperation)
    }
}

#[derive(Clone)]
pub struct TeamsService {
    pub config: Arc<Config>,
    source: Arc<dyn MessageSource>,
    watch_state: Arc<Mutex<HashMap<String, String>>>,
}

impl TeamsService {
    pub fn new(config: Config, source: Arc<dyn MessageSource>) -> Self {
        Self {
            config: Arc::new(config),
            source,
            watch_state: Arc::new(Mutex::new(HashMap::new())),
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
        let mut coverage = message_coverage(&request.target);
        for _ in 0..max_pages {
            let page = self
                .messages(MessagesRequest {
                    target: request.target.clone(),
                    limit: Some(25),
                    cursor,
                })
                .await?;
            coverage = page.coverage;
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

    pub fn resolve(&self, request: ResolveRequest) -> Result<ResolveOutcome> {
        let query = request.query.trim();
        if query.is_empty()
            || query.chars().count() > 256
            || query.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput);
        }
        let query_n = query.to_lowercase();
        let mut exact = Vec::new();
        let mut partial = Vec::new();
        for chat in &self.config.chats {
            let label_n = chat.label.trim().to_lowercase();
            let candidate = ResolveCandidate {
                target: Target::Chat {
                    chat_id: chat.chat_id.clone(),
                },
                label: chat.label.clone(),
                match_quality: "exact",
            };
            if label_n == query_n || chat.chat_id.to_lowercase() == query_n {
                exact.push(candidate);
            } else if query.chars().count() >= 2 && label_n.contains(&query_n) {
                partial.push(ResolveCandidate {
                    match_quality: "partial",
                    ..candidate
                });
            }
        }
        for channel in &self.config.channels {
            let label_n = channel.label.trim().to_lowercase();
            let candidate = ResolveCandidate {
                target: Target::Channel {
                    team_id: channel.team_id.to_string(),
                    channel_id: channel.channel_id.clone(),
                },
                label: channel.label.clone(),
                match_quality: "exact",
            };
            if label_n == query_n || channel.channel_id.to_lowercase() == query_n {
                exact.push(candidate);
            } else if query.chars().count() >= 2 && label_n.contains(&query_n) {
                partial.push(ResolveCandidate {
                    match_quality: "partial",
                    ..candidate
                });
            }
        }
        if exact.len() == 1 {
            let selected = exact[0].target.clone();
            return Ok(ResolveOutcome {
                status: "resolved",
                selected: Some(selected),
                candidates: exact,
                notice: locale::text_for(&self.config.language, "notice-untrusted"),
            });
        }
        let mut candidates = if exact.is_empty() { partial } else { exact };
        if candidates.is_empty() {
            candidates = self.listed_resources();
        }
        Ok(ResolveOutcome {
            status: if candidates.is_empty() {
                "not_found"
            } else {
                "needs_confirmation"
            },
            selected: None,
            candidates,
            notice: locale::text_for(&self.config.language, "notice-ask-which"),
        })
    }

    pub async fn search_open_chats(&self, request: OpenChatQuery) -> Result<OpenChatOutcome> {
        let query = request.query.trim();
        if query.chars().count() == 0
            || query.chars().count() > 256
            || query.chars().any(char::is_control)
        {
            return Err(AppError::InvalidInput);
        }
        let page = self.source.search_open_chats(query).await?;
        let mut matches = page.matches;
        let truncated = matches.len() > 20;
        matches.truncate(20);
        let unique = matches.len() == 1 && page.complete && !truncated;
        let selected = unique.then(|| Target::Chat {
            chat_id: matches[0].chat_id.clone(),
        });
        let status = if matches.is_empty() {
            "not_found"
        } else if unique {
            "resolved"
        } else {
            "needs_confirmation"
        };
        let notice_key = if !page.complete {
            "notice-open-chat-partial"
        } else if matches.is_empty() {
            "notice-open-chat-none"
        } else if unique {
            "notice-untrusted"
        } else {
            "notice-ask-which"
        };
        let candidates = matches
            .into_iter()
            .map(|chat| OpenChatCandidate {
                authorized: self.config.allows_chat(&chat.chat_id),
                target: Target::Chat {
                    chat_id: chat.chat_id,
                },
                label: chat.label,
            })
            .collect();
        Ok(OpenChatOutcome {
            status,
            selected,
            candidates,
            complete: page.complete && !truncated,
            notice: locale::text_for(&self.config.language, notice_key),
        })
    }

    pub async fn find(&self, request: FindRequest) -> Result<FindOutcome> {
        let author = request.author.trim();
        let max_pages = request.max_pages.unwrap_or(1);
        let text_query = request.query.as_deref().map(str::trim).filter(|q| !q.is_empty());
        if !(2..=128).contains(&author.chars().count())
            || author.chars().any(char::is_control)
            || !(1..=3).contains(&max_pages)
            || text_query.is_some_and(|query| {
                query.chars().count() > 256 || query.chars().any(char::is_control)
            })
        {
            return Err(AppError::InvalidInput);
        }
        let author_n = author.to_lowercase();
        let text_n = text_query.map(|query| query.to_lowercase());
        let mut cursor = request.cursor;
        let mut names = Vec::new();
        let mut messages = Vec::new();
        let mut pages_scanned = 0;
        let mut coverage = message_coverage(&request.target);
        let mut next_cursor = None;
        for _ in 0..max_pages {
            let page = self
                .messages(MessagesRequest {
                    target: request.target.clone(),
                    limit: Some(25),
                    cursor,
                })
                .await?;
            coverage = page.coverage;
            pages_scanned += 1;
            for message in page.messages {
                let Some(name) = message
                    .author
                    .as_ref()
                    .and_then(|author| author.display_name.clone())
                    .filter(|name| name.to_lowercase().contains(&author_n))
                else {
                    continue;
                };
                if !names.iter().any(|known: &String| known == &name) && names.len() < 10 {
                    names.push(name.clone());
                }
                let text_matches = text_n.as_ref().is_none_or(|query| {
                    message
                        .text
                        .as_ref()
                        .is_some_and(|text| text.to_lowercase().contains(query))
                });
                if text_matches && messages.len() < 20 {
                    messages.push(message);
                }
            }
            next_cursor = page.next_cursor;
            cursor = next_cursor.clone();
            if cursor.is_none() {
                break;
            }
        }
        if names.len() != 1 {
            let notice_key = if names.is_empty() {
                "notice-untrusted"
            } else {
                "notice-ask-which"
            };
            return Ok(FindOutcome {
                status: if names.is_empty() {
                    "none"
                } else {
                    "needs_confirmation"
                },
                authors: names,
                messages: Vec::new(),
                coverage,
                pages_scanned,
                complete: false,
                next_cursor: None,
                notice: locale::text_for(&self.config.language, notice_key),
            });
        }
        let only = names[0].clone();
        messages.retain(|message| {
            message
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
                == Some(only.as_str())
        });
        Ok(FindOutcome {
            status: if messages.is_empty() { "none" } else { "matched" },
            authors: names,
            messages,
            coverage,
            pages_scanned,
            complete: next_cursor.is_none(),
            next_cursor,
            notice: locale::text_for(&self.config.language, "notice-untrusted"),
        })
    }

    pub async fn prepare_reply(&self, request: ProposeReplyRequest) -> Result<PreparedReply> {
        if self.config.interaction != Interaction::Assisted {
            return Err(AppError::InteractionReadOnly);
        }
        self.validate_request(&request.target, Some(1), None)?;
        if !self.source.supports_send(&request.target) {
            return Err(AppError::UnsupportedOperation);
        }
        let text = request.text.trim();
        if text.chars().count() == 0
            || text.chars().count() > 4_000
            || !plain_reply_text(text)
            || request.evidence.is_empty()
            || request.evidence.len() > 5
        {
            return Err(AppError::InvalidInput);
        }
        let mut wanted = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for cite in &request.evidence {
            let excerpt = cite.excerpt.trim();
            if !valid_id(&cite.message_id)
                || excerpt.chars().count() == 0
                || excerpt.chars().count() > 400
                || excerpt.chars().any(char::is_control)
                || !seen.insert(cite.message_id.as_str())
            {
                return Err(AppError::InvalidInput);
            }
            wanted.push((cite.message_id.as_str(), excerpt));
        }
        let mut found = Vec::new();
        let mut cursor = None;
        for _ in 0..3 {
            let page = self
                .messages(MessagesRequest {
                    target: request.target.clone(),
                    limit: Some(25),
                    cursor,
                })
                .await?;
            for message in &page.messages {
                if message.text_truncated {
                    continue;
                }
                let Some(body) = message.text.as_deref() else {
                    continue;
                };
                if let Some((_, excerpt)) = wanted.iter().find(|(id, _)| *id == message.id) {
                    if body.contains(excerpt) {
                        found.push(VerifiedEvidence {
                            message_id: message.id.clone(),
                            excerpt: (*excerpt).to_owned(),
                            author: message
                                .author
                                .as_ref()
                                .and_then(|author| author.display_name.clone()),
                            created_at: message.created_at.clone(),
                        });
                    }
                }
            }
            if found.len() == wanted.len() || page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }
        if found.len() != wanted.len() {
            return Err(AppError::EvidenceRejected);
        }
        let label = self.destination_label(&request.target);
        let evidence_lines = found
            .iter()
            .map(|item| {
                format!(
                    "- {}: {} [{}]",
                    item.author.as_deref().unwrap_or("unknown"),
                    item.excerpt,
                    item.message_id
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let confirmation_message = format!(
            "{header}\n\nDestination: {label}\nAccount: {account}\n\nMessage:\n{text}\n\nEvidence:\n{evidence}\n\nType send to publish this exact message. It is a new chat or channel post, not a thread reply.",
            header = locale::text_for(&self.config.language, "notice-confirm-header"),
            account = self.config.identity.account_id,
            evidence = evidence_lines,
        );
        if confirmation_message.chars().count() > 12_000 {
            return Err(AppError::InvalidInput);
        }
        Ok(PreparedReply {
            target: request.target,
            destination_label: label,
            text: text.to_owned(),
            evidence: found,
            confirmation_message,
        })
    }

    pub async fn publish_prepared(&self, prepared: &PreparedReply) -> Result<SendReceipt> {
        if self.config.interaction != Interaction::Assisted {
            return Err(AppError::InteractionReadOnly);
        }
        self.validate_request(&prepared.target, Some(1), None)?;
        if !self.source.supports_send(&prepared.target) {
            return Err(AppError::UnsupportedOperation);
        }
        if !plain_reply_text(&prepared.text) {
            return Err(AppError::InvalidInput);
        }
        let client_message_id = new_client_message_id();
        self.source
            .send_text(&prepared.target, &prepared.text, &client_message_id)
            .await
    }

    pub async fn poll(&self, request: PollRequest) -> Result<PollOutcome> {
        if let Some(since_id) = &request.since_id {
            if since_id.is_empty() || since_id.len() > 512 || !valid_id(since_id) {
                return Err(AppError::InvalidInput);
            }
        }
        let watch = match self.selected_watch(request.label.as_deref())? {
            WatchPick::Ask { labels } => {
                return Ok(PollOutcome {
                    status: "needs_confirmation",
                    label: None,
                    purpose: None,
                    baseline_id: None,
                    messages: Vec::new(),
                    truncated: false,
                    notice: format!(
                        "{}\n{}",
                        locale::text_for(&self.config.language, "notice-ask-which"),
                        labels.join(", ")
                    ),
                });
            }
            WatchPick::None => {
                return Ok(PollOutcome {
                    status: "not_configured",
                    label: None,
                    purpose: None,
                    baseline_id: None,
                    messages: Vec::new(),
                    truncated: false,
                    notice: locale::text_for(&self.config.language, "notice-watch-unattended"),
                });
            }
            WatchPick::One(watch) => watch,
        };
        let stored = self
            .watch_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&watch.label)
            .cloned();
        let since = request.since_id.or(stored);
        let target = Target::Chat {
            chat_id: watch.chat_id.clone(),
        };
        let page = self
            .messages(MessagesRequest {
                target,
                limit: Some(25),
                cursor: None,
            })
            .await?;
        let baseline_id = newest_id(&page.messages);
        let unattended = locale::text_for(&self.config.language, "notice-watch-unattended");
        let Some(since_id) = since else {
            self.remember_watch(&watch.label, &baseline_id);
            return Ok(PollOutcome {
                status: "baseline",
                label: Some(watch.label),
                purpose: Some(watch.purpose),
                baseline_id,
                messages: Vec::new(),
                truncated: false,
                notice: format!(
                    "{}\n{unattended}",
                    locale::text_for(&self.config.language, "notice-watch-baseline")
                ),
            });
        };
        let Some(anchor) = page.messages.iter().find(|message| message.id == since_id) else {
            self.remember_watch(&watch.label, &baseline_id);
            return Ok(PollOutcome {
                status: "gap",
                label: Some(watch.label),
                purpose: Some(watch.purpose),
                baseline_id,
                messages: Vec::new(),
                truncated: false,
                notice: format!(
                    "{}\n{unattended}",
                    locale::text_for(&self.config.language, "notice-watch-gap")
                ),
            });
        };
        let mut newer = Vec::new();
        for message in &page.messages {
            if message.id == anchor.id {
                continue;
            }
            match newer_than(message, anchor) {
                Some(true) => newer.push(message.clone()),
                Some(false) => {}
                None => {
                    self.remember_watch(&watch.label, &baseline_id);
                    return Ok(PollOutcome {
                        status: "gap",
                        label: Some(watch.label),
                        purpose: Some(watch.purpose),
                        baseline_id,
                        messages: Vec::new(),
                        truncated: false,
                        notice: format!(
                            "{}\n{unattended}",
                            locale::text_for(&self.config.language, "notice-watch-gap")
                        ),
                    });
                }
            }
        }
        let truncated = newer.len() > 20;
        newer.truncate(20);
        self.remember_watch(&watch.label, &baseline_id);
        Ok(PollOutcome {
            status: if newer.is_empty() { "baseline" } else { "messages" },
            label: Some(watch.label),
            purpose: Some(watch.purpose),
            baseline_id,
            messages: newer,
            truncated,
            notice: format!("{unattended}\n{}", locale::text_for(&self.config.language, "notice-untrusted")),
        })
    }

    fn remember_watch(&self, label: &str, baseline: &Option<String>) {
        if let Some(baseline) = baseline {
            self.watch_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(label.to_owned(), baseline.clone());
        }
    }

    fn listed_resources(&self) -> Vec<ResolveCandidate> {
        let mut candidates = Vec::new();
        for chat in &self.config.chats {
            candidates.push(ResolveCandidate {
                target: Target::Chat {
                    chat_id: chat.chat_id.clone(),
                },
                label: chat.label.clone(),
                match_quality: "listed",
            });
        }
        for channel in &self.config.channels {
            candidates.push(ResolveCandidate {
                target: Target::Channel {
                    team_id: channel.team_id.to_string(),
                    channel_id: channel.channel_id.clone(),
                },
                label: channel.label.clone(),
                match_quality: "listed",
            });
        }
        candidates
    }

    fn destination_label(&self, target: &Target) -> String {
        match target {
            Target::Chat { chat_id } => self
                .config
                .chats
                .iter()
                .find(|chat| &chat.chat_id == chat_id)
                .map(|chat| chat.label.clone())
                .unwrap_or_else(|| chat_id.clone()),
            Target::Channel {
                team_id,
                channel_id,
            } => self
                .config
                .channels
                .iter()
                .find(|channel| {
                    channel.team_id.to_string() == *team_id && &channel.channel_id == channel_id
                })
                .map(|channel| channel.label.clone())
                .unwrap_or_else(|| channel_id.clone()),
        }
    }

    fn selected_watch(&self, label: Option<&str>) -> Result<WatchPick> {
        if self.config.watches.is_empty() {
            return Ok(WatchPick::None);
        }
        let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) else {
            if self.config.watches.len() == 1 {
                let watch = &self.config.watches[0];
                return Ok(WatchPick::One(SelectedWatch {
                    chat_id: watch.chat_id.clone(),
                    label: watch.label.clone(),
                    purpose: "code_review",
                }));
            }
            return Ok(WatchPick::Ask {
                labels: self
                    .config
                    .watches
                    .iter()
                    .map(|watch| watch.label.clone())
                    .collect(),
            });
        };
        if label.chars().any(char::is_control) || label.chars().count() > 256 {
            return Err(AppError::InvalidInput);
        }
        let matches: Vec<_> = self
            .config
            .watches
            .iter()
            .filter(|watch| watch.label.eq_ignore_ascii_case(label))
            .collect();
        if matches.len() == 1 {
            let watch = matches[0];
            return Ok(WatchPick::One(SelectedWatch {
                chat_id: watch.chat_id.clone(),
                label: watch.label.clone(),
                purpose: "code_review",
            }));
        }
        Ok(WatchPick::Ask {
            labels: self
                .config
                .watches
                .iter()
                .map(|watch| watch.label.clone())
                .collect(),
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

struct SelectedWatch {
    chat_id: String,
    label: String,
    purpose: &'static str,
}

enum WatchPick {
    None,
    One(SelectedWatch),
    Ask { labels: Vec<String> },
}

fn plain_reply_text(text: &str) -> bool {
    text.chars().any(|ch| !ch.is_whitespace())
        && text.chars().all(|ch| {
            ch == '\n'
                || ch == '\t'
                || (!ch.is_control() && !('\u{202A}'..='\u{202E}').contains(&ch) && !('\u{2066}'..='\u{2069}').contains(&ch))
        })
}

fn new_client_message_id() -> String {
    let value = u128::from_be_bytes(*uuid::Uuid::new_v4().as_bytes());
    let n = 1_000_000_000_000_000_000u128 + (value % 9_000_000_000_000_000_000u128);
    n.to_string()
}

fn newest_id(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .filter(|message| {
            message
                .created_at
                .as_deref()
                .is_some_and(|stamp| stamp.contains('T'))
        })
        .max_by(|left, right| left.created_at.cmp(&right.created_at))
        .map(|message| message.id.clone())
        .or_else(|| messages.last().map(|message| message.id.clone()))
}

fn newer_than(message: &Message, anchor: &Message) -> Option<bool> {
    let left = message.created_at.as_deref()?;
    let right = anchor.created_at.as_deref()?;
    if !left.contains('T') || !right.contains('T') {
        return None;
    }
    Some(left > right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{
        config::Interaction,
        demo::{DemoSource, demo_config},
    };

    #[test]
    fn open_chat_title_search_matches_the_sidebar_rule() {
        assert!(title_matches_query("Ana Costa", "ana"));
        assert!(title_matches_query("ANA", "ana"));
        assert!(!title_matches_query("Ana", "maria"));
        assert!(!title_matches_query("Ana", "anabela"));
        assert!(title_matches_query("João", "joão"));
        assert!(title_matches_query("José Almeida", "josé"));
        assert!(!title_matches_query("José", "jose"));
        assert!(title_matches_query("Allan and José", "josé"));
        assert!(title_matches_query("Qualquer", ""));
    }

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
                    deleted: Some(false),
                    text: Some("Fictional matching text".into()),
                    text_truncated: index == 0,
                    attachment_count: Some(0),
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

    fn sample(id: &str, author: &str, text: &str, at: &str) -> Message {
        Message {
            id: id.into(),
            target: Target::Chat {
                chat_id: "19:fictional-chat@thread.v2".into(),
            },
            root_message_id: None,
            reply_to_id: None,
            author: Some(Author {
                id: None,
                display_name: Some(author.into()),
                kind: "user".into(),
            }),
            created_at: Some(at.into()),
            modified_at: None,
            deleted: Some(false),
            text: Some(text.into()),
            text_truncated: false,
            attachment_count: Some(0),
            web_url: None,
            content_trust: "untrusted",
        }
    }

    struct Scripted {
        messages: Vec<Message>,
        sends: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MessageSource for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }

        async fn messages(
            &self,
            target: &Target,
            _limit: u32,
            _cursor: Option<&str>,
        ) -> Result<MessagePage> {
            Ok(MessagePage {
                messages: self.messages.clone(),
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

        fn supports_send(&self, target: &Target) -> bool {
            matches!(target, Target::Chat { .. })
        }

        async fn send_text(
            &self,
            _target: &Target,
            _text: &str,
            client_message_id: &str,
        ) -> Result<SendReceipt> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(SendReceipt {
                message_id: Some("sent-1".into()),
                client_message_id: client_message_id.into(),
            })
        }
    }

    #[test]
    fn resolve_accepts_one_exact_label_and_asks_otherwise() {
        let service = TeamsService::new(demo_config(), Arc::new(DemoSource));
        let exact = service
            .resolve(ResolveRequest {
                query: " example chat ".into(),
            })
            .unwrap();
        assert_eq!(exact.status, "resolved");
        assert!(exact.selected.is_some());
        let ambiguous = service
            .resolve(ResolveRequest {
                query: "example".into(),
            })
            .unwrap();
        assert_eq!(ambiguous.status, "needs_confirmation");
        assert!(ambiguous.selected.is_none());
        assert!(ambiguous.candidates.len() >= 2);
        let missing = service
            .resolve(ResolveRequest {
                query: "not-a-real-room".into(),
            })
            .unwrap();
        assert_eq!(missing.status, "needs_confirmation");
        assert!(missing.selected.is_none());
        assert!(!missing.candidates.is_empty());
    }

    #[tokio::test]
    async fn find_asks_when_the_author_matches_more_than_one_person() {
        let service = TeamsService::new(
            demo_config(),
            Arc::new(Scripted {
                messages: vec![
                    sample("1", "Ana Silva", "Please review the patch.", "2026-01-01T10:00:00Z"),
                    sample("2", "Ana Costa", "I left a comment.", "2026-01-01T11:00:00Z"),
                ],
                sends: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let ambiguous = service
            .find(FindRequest {
                target: Target::Chat {
                    chat_id: "19:fictional-chat@thread.v2".into(),
                },
                author: "Ana".into(),
                query: None,
                max_pages: Some(1),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(ambiguous.status, "needs_confirmation");
        assert!(ambiguous.messages.is_empty());
        assert_eq!(ambiguous.authors.len(), 2);
        let one = service
            .find(FindRequest {
                target: Target::Chat {
                    chat_id: "19:fictional-chat@thread.v2".into(),
                },
                author: "Ana Silva".into(),
                query: Some("patch".into()),
                max_pages: Some(1),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(one.status, "matched");
        assert_eq!(one.messages.len(), 1);
        assert_eq!(one.messages[0].id, "1");
    }

    #[tokio::test]
    async fn prepare_requires_assisted_mode_and_a_real_excerpt_before_any_send() {
        let sends = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(Scripted {
            messages: vec![sample(
                "1",
                "Ana Silva",
                "Please review the patch.",
                "2026-01-01T10:00:00Z",
            )],
            sends: sends.clone(),
        });
        let readonly = TeamsService::new(demo_config(), source.clone());
        let request = ProposeReplyRequest {
            target: Target::Chat {
                chat_id: "19:fictional-chat@thread.v2".into(),
            },
            text: "I will review it.".into(),
            evidence: vec![EvidenceCite {
                message_id: "1".into(),
                excerpt: "review the patch".into(),
            }],
        };
        assert!(matches!(
            readonly.prepare_reply(request.clone()).await,
            Err(AppError::InteractionReadOnly)
        ));
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        let mut config = demo_config();
        config.interaction = Interaction::Assisted;
        let assisted = TeamsService::new(config, source);
        let rejected = assisted
            .prepare_reply(ProposeReplyRequest {
                evidence: vec![EvidenceCite {
                    message_id: "1".into(),
                    excerpt: "this sentence was not said".into(),
                }],
                ..request.clone()
            })
            .await;
        assert!(matches!(rejected, Err(AppError::EvidenceRejected)));
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        let prepared = assisted.prepare_reply(request).await.unwrap();
        assert!(prepared.confirmation_message.contains("I will review it."));
        assert!(prepared.confirmation_message.contains("Type send"));
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        let receipt = assisted.publish_prepared(&prepared).await.unwrap();
        assert_eq!(receipt.message_id.as_deref(), Some("sent-1"));
        assert_eq!(sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn watch_polling_establishes_a_baseline_and_does_not_replay_old_requests() {
        use crate::config::{Watch, WatchPurpose};
        let mut config = demo_config();
        config.watches.push(Watch {
            chat_id: "19:fictional-chat@thread.v2".into(),
            label: "Example review".into(),
            purpose: WatchPurpose::CodeReview,
        });
        let service = TeamsService::new(
            config,
            Arc::new(Scripted {
                messages: vec![
                    sample("1", "Ana Silva", "Old note", "2026-01-01T10:00:00Z"),
                    sample("2", "Ana Silva", "Please review PR 9", "2026-01-01T12:00:00Z"),
                ],
                sends: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let first = service
            .poll(PollRequest {
                label: None,
                since_id: None,
            })
            .await
            .unwrap();
        assert_eq!(first.status, "baseline");
        assert!(first.messages.is_empty());
        assert_eq!(first.baseline_id.as_deref(), Some("2"));
        let second = service
            .poll(PollRequest {
                label: None,
                since_id: None,
            })
            .await
            .unwrap();
        assert!(second.messages.is_empty());
        let caught_up = service
            .poll(PollRequest {
                label: Some("Example review".into()),
                since_id: Some("1".into()),
            })
            .await
            .unwrap();
        assert_eq!(caught_up.status, "messages");
        assert_eq!(caught_up.messages.len(), 1);
        assert_eq!(caught_up.messages[0].id, "2");
        let gap = service
            .poll(PollRequest {
                label: Some("Example review".into()),
                since_id: Some("missing-id".into()),
            })
            .await
            .unwrap();
        assert_eq!(gap.status, "gap");
        assert!(gap.messages.is_empty());
    }
}
