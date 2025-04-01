use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use anyhow::Result;
use clap::{Parser, ValueEnum};
use tracing::info;
use url::Url;

use aspens::commands::config::{add_market, add_token, deploy_contract, get_config};
use aspens::commands::trading::{balance, deposit, send_order, withdraw};

const BASE_SEPOLIA_RPC_URL: &str = "http://localhost:8545";
const BASE_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "036CbD53842c5426634e7929541eC2318f3dCF7e";
const OP_SEPOLIA_RPC_URL: &str = "http://localhost:8546";
const OP_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "5fd84259d66Cd46123540766Be93DFE6D43130D7";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The URL of the arborter server
    #[arg(short, long, default_value_t = Url::parse("http://localhost:50051").unwrap())]
    url: Url,

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
        chain_network: SupportedChain,
    },
    /// Deploy the trade contract onto the given chain
    DeployContract {
        /// The chain network to deploy the contract to
        chain_network: SupportedChain,
        base_or_quote: BaseOrQuote,
    },
    /// Deposit token(s) to make them available for trading
    Deposit {
        chain: SupportedChain,
        token: String,
        amount: u64,
    },
    /// Withdraw token(s) to a local wallet
    Withdraw {
        chain: SupportedChain,
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
    GetBalance,
    /// Fetch the latest top of book
    GetOrderbook {
        /// Market ID to fetch orderbook for
        market_id: String,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum SupportedChain {
    /// Base Sepolia (testnet)
    BaseSepolia,
    /// Optimism Sepolia (testnet)
    OptimismSepolia,
}

impl std::fmt::Display for SupportedChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SupportedChain::BaseSepolia => write!(f, "base-sepolia"),
            SupportedChain::OptimismSepolia => write!(f, "optimism-sepolia"),
        }
    }
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Load environment variables
    dotenv::dotenv().ok();

    let cli = Cli::parse();

    match cli.command {
        Commands::Initialize => {
            info!("Initialized session at {}", cli.url);
            info!("Available config for {} is <TODO!!>", cli.url);
        }
        Commands::GetConfig => {
            info!("Fetching config...");
            let result = get_config::call_get_config(cli.url.to_string()).await?;
            info!("GetConfig result: {result:?}");
        }
        Commands::AddMarket => {
            info!("Adding market...");
            let result = add_market::call_add_market(cli.url.to_string()).await?;
            info!("AddMarket result: {result:?}");
        }
        Commands::AddToken { chain_network } => {
            info!("Adding token ___ on {chain_network:?}");
            let result =
                add_token::call_add_token(cli.url.to_string(), &chain_network.to_string()).await?;
            info!("AddToken result: {result:?}");
        }
        Commands::DeployContract {
            chain_network,
            base_or_quote,
        } => {
            info!("Deploying contract on {chain_network:?}");
            let result = deploy_contract::call_deploy_contract(
                cli.url.to_string(),
                &chain_network.to_string(),
                &base_or_quote.to_string(),
            )
            .await?;
            info!("DeployContract result: {result:?}");
        }
        Commands::Deposit {
            chain,
            token,
            amount,
        } => {
            info!("Depositing {amount:?} {token:?} on {chain:?}");
            let named_chain = match chain {
                SupportedChain::OptimismSepolia => NamedChain::OptimismSepolia,
                SupportedChain::BaseSepolia => NamedChain::BaseSepolia,
            };

            let rpc_url = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_RPC_URL,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_RPC_URL,
            };

            let token_address = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            };

            let result = deposit::call_deposit(named_chain, rpc_url, token_address, amount).await?;
            info!("Deposit result: {result:?}");
        }
        Commands::Withdraw {
            chain,
            token,
            amount,
        } => {
            info!("Withdrawing {amount:?} {token:?} on {chain:?}");
            let named_chain = match chain {
                SupportedChain::OptimismSepolia => NamedChain::OptimismSepolia,
                SupportedChain::BaseSepolia => NamedChain::BaseSepolia,
            };

            let rpc_url = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_RPC_URL,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_RPC_URL,
            };

            let token_address = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            };

            let result =
                withdraw::call_withdraw(named_chain, rpc_url, token_address, amount).await?;
            info!("Withdraw result: {result:?}");
        }
        Commands::Buy {
            amount,
            limit_price,
        } => {
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?}");
            let result =
                send_order::call_send_order(cli.url.to_string(), 1, amount, Some(limit_price))
                    .await?;
            info!("SendOrder result: {result:?}");
            info!("Order sent");
        }
        Commands::Sell {
            amount,
            limit_price,
        } => {
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");
            let result =
                send_order::call_send_order(cli.url.to_string(), 2, amount, Some(limit_price))
                    .await?;
            info!("SendOrder result: {result:?}");
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
        Commands::GetBalance => {
            let error_val = Uint::from(99999);
            let op_wallet_balance = balance::call_get_erc20_balance(
                NamedChain::OptimismSepolia,
                OP_SEPOLIA_RPC_URL,
                OP_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let op_available_balance = balance::call_get_balance(
                NamedChain::OptimismSepolia,
                OP_SEPOLIA_RPC_URL,
                OP_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let op_locked_balance = balance::call_get_locked_balance(
                NamedChain::OptimismSepolia,
                OP_SEPOLIA_RPC_URL,
                OP_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let base_wallet_balance = balance::call_get_erc20_balance(
                NamedChain::BaseSepolia,
                BASE_SEPOLIA_RPC_URL,
                BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let base_available_balance = balance::call_get_balance(
                NamedChain::BaseSepolia,
                BASE_SEPOLIA_RPC_URL,
                BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let base_locked_balance = balance::call_get_locked_balance(
                NamedChain::BaseSepolia,
                BASE_SEPOLIA_RPC_URL,
                BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            )
            .await
            .unwrap_or(error_val);

            let balance_table = balance::balance_table(
                vec!["USDC", "Base Sepolia", "Optimism Sepolia"],
                base_wallet_balance,
                base_available_balance,
                base_locked_balance,
                op_wallet_balance,
                op_available_balance,
                op_locked_balance,
            );
            if op_wallet_balance.eq(&error_val)
                | op_available_balance.eq(&error_val)
                | op_locked_balance.eq(&error_val)
                | base_wallet_balance.eq(&error_val)
                | base_available_balance.eq(&error_val)
                | base_locked_balance.eq(&error_val)
            {
                info!("** A '99999' value represents an error in fetching the actual value");
            }

            info!("{balance_table}");
        }
        Commands::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
    }

    Ok(())
}
