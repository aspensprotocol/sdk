use std::path::PathBuf;

use clap::{Parser, Subcommand};

use aspens_mcp::config;

const DEFAULT_CONFIG: &str = "~/.aspens-mcp/aspens-mcp.toml";

#[derive(Parser)]
#[command(name = "aspens-mcp", about = "MCP server over the Aspens SDK")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Path to the config file (used when no subcommand is given).
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server on stdio (the default).
    Serve {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    /// Lift a halt.
    Resume {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
}

fn serve(path: &std::path::Path) -> eyre::Result<()> {
    let cfg = config::load(path)?;
    let keys = config::startup_keys(&cfg)?;
    tracing::info!(evm = keys.evm, solana = keys.solana, "config loaded");
    Ok(())
}

fn main() -> eyre::Result<()> {
    // Logs go to stderr only: stdout carries nothing but JSON-RPC frames.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        None => serve(&config::expand_tilde(&cli.config)),
        Some(Command::Serve { config: c }) => serve(&config::expand_tilde(&c)),
        Some(Command::Resume { config: c }) => {
            // Parsed only; lifting a halt is implemented with the ledger.
            let _ = config::expand_tilde(&c);
            Ok(())
        }
    }
}
