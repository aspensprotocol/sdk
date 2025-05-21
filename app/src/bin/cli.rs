use alloy_chains::NamedChain;
use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::str::FromStr;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

use aspens::commands::config::{call_get_config, download_config_to_file};
use aspens::commands::trading::{balance, deposit, send_order, withdraw};

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The URL of the arborter server
    #[arg(short, long, default_value_t = Url::parse("http://localhost:50051").unwrap())]
    url: Url,

    #[command(flatten)]
    verbose: clap_verbosity::Verbosity,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Parser)]
enum Commands {
    /// Initialize a new trading session
    Initialize,
    /// Config: Fetch the current configuration from the arborter server
    GetConfig,
    /// Download configuration to a file
    DownloadConfig {
        /// Path to save the configuration file
        #[arg(short, long)]
        path: String,
    },
    /// Deposit token(s) to make them available for trading
    Deposit {
        /// The chain network to deposit to
        chain: String,
        token: String,
        amount: u64,
    },
    /// Withdraw token(s) to a local wallet
    Withdraw {
        /// The chain network to withdraw from
        chain: String,
        token: String,
        amount: u64,
    },
    /// Send a BUY order
    Buy {
        /// Amount to buy
        amount: u64,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<u64>,
    },
    /// Send a SELL order
    Sell {
        /// Amount to sell
        amount: u64,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<u64>,
    },
    /// Get a list of all active orders
    GetOrders,
    /// Cancel an order
    CancelOrder {
        /// Order ID to cancel
        order_id: u64,
    },
    /// Fetch the balances
    Balance,
    /// Fetch the latest top of book
    GetOrderbook {
        /// Market ID to fetch orderbook for
        market_id: String,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum BaseOrQuote {
    Base,
    Quote,
}

impl std::fmt::Display for BaseOrQuote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BaseOrQuote::Base => write!(f, "base"),
            BaseOrQuote::Quote => write!(f, "quote"),
        }
    }
}

// Helper function to parse chain string into NamedChain
fn parse_chain(chain_str: &str) -> Result<NamedChain> {
    NamedChain::from_str(chain_str).with_context(|| {
        format!("Invalid chain name: {chain_str}. Valid chains are: base-goerli or base-sepolia")
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables
    dotenv::from_filename(".env.anvil.local").ok();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    let cli = Cli::parse();

    match cli.command {
        Commands::Initialize => {
            info!("Initialized session at {}", cli.url);
            info!("Available config for {} is <TODO!!>", cli.url);
        }
        Commands::GetConfig => {
            info!("Fetching config...");
            let config = call_get_config(cli.url.to_string()).await?;
            info!("Configuration: {:#?}", config);
        }
        Commands::Deposit {
            chain,
            token,
            amount,
        } => {
            let chain = parse_chain(&chain)?;
            info!("Depositing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let rpc_url = match chain {
                NamedChain::BaseGoerli => base_chain_rpc_url,
                NamedChain::BaseSepolia => quote_chain_rpc_url,
                _ => unreachable!(),
            };

            let token_address = match chain {
                NamedChain::BaseGoerli => base_chain_usdc_token_address,
                NamedChain::BaseSepolia => quote_chain_usdc_token_address,
                _ => unreachable!(),
            };

            deposit::call_deposit(chain, &rpc_url, &token_address, amount).await?
        }
        Commands::Withdraw {
            chain,
            token,
            amount,
        } => {
            let chain = parse_chain(&chain)?;
            info!("Withdrawing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let rpc_url = match chain {
                NamedChain::BaseGoerli => base_chain_rpc_url,
                NamedChain::BaseSepolia => quote_chain_rpc_url,
                _ => unreachable!(),
            };

            let token_address = match chain {
                NamedChain::BaseGoerli => base_chain_usdc_token_address,
                NamedChain::BaseSepolia => quote_chain_usdc_token_address,
                _ => unreachable!(),
            };

            withdraw::call_withdraw(chain, &rpc_url, &token_address, amount).await?
        }
        Commands::Buy {
            amount,
            limit_price,
        } => {
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?}");
            send_order::call_send_order(cli.url.to_string(), 1, amount, limit_price).await?;
            info!("Order sent");
        }
        Commands::Sell {
            amount,
            limit_price,
        } => {
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");
            send_order::call_send_order(cli.url.to_string(), 2, amount, limit_price).await?;
            info!("Order sent");
        }
        Commands::GetOrders => {
            info!("Getting orders...");
            info!("TODO: Implement this");
        }
        Commands::CancelOrder { order_id } => {
            info!("Order canceled: {order_id:?}");
            info!("TODO: Implement this");
        }
        Commands::Balance => {
            info!("Getting balance");
            balance::balance(&[]).await?;
        }
        Commands::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
        Commands::DownloadConfig { path } => {
            download_config_to_file(cli.url.to_string(), path).await?;
        }
    }

    Ok(())
}
