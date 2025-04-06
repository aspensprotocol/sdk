use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::str::FromStr;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use url::Url;

use aspens::commands::config::{
    add_market, add_token, call_get_config, deploy_contract, download_config_to_file,
};
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
    /// Config: Add a new market to the arborter service
    AddMarket,
    /// Config: Add a new token to the arborter service
    AddToken {
        /// The chain network to add the token to
        chain: String,
    },
    /// Deploy the trade contract onto the given chain
    DeployContract {
        /// The chain network to deploy the contract to
        chain: String,
        base_or_quote: BaseOrQuote,
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
        limit_price: u64,
    },
    /// Send a SELL order
    Sell {
        /// Amount to sell
        amount: u64,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: u64,
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
    /// Download configuration to a file
    DownloadConfig {
        /// Path to save the configuration file
        #[arg(short, long)]
        path: String,
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

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stdout)
        .init();

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
        Commands::AddMarket => {
            info!("Adding market...");
            add_market::call_add_market(cli.url.to_string()).await?
        }
        Commands::AddToken { chain } => {
            let chain = parse_chain(&chain)?;
            info!("Adding token ___ on {chain:?}");
            add_token::call_add_token(cli.url.to_string(), chain.as_ref()).await?
        }
        Commands::DeployContract {
            chain,
            base_or_quote,
        } => {
            let chain = parse_chain(&chain)?;
            info!("Deploying contract on {chain:?}");
            deploy_contract::call_deploy_contract(
                cli.url.to_string(),
                chain.as_ref(),
                &base_or_quote.to_string(),
            )
            .await?
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
            send_order::call_send_order(cli.url.to_string(), 1, amount, Some(limit_price)).await?;
            info!("Order sent");
        }
        Commands::Sell {
            amount,
            limit_price,
        } => {
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");
            send_order::call_send_order(cli.url.to_string(), 2, amount, Some(limit_price)).await?;
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
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let error_val = Uint::from(99999);
            let base_wallet_balance = match balance::call_get_erc20_balance(
                NamedChain::BaseGoerli,
                &base_chain_rpc_url,
                &base_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!("Failed to get balance for {base_chain_usdc_token_address}: {e}");
                    error_val
                }
            };

            let base_available_balance = match balance::call_get_balance(
                NamedChain::BaseGoerli,
                &base_chain_rpc_url,
                &base_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!(
                        "Failed to get available balance for {base_chain_usdc_token_address}: {e}"
                    );
                    error_val
                }
            };

            let base_locked_balance = match balance::call_get_locked_balance(
                NamedChain::BaseGoerli,
                &base_chain_rpc_url,
                &base_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!("Failed to get locked balance for {base_chain_usdc_token_address}: {e}");
                    error_val
                }
            };

            let quote_wallet_balance = match balance::call_get_erc20_balance(
                NamedChain::BaseSepolia,
                &quote_chain_rpc_url,
                &quote_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!("Failed to get balance for {quote_chain_usdc_token_address}: {e}");
                    error_val
                }
            };

            let quote_available_balance = match balance::call_get_balance(
                NamedChain::BaseSepolia,
                &quote_chain_rpc_url,
                &quote_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!(
                        "Failed to get available balance for {quote_chain_usdc_token_address}: {e}"
                    );
                    error_val
                }
            };

            let quote_locked_balance = match balance::call_get_locked_balance(
                NamedChain::BaseSepolia,
                &quote_chain_rpc_url,
                &quote_chain_usdc_token_address,
            )
            .await
            {
                Ok(balance) => balance,
                Err(e) => {
                    error!(
                        "Failed to get locked balance for {quote_chain_usdc_token_address}: {e}"
                    );
                    error_val
                }
            };

            let balance_table = balance::balance_table(
                vec!["USDC", "Base Chain", "Quote Chain"],
                base_wallet_balance,
                base_available_balance,
                base_locked_balance,
                quote_wallet_balance,
                quote_available_balance,
                quote_locked_balance,
            );
            if base_wallet_balance.eq(&error_val)
                | base_available_balance.eq(&error_val)
                | base_locked_balance.eq(&error_val)
                | quote_wallet_balance.eq(&error_val)
                | quote_available_balance.eq(&error_val)
                | quote_locked_balance.eq(&error_val)
            {
                info!("** A '99999' value represents an error in fetching the actual value");
            }

            info!("\n{balance_table}");
        }
        Commands::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
        Commands::DownloadConfig { path } => {
            download_config_to_file(path).await?;
        }
    }

    Ok(())
}
