use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use clap::{Parser, ValueEnum};
use eyre::Result;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The Aspens stack URL
    #[arg(short = 's', long = "stack")]
    stack_url: Option<Url>,

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
    /// Fetch and display the configuration from the server
    Config {
        /// Optional path to save the configuration file (supports .json or .toml)
        #[arg(short, long)]
        output_file: Option<String>,
    },
    /// Deposit tokens to make them available for trading (requires NETWORK TOKEN AMOUNT)
    Deposit {
        /// The network name to deposit to (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to deposit (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to deposit
        amount: u64,
    },
    /// Withdraw tokens to a local wallet (requires NETWORK TOKEN AMOUNT)
    Withdraw {
        /// The network name to withdraw from (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to withdraw (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to withdraw
        amount: u64,
    },
    /// Send a BUY order with amount and optional limit price
    Buy {
        /// Amount to buy
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
        /// Market ID to trade on (defaults to MARKET_ID_1 from environment)
        #[arg(short, long)]
        market: Option<String>,
    },
    /// Send a SELL order with amount and optional limit price
    Sell {
        /// Amount to sell
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
        /// Market ID to trade on (defaults to MARKET_ID_1 from environment)
        #[arg(short, long)]
        market: Option<String>,
    },
    /// Fetch the current balances across all chains
    Balance,
    /// Show current configuration and connection status
    Status,
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

    // Configure log level based on verbosity flag
    let log_level = if cli.verbose.is_silent() {
        Level::ERROR
    } else {
        match cli.verbose.log_level_filter() {
            log::LevelFilter::Off => Level::ERROR,
            log::LevelFilter::Error => Level::ERROR,
            log::LevelFilter::Warn => Level::WARN,
            log::LevelFilter::Info => Level::INFO,
            log::LevelFilter::Debug => Level::DEBUG,
            log::LevelFilter::Trace => Level::TRACE,
        }
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    // Build the client
    let mut builder = AspensClient::builder().with_environment(&cli.env);

    if let Some(url) = cli.stack_url {
        builder = builder.with_url(url.to_string())?;
    }

    let client = builder.build()?;
    let executor = DirectExecutor;

    match cli.command {
        Commands::Deposit {
            network,
            token,
            amount,
        } => {
            info!("Depositing {amount} {token} on {network}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor.execute(aspens::commands::config::call_get_config(stack_url))?;
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            executor.execute(deposit::call_deposit_from_config(
                network, token, amount, privkey, config,
            ))?;
            info!("Deposit was successful");
        }
        Commands::Withdraw {
            network,
            token,
            amount,
        } => {
            info!("Withdrawing {amount} {token} from {network}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor.execute(aspens::commands::config::call_get_config(stack_url))?;
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            executor.execute(withdraw::call_withdraw_from_config(
                network, token, amount, privkey, config,
            ))?;
            info!("Withdraw was successful");
        }
        Commands::Buy {
            amount,
            limit_price,
            market,
        } => {
            let market_id =
                market.unwrap_or_else(|| client.get_env("MARKET_ID_1").unwrap().clone());
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?} on market {market_id}");
            let pubkey = client.get_env("EVM_TESTNET_PUBKEY").unwrap().clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order(
                client.stack_url().to_string(),
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
                info!("💡 Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }

            info!("✓ Buy order sent successfully");
        }
        Commands::Sell {
            amount,
            limit_price,
            market,
        } => {
            let market_id =
                market.unwrap_or_else(|| client.get_env("MARKET_ID_1").unwrap().clone());
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?} on market {market_id}");
            let pubkey = client.get_env("EVM_TESTNET_PUBKEY").unwrap().clone();
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order(
                client.stack_url().to_string(),
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
                info!("💡 Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }

            info!("✓ Sell order sent successfully");
        }
        Commands::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = client.stack_url().to_string();
            let config = executor.execute(config::get_config(stack_url))?;
            let privkey = client.get_env("EVM_TESTNET_PRIVKEY").unwrap().clone();

            executor.execute(balance::balance_from_config(config, privkey))?;
        }
        Commands::Status => {
            info!("Configuration Status:");
            info!("  Environment: {}", client.environment());
            info!("  Stack URL: {}", client.stack_url());
            info!(
                "  Market ID 1: {}",
                client
                    .get_env("MARKET_ID_1")
                    .unwrap_or(&"not set".to_string())
            );
            info!(
                "  Market ID 2: {}",
                client
                    .get_env("MARKET_ID_2")
                    .unwrap_or(&"not set".to_string())
            );
            info!(
                "  Base Chain RPC: {}",
                client
                    .get_env("BASE_CHAIN_RPC_URL")
                    .unwrap_or(&"not set".to_string())
            );
            info!(
                "  Quote Chain RPC: {}",
                client
                    .get_env("QUOTE_CHAIN_RPC_URL")
                    .unwrap_or(&"not set".to_string())
            );
            info!(
                "  Public Key: {}",
                client
                    .get_env("EVM_TESTNET_PUBKEY")
                    .unwrap_or(&"not set".to_string())
            );
        }
        Commands::Config { output_file } => {
            use aspens::commands::config;

            let stack_url = client.stack_url().to_string();
            info!("Fetching configuration from {stack_url}");
            let config = executor.execute(config::get_config(stack_url.clone()))?;

            // If output_file is provided, save to file
            if let Some(path) = output_file {
                executor.execute(config::download_config(stack_url.clone(), path.clone()))?;
                info!("Configuration saved to: {}", path);
            } else {
                // Display config as JSON
                let json = serde_json::to_string_pretty(&config)?;
                println!("{}", json);
            }
        }
    }

    Ok(())
}
