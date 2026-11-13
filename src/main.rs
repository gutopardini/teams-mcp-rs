use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use teams_mcp_rs::{
    auth::{MicrosoftAuth, SystemSecretStore},
    backend::{MessageSource, MessagesRequest, Target, TeamsService},
    config::{Backend, Config},
    demo::{DemoSource, demo_config},
    error::{AppError, Result},
    graph::GraphSource,
    locale,
    native::NativeSource,
    server::TeamsMcpServer,
};

#[derive(Parser)]
#[command(
    name = "teams-mcp",
    version,
    about = "A read-only-first Teams MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the MCP server over stdio.
    Serve {
        /// Configuration file with the account and explicit resource allowlist.
        #[arg(long, required_unless_present = "demo", conflicts_with = "demo")]
        config: Option<PathBuf>,
        /// Use fictional data and make no network requests.
        #[arg(long)]
        demo: bool,
    },
    /// Sign in using a separate Microsoft browser or device authorization flow.
    Login {
        #[arg(long)]
        config: PathBuf,
        /// Open the existing Teams Linux authentication window.
        #[arg(long)]
        webview_helper: Option<PathBuf>,
        /// Allow sign-in before choosing a specific chat.
        #[arg(long)]
        bootstrap_chat: bool,
    },
    /// Interactively select one native Teams chat for read-only access.
    ChooseChat {
        #[arg(long)]
        config: PathBuf,
        /// List conversations without changing the allowlist.
        #[arg(long)]
        list: bool,
    },
    /// Remove this application's stored refresh token.
    Logout {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("{}", locale::text(error.message_key()));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Serve { config, demo } => {
            let (config, source): (Config, Arc<dyn MessageSource>) = if demo {
                (demo_config(), Arc::new(DemoSource))
            } else {
                let config = Config::load(config.as_deref().ok_or(AppError::InvalidConfig)?)?;
                let auth = Arc::new(MicrosoftAuth::new(
                    config.clone(),
                    Arc::new(SystemSecretStore),
                )?);
                let source: Arc<dyn MessageSource> = match config.backend {
                    Backend::Graph => Arc::new(GraphSource::new(auth)?),
                    Backend::NativeTeams => Arc::new(NativeSource::new(auth)?),
                };
                (config, source)
            };
            let server = TeamsMcpServer::new(TeamsService::new(config, source));
            server
                .serve(stdio())
                .await
                .map_err(|_| AppError::Protocol)?
                .waiting()
                .await
                .map_err(|_| AppError::Protocol)?;
            Ok(())
        }
        Commands::Login {
            config,
            webview_helper,
            bootstrap_chat,
        } => {
            let config = Config::load(&config)?;
            let auth = MicrosoftAuth::new(config, Arc::new(SystemSecretStore))?;
            if let Some(helper) = webview_helper {
                auth.browser_login(&helper, bootstrap_chat).await?;
            } else {
                auth.device_login(bootstrap_chat).await?;
            }
            println!(
                "Sign-in completed. The refresh token is stored in the system credential store."
            );
            Ok(())
        }
        Commands::Logout { config } => {
            let config = Config::load(&config)?;
            let auth = MicrosoftAuth::new(config, Arc::new(SystemSecretStore))?;
            auth.logout().await?;
            println!("The stored Teams MCP session was removed.");
            Ok(())
        }
        Commands::ChooseChat { config, list } => choose_chat(&config, list).await,
    }
}

