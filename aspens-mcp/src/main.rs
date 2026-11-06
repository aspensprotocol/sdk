use std::path::PathBuf;

use clap::{Parser, Subcommand};

use aspens_mcp::config;
use aspens_mcp::ledger::{Ledger, Record};
use aspens_mcp::server::Server;
use aspens_mcp::venue::{SdkVenue, Venue};
use rmcp::ServiceExt;

const DEFAULT_CONFIG: &str = "~/.aspens-mcp/aspens-mcp.toml";

/// How long `serve` waits, once the client has gone, for sends still in
/// flight to record their outcomes. Above the SDK's 60 s gRPC timeout.
const SEND_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(70);

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

async fn serve(path: &std::path::Path) -> eyre::Result<()> {
    let cfg = config::load(path)?;
    let keys = config::startup_keys(&cfg)?;
    tracing::info!(evm = keys.evm, solana = keys.solana, "config loaded");
    let venue = SdkVenue::new(&cfg)?;
    let venue_cfg = venue.config().await?;
    let markets = config::resolve_markets(&cfg, &venue_cfg)?;
    config::validate_deposit_caps(&cfg, &venue_cfg)?;
    let ledger = Ledger::open(&cfg.state_dir)?;
    let server = Server::new(venue, cfg, markets, ledger);
    let in_flight = server.in_flight();
    let served = server
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await;
    // The client is gone (stdin closed, or the session ended). A send still
    // running owns the ledger between its usage and outcome records: let it
    // finish rather than exit under it.
    if in_flight.count() > 0 {
        tracing::info!(sends = in_flight.count(), "waiting for in-flight sends");
    }
    if !in_flight.wait_idle(SEND_DRAIN_TIMEOUT).await {
        tracing::error!(
            sends = in_flight.count(),
            "exiting with sends still in flight after {SEND_DRAIN_TIMEOUT:?}; their usage is \
             recorded, and on restart they show as unknown"
        );
    }
    served?;
    Ok(())
}

/// Lift a halt: append `Resume` to the ledger if it is halted. Needs no venue.
/// Refuses while a running server holds the ledger.
fn resume(path: &std::path::Path) -> eyre::Result<()> {
    let cfg = config::load(path)?;
    let mut ledger = Ledger::open(&cfg.state_dir).map_err(|e| {
        let msg = format!("{e:#}");
        if msg.starts_with("another aspens-mcp is using") {
            eyre::eyre!("{msg}: stop the running aspens-mcp server first, then run resume again")
        } else {
            e
        }
    })?;
    let Some(reason) = ledger.halted() else {
        eprintln!("aspens-mcp: not halted; nothing to resume");
        return Ok(());
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    ledger.append(Record::Resume { ts })?;
    eprintln!(
        "aspens-mcp: resumed trading (was halted: {})",
        reason.as_deref().unwrap_or("no reason recorded")
    );
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
        None | Some(Command::Serve) => tokio::runtime::Runtime::new()?.block_on(serve(&path)),
        Some(Command::Resume) => resume(&path),
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
