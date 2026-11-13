use std::{path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use teams_mcp_rs::{
    auth::{MicrosoftAuth, SystemSecretStore},
    backend::{MessageSource, TeamsService},
    config::Config,
    demo::{DemoSource, demo_config},
    error::{AppError, Result},
    graph::GraphSource,
    locale,
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
    /// Sign in using a separate Microsoft device authorization flow.
    Login {
        #[arg(long)]
        config: PathBuf,
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
                let source = Arc::new(GraphSource::new(auth)?);
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
        Commands::Login { config } => {
            let config = Config::load(&config)?;
            let auth = MicrosoftAuth::new(config, Arc::new(SystemSecretStore))?;
            auth.login().await?;
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
    }
}
