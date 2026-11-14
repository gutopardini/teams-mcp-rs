use async_trait::async_trait;

use crate::{
    backend::{
        Author, Message, MessagePage, MessageSource, OpenChatMatch, OpenChatPage, Target,
        ThreadPage, message_coverage, title_matches_query, untrusted_notice,
    },
    config::Config,
    error::{AppError, Result},
};

pub struct DemoSource;

pub fn demo_config() -> Config {
    let mut config: Config = toml::from_str(include_str!("../config.example.toml"))
        .expect("valid fictional configuration");
    config.identity.client_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002")
        .expect("valid fictional client ID");
    config
}

fn root(target: &Target) -> Message {
    Message {
        id: "demo-message-1".into(),
        target: target.clone(),
        root_message_id: None,
        reply_to_id: None,
        author: Some(Author {
            id: Some("fictional-user-1".into()),
            display_name: Some("Alex Example".into()),
            kind: "user".into(),
        }),
        created_at: Some("2026-01-01T10:00:00Z".into()),
        modified_at: None,
        deleted: Some(false),
        text: Some("The team will investigate the fictional incident.".into()),
        text_truncated: false,
        attachment_count: Some(0),
        images: Vec::new(),
        web_url: None,
        content_trust: "untrusted",
        image_bytes: Vec::new(),
        pending_image_urls: Vec::new(),
        hosted_content_ids: Vec::new(),
    }
}

#[async_trait]
impl MessageSource for DemoSource {
    fn name(&self) -> &'static str {
        "demo"
    }

    async fn messages(
        &self,
        target: &Target,
        _limit: u32,
        cursor: Option<&str>,
    ) -> Result<MessagePage> {
        if cursor.is_some() {
            return Err(AppError::InvalidCursor);
        }
        Ok(MessagePage {
            messages: vec![root(target)],
            coverage: message_coverage(target),
            next_cursor: None,
            complete: true,
            notice: untrusted_notice(),
        })
    }

    async fn thread(
        &self,
        target: &Target,
        root_message_id: &str,
        _limit: u32,
        cursor: Option<&str>,
    ) -> Result<ThreadPage> {
        if cursor.is_some() {
            return Err(AppError::InvalidCursor);
        }
        if root_message_id != "demo-message-1" {
            return Err(AppError::InvalidInput);
        }
        let mut reply = root(target);
        reply.id = "demo-reply-1".into();
        reply.root_message_id = Some(root_message_id.into());
        reply.reply_to_id = Some(root_message_id.into());
        reply.text = Some("A follow-up is planned for tomorrow.".into());
        Ok(ThreadPage {
            root: root(target),
            replies: vec![reply],
            next_cursor: None,
            complete: true,
            notice: untrusted_notice(),
        })
    }

    async fn search_open_chats(&self, query: &str) -> Result<OpenChatPage> {
        let chats = [
            ("19:fictional-chat@thread.v2", "Example chat"),
            ("19:allan-and-jose@thread.v2", "Allan and José"),
        ];
        Ok(OpenChatPage {
            matches: chats
                .into_iter()
                .filter(|(_, title)| title_matches_query(title, query))
                .map(|(chat_id, label)| OpenChatMatch {
                    chat_id: chat_id.into(),
                    label: label.into(),
                })
                .collect(),
            complete: true,
        })
    }
}
