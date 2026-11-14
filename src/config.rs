use std::{collections::HashSet, path::Path};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, Result};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub backend: Backend,
    /// `read_only` denies reply tools. `assisted` can publish only after the MCP client confirms.
    #[serde(default)]
    pub interaction: Interaction,
    pub identity: Identity,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub chats: Vec<Chat>,
    #[serde(default)]
    pub watches: Vec<Watch>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Graph,
    NativeTeams,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Interaction {
    #[default]
    ReadOnly,
    Assisted,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchPurpose {
    CodeReview,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    pub chat_id: String,
    pub label: String,
    pub purpose: WatchPurpose,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub tenant_id: Uuid,
    #[serde(default = "default_client_id")]
    pub client_id: Uuid,
    pub account_id: Uuid,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub team_id: Uuid,
    pub channel_id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    pub chat_id: String,
    pub label: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|_| AppError::InvalidConfig)?;
        if raw.len() > 128 * 1024 {
            return Err(AppError::InvalidConfig);
        }
        let config: Self = toml::from_str(&raw).map_err(|_| AppError::InvalidConfig)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.identity.tenant_id.is_nil()
            || self.identity.client_id.is_nil()
            || self.identity.account_id.is_nil()
            || self
                .language
                .parse::<unic_langid::LanguageIdentifier>()
                .is_err()
        {
            return Err(AppError::InvalidConfig);
        }
        let mut channels = HashSet::new();
        for channel in &self.channels {
            if channel.team_id.is_nil()
                || !valid_id(&channel.channel_id)
                || !valid_label(&channel.label)
                || !channels.insert((channel.team_id, channel.channel_id.as_str()))
            {
                return Err(AppError::InvalidConfig);
            }
        }
        let mut chats = HashSet::new();
        for chat in &self.chats {
            if !valid_id(&chat.chat_id)
                || !valid_label(&chat.label)
                || !chats.insert(chat.chat_id.as_str())
            {
                return Err(AppError::InvalidConfig);
            }
        }
        if self.watches.len() > 8 {
            return Err(AppError::InvalidConfig);
        }
        let mut watch_labels = HashSet::new();
        for watch in &self.watches {
            if !chats.contains(watch.chat_id.as_str())
                || !valid_label(&watch.label)
                || !watch_labels.insert(watch.label.as_str())
            {
                return Err(AppError::InvalidConfig);
            }
        }
        Ok(())
    }

    pub fn allows_channel(&self, team_id: &str, channel_id: &str) -> bool {
        self.channels.iter().any(|channel| {
            channel.team_id.to_string() == team_id && channel.channel_id == channel_id
        })
    }

    pub fn allows_chat(&self, chat_id: &str) -> bool {
        self.chats.iter().any(|chat| chat.chat_id == chat_id)
    }

    pub fn scopes(&self) -> Result<Vec<&'static str>> {
        self.scopes_for_login(false)
    }

    pub fn scopes_for_login(&self, bootstrap_chat: bool) -> Result<Vec<&'static str>> {
        if self.channels.is_empty() && self.chats.is_empty() && !bootstrap_chat {
            return Err(AppError::NoResources);
        }
        if self.backend == Backend::NativeTeams {
            return Ok(vec![
                "openid",
                "profile",
                "offline_access",
                "https://api.spaces.skype.com/.default",
            ]);
        }
        let mut scopes = vec![
            "openid",
            "offline_access",
            "https://graph.microsoft.com/User.Read",
        ];
        if !self.channels.is_empty() {
            scopes.push("https://graph.microsoft.com/ChannelMessage.Read.All");
        }
        if !self.chats.is_empty() || bootstrap_chat {
            scopes.push("https://graph.microsoft.com/Chat.Read");
        }
        if self.interaction == Interaction::Assisted {
            if !self.channels.is_empty() {
                scopes.push("https://graph.microsoft.com/ChannelMessage.Send");
            }
            if !self.chats.is_empty() || bootstrap_chat {
                scopes.push("https://graph.microsoft.com/ChatMessage.Send");
            }
        }
        Ok(scopes)
    }
}

fn default_client_id() -> Uuid {
    Uuid::parse_str(crate::application_id::APPLICATION_ID).expect("valid built-in application ID")
}

fn default_language() -> String {
    "en-US".to_owned()
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        && id != "."
        && id != ".."
        && !id.chars().any(|c| c.is_control() || c == '/' || c == '\\')
}

fn valid_label(label: &str) -> bool {
    !label.trim().is_empty() && label.len() <= 256 && !label.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Config {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn allowlist_is_exact_and_scopes_follow_configured_resources() {
        let mut config = example();
        assert!(config.allows_chat("19:fictional-chat@thread.v2"));
        assert!(!config.allows_chat("19:other-chat@thread.v2"));
        assert!(config.allows_channel(
            "00000000-0000-0000-0000-000000000003",
            "19:fictional-channel@thread.tacv2"
        ));
        assert!(!config.allows_channel(
            "00000000-0000-0000-0000-000000000004",
            "19:fictional-channel@thread.tacv2"
        ));
        config.chats.clear();
        assert!(
            !config
                .scopes()
                .unwrap()
                .iter()
                .any(|scope| scope.ends_with("Chat.Read"))
        );
        config.channels.clear();
        assert!(matches!(config.scopes(), Err(AppError::NoResources)));
        let bootstrap = config.scopes_for_login(true).unwrap();
        assert!(bootstrap.iter().any(|scope| scope.ends_with("/Chat.Read")));
        assert!(
            !bootstrap
                .iter()
                .any(|scope| scope.ends_with("/ChannelMessage.Read.All"))
        );
        config.backend = Backend::NativeTeams;
        let scopes = config.scopes_for_login(true).unwrap();
        assert!(scopes.contains(&"https://api.spaces.skype.com/.default"));
        assert!(
            !scopes
                .iter()
                .any(|scope| scope.contains("graph.microsoft.com"))
        );
    }

    #[test]
    fn rejects_ambiguous_ids_duplicates_and_unknown_config_fields() {
        for id in ["", ".", "..", "a/b", "a\\b", "a\nb"] {
            assert!(!valid_id(id), "{id:?}");
        }
        let mut config = example();
        config.chats.push(config.chats[0].clone());
        assert!(matches!(config.validate(), Err(AppError::InvalidConfig)));
        let invalid = format!(
            "{}\nunknown = true\n",
            include_str!("../config.example.toml")
        );
        assert!(toml::from_str::<Config>(&invalid).is_err());
    }

    #[test]
    fn assisted_graph_login_requests_send_scopes_and_watches_stay_on_the_allowlist() {
        let mut config = example();
        assert!(
            !config
                .scopes()
                .unwrap()
                .iter()
                .any(|scope| scope.ends_with("Message.Send"))
        );
        config.interaction = Interaction::Assisted;
        let scopes = config.scopes().unwrap();
        assert!(scopes.contains(&"https://graph.microsoft.com/ChatMessage.Send"));
        assert!(scopes.contains(&"https://graph.microsoft.com/ChannelMessage.Send"));
        config.watches.push(Watch {
            chat_id: config.chats[0].chat_id.clone(),
            label: "Example review".into(),
            purpose: WatchPurpose::CodeReview,
        });
        config.validate().unwrap();
        config.watches[0].chat_id = "19:missing@thread.v2".into();
        assert!(matches!(config.validate(), Err(AppError::InvalidConfig)));
    }
}