async fn choose_chat(path: &Path, list_only: bool) -> Result<()> {
    let original = std::fs::read_to_string(path).map_err(|_| AppError::InvalidConfig)?;
    let config = Config::load(path)?;
    if config.backend != Backend::NativeTeams {
        return Err(AppError::UnsupportedOperation);
    }
    let auth = Arc::new(MicrosoftAuth::new(
        config.clone(),
        Arc::new(SystemSecretStore),
    )?);
    let source = Arc::new(NativeSource::new(auth)?);
    let chats = source.discover_chats().await?;
    if chats.is_empty() {
        println!("No chats were found in the first page.");
        return Ok(());
    }
    println!("No.  Conversation | Last sender | Last activity | Access");
    for (index, chat) in chats.iter().enumerate() {
        let sender = if chat.last_sender.is_empty() {
            "-"
        } else {
            &chat.last_sender
        };
        let activity = if chat.last_activity.is_empty() {
            "-"
        } else {
            &chat.last_activity
        };
        let access = if config.allows_chat(&chat.id) {
            "authorized"
        } else {
            "-"
        };
        println!(
            "{:>3}  {} | {sender} | {activity} | {access}",
            index + 1,
            chat.label
        );
    }
    if list_only {
        return Ok(());
    }
    let index = loop {
        print!("Select a chat number to authorize (Enter to cancel): ");
        std::io::stdout()
            .flush()
            .map_err(|_| AppError::SelectionUnavailable)?;
        let mut input = String::new();
        if std::io::stdin()
            .read_line(&mut input)
            .map_err(|_| AppError::SelectionUnavailable)?
            == 0
            || input.trim().is_empty()
        {
            println!("Chat selection cancelled; no configuration was changed.");
            return Ok(());
        }
        if let Some(index) = selected_chat_index(&input, chats.len()) {
            break index;
        }
        println!(
            "Enter a number from 1 to {}, or press Enter to cancel.",
            chats.len()
        );
    };
    let selected = &chats[index];
    let config = Config::load(path)?;
    if std::fs::read_to_string(path).map_err(|_| AppError::InvalidConfig)? != original {
        return Err(AppError::ConfigChanged);
    }
    if !config.allows_chat(&selected.id) {
        let updated = format!(
            "{original}\n[[chats]]\nchat_id = {}\nlabel = {}\n",
            toml::Value::String(selected.id.clone()),
            toml::Value::String(selected.label.clone())
        );
        let parsed: Config = toml::from_str(&updated).map_err(|_| AppError::InvalidConfig)?;
        parsed.validate()?;
        write_private_config(path, &original, &updated)?;
    }
    println!("Selected chat is authorized. Testing read-only access...");
    let config = Config::load(path)?;
    let service = TeamsService::new(config, source);
    let page = service
        .messages(MessagesRequest {
            target: Target::Chat {
                chat_id: selected.id.clone(),
            },
            limit: Some(10),
            cursor: None,
        })
        .await?;
    println!(
        "Read succeeded: {} messages returned. Message content was not printed.",
        page.messages.len()
    );
    Ok(())
}

fn selected_chat_index(input: &str, count: usize) -> Option<usize> {
    input
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=count).contains(n))
        .map(|n| n - 1)
}

#[cfg(target_os = "linux")]
fn write_private_config(path: &Path, original: &str, updated: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temp = path.with_extension(format!("teams-mcp-{:016x}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| AppError::InvalidConfig)?;
        file.write_all(updated.as_bytes())
            .map_err(|_| AppError::InvalidConfig)?;
        file.sync_all().map_err(|_| AppError::InvalidConfig)?;
        if std::fs::read_to_string(path).map_err(|_| AppError::InvalidConfig)? != original {
            return Err(AppError::ConfigChanged);
        }
        std::fs::rename(&temp, path).map_err(|_| AppError::InvalidConfig)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn write_private_config(_path: &Path, _original: &str, _updated: &str) -> Result<()> {
    Err(AppError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_selection_rejects_invalid_numbers() {
        assert_eq!(selected_chat_index(" 1\n", 3), Some(0));
        assert_eq!(selected_chat_index("3", 3), Some(2));
        for value in ["", "0", "4", "-1", "1 2", "abc"] {
            assert_eq!(selected_chat_index(value, 3), None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn configuration_updates_are_private_and_reject_stale_input() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "teams-mcp-config-test-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("fixture.toml");
        std::fs::write(&path, "fixture before").unwrap();
        write_private_config(&path, "fixture before", "fixture after").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fixture after");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(matches!(
            write_private_config(&path, "fixture before", "stale"),
            Err(AppError::ConfigChanged)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fixture after");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
