use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "riley-comments", about = "Comment service for rileyleff.com")]
struct Cli {
    /// Path to config TOML file
    #[arg(short, long, global = true, env = "RILEY_COMMENTS_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP server
    Serve,
    /// Run database migrations
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let config_path = cli
        .config
        .unwrap_or_else(|| PathBuf::from("/etc/riley_comments/config.toml"));

    let config = riley_comments_core::config::load_config(&config_path)?;
    let pool = riley_comments_core::db::connect(&config.database).await?;

    match cli.command {
        Command::Serve => {
            tracing::info!("running migrations");
            riley_comments_core::db::migrate(&pool).await?;
            riley_comments_api::serve(config, pool).await?;
        }
        Command::Migrate => {
            tracing::info!("running migrations");
            riley_comments_core::db::migrate(&pool).await?;
            tracing::info!("migrations complete");
        }
    }

    Ok(())
}
