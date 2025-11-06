use eyre::Result;
use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use clap::{Parser, ValueEnum};
use std::str::FromStr;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The URL of the arborter server
    #[arg(short, long)]
    url: Option<Url>,

    /// Environment configuration to use
    #[arg(short, long, default_value = "anvil")]
    env: String,

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
    #[cfg(feature = "admin")]
    GetConfig,
    /// Download configuration to a file
    #[cfg(feature = "admin")]
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
        amount: String,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
    },
    /// Send a SELL order
    Sell {
        /// Amount to sell
        amount: String,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global subscriber");

    // Build the client
    let mut builder = AspensClient::builder().with_environment(&cli.env);

    if let Some(url) = cli.url {
        builder = builder.with_url(url.to_string())?;
    }

    let client = builder.build()?;
    let executor = DirectExecutor;

    match cli.command {
        Commands::Initialize => {
            info!("Initialized session at {}", client.url());
            info!("Available config for {} is <TODO!!>", client.url());
        }
        #[cfg(feature = "admin")]
        Commands::GetConfig => {
            use aspens::commands::config;
            let result = executor.execute(config::get_config(client.url().to_string()));
            info!("GetConfig result: {result:?}");
        }
        Commands::Deposit {
            chain,
            token,
            amount,
        } => {
            info!("Depositing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = client.get_env("BASE_CHAIN_RPC_URL").unwrap().clone();
            let base_chain_contract_address = client
                .get_env("BASE_CHAIN_CONTRACT_ADDRESS")
                .unwrap()
                .clone();
            let base_chain_usdc_token_address = client
                .get_env("BASE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap()
                .clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let chain = alloy_chains::NamedChain::from_str(&chain)?;
            let result = executor.execute(deposit::call_deposit(
                chain,
                base_chain_rpc_url,
                base_chain_usdc_token_address,
                base_chain_contract_address,
                privkey,
                amount,
            ))?;
            info!("Deposit result: {result:?}");
        }
        Commands::Withdraw {
            chain,
            token,
            amount,
        } => {
            info!("Withdrawing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = client.get_env("BASE_CHAIN_RPC_URL").unwrap().clone();
            let base_chain_contract_address = client
                .get_env("BASE_CHAIN_CONTRACT_ADDRESS")
                .unwrap()
                .clone();
            let base_chain_usdc_token_address = client
                .get_env("BASE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap()
                .clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let chain = alloy_chains::NamedChain::from_str(&chain)?;
            let result = executor.execute(withdraw::call_withdraw(
                chain,
                base_chain_rpc_url,
                base_chain_usdc_token_address,
                base_chain_contract_address,
                privkey,
                amount,
            ));
            info!("Withdraw result: {result:?}");
        }
        Commands::Buy {
            amount,
            limit_price,
        } => {
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?}");
            let market_id = client.get_env("MARKET_ID_1").unwrap().clone();
            let pubkey = client.get_env("EVM_TESTNET_PUBKEY").unwrap().clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order(
                client.url().to_string(),
                1, // Buy side
                amount,
                limit_price,
                market_id,
                pubkey.clone(),
                pubkey,
                privkey,
            ))?;
            info!("SendOrder result: {result:?}");

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
            }

            info!("Order sent");
        }
        Commands::Sell {
            amount,
            limit_price,
        } => {
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");
            let market_id = client.get_env("MARKET_ID_1").unwrap().clone();
            let pubkey = client.get_env("EVM_TESTNET_PUBKEY").unwrap().clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order(
                client.url().to_string(),
                2, // Sell side
                amount,
                limit_price,
                market_id,
                pubkey.clone(),
                pubkey,
                privkey,
            ))?;
            info!("SendOrder result: {result:?}");

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
            }

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
            let base_chain_rpc_url = client.get_env("BASE_CHAIN_RPC_URL").unwrap().clone();
            let base_chain_usdc_token_address = client
                .get_env("BASE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap()
                .clone();
            let quote_chain_rpc_url = client.get_env("QUOTE_CHAIN_RPC_URL").unwrap().clone();
            let quote_chain_usdc_token_address = client
                .get_env("QUOTE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap()
                .clone();
            let base_chain_contract_address = client
                .get_env("BASE_CHAIN_CONTRACT_ADDRESS")
                .unwrap()
                .clone();
            let quote_chain_contract_address = client
                .get_env("QUOTE_CHAIN_CONTRACT_ADDRESS")
                .unwrap()
                .clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let result = executor.execute(balance::balance(
                base_chain_rpc_url,
                base_chain_usdc_token_address,
                quote_chain_rpc_url,
                quote_chain_usdc_token_address,
                base_chain_contract_address,
                quote_chain_contract_address,
                privkey,
            ))?;
            info!("Balance result: {result:?}");
        }
        Commands::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
        #[cfg(feature = "admin")]
        Commands::DownloadConfig { path } => {
            use aspens::commands::config;
            let result =
                executor.execute(config::download_config(client.url().to_string(), path));
            info!("DownloadConfig result: {result:?}");
        }
    }

    Ok(())
}
