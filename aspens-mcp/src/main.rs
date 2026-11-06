use std::path::PathBuf;

use clap::{Parser, Subcommand};

use aspens_mcp::config;

const DEFAULT_CONFIG: &str = "~/.aspens-mcp/aspens-mcp.toml";

#[derive(Parser)]
#[command(name = "aspens-mcp", about = "MCP server over the Aspens SDK")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Path to the config file (before or after the subcommand).
    #[arg(long, global = true, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server on stdio (the default).
    Serve,
    /// Lift a halt.
    Resume,
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let path = config::expand_tilde(&cli.config);
    match cli.command {
        None | Some(Command::Serve) => serve(&path),
        // Parsed only; lifting a halt is implemented with the ledger.
        Some(Command::Resume) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> (PathBuf, bool) {
        let mut v = vec!["aspens-mcp"];
        v.extend_from_slice(args);
        let cli = Cli::try_parse_from(v).unwrap();
        (cli.config, matches!(cli.command, Some(Command::Resume)))
    }

    #[test]
    fn config_honoured_in_every_position() {
        let p = PathBuf::from("/x/p.toml");
        assert_eq!(cfg(&["--config", "/x/p.toml"]).0, p);
        assert_eq!(cfg(&["--config", "/x/p.toml", "serve"]).0, p);
        assert_eq!(cfg(&["serve", "--config", "/x/p.toml"]).0, p);
        let (c, resume) = cfg(&["resume", "--config", "/x/p.toml"]);
        assert_eq!(c, p);
        assert!(resume);
        assert_eq!(cfg(&["serve"]).0, PathBuf::from(DEFAULT_CONFIG));
    }
}
