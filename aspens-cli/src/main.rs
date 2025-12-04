use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use clap::Parser;
use eyre::Result;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The Aspens stack URL (overrides ASPENS_MARKET_STACK_URL from .env)
    #[arg(short = 's', long = "stack")]
    stack_url: Option<Url>,

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
    /// Send a market BUY order (executes at best available price)
    BuyMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
    },
    /// Send a limit BUY order (executes at specified price or better)
    BuyLimit {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
        /// Limit price for the order
        price: String,
    },
    /// Send a market SELL order (executes at best available price)
    SellMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
    },
    /// Send a limit SELL order (executes at specified price or better)
    SellLimit {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
        /// Limit price for the order
        price: String,
    },
    /// Fetch the current balances for all supported tokens across all chains
    Balance,
    /// Show current configuration and connection status
    Status,
    /// Get the public key and address for the trader wallet
    TraderPublicKey,
    /// Get the signer public key(s) for the trading instance
    SignerPublicKey {
        /// Optional chain ID to filter by. If not provided, returns all chains.
        #[arg(long)]
        chain_id: Option<u32>,
    },
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
    let mut builder = AspensClient::builder();

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
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

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
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            executor.execute(withdraw::call_withdraw_from_config(
                network, token, amount, privkey, config,
            ))?;
            info!("Withdraw was successful");
        }
        Commands::BuyMarket { market, amount } => {
            info!("Sending market BUY order for {amount} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config =
                executor.execute(aspens::commands::config::call_get_config(stack_url.clone()))?;
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order_from_config(
                stack_url, market, 1, // Buy side
                amount, None, // No limit price (market order)
                privkey, config,
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

            info!("✓ Market buy order sent successfully");
        }
        Commands::BuyLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit BUY order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config =
                executor.execute(aspens::commands::config::call_get_config(stack_url.clone()))?;
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order_from_config(
                stack_url,
                market,
                1, // Buy side
                amount,
                Some(price),
                privkey,
                config,
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

            info!("✓ Limit buy order sent successfully");
        }
        Commands::SellMarket { market, amount } => {
            info!("Sending market SELL order for {amount} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config =
                executor.execute(aspens::commands::config::call_get_config(stack_url.clone()))?;
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order_from_config(
                stack_url, market, 2, // Sell side
                amount, None, // No limit price (market order)
                privkey, config,
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

            info!("✓ Market sell order sent successfully");
        }
        Commands::SellLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit SELL order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config =
                executor.execute(aspens::commands::config::call_get_config(stack_url.clone()))?;
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            let result = executor.execute(send_order::call_send_order_from_config(
                stack_url,
                market,
                2, // Sell side
                amount,
                Some(price),
                privkey,
                config,
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

            info!("✓ Limit sell order sent successfully");
        }
        Commands::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = client.stack_url().to_string();
            let config = executor.execute(config::get_config(stack_url))?;
            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();

            executor.execute(balance::balance_from_config(config, privkey))?;
        }
        Commands::Status => {
            info!("Configuration Status:");
            info!("  Stack URL: {}", client.stack_url());

            // Ping the gRPC server
            let ping_result =
                executor.execute(aspens::health::ping_grpc_server(client.stack_url().to_string()));
            if ping_result.success {
                info!(
                    "  Connection: OK ({}ms)",
                    ping_result.latency_ms.unwrap_or(0)
                );
            } else {
                info!(
                    "  Connection: FAILED - {}",
                    ping_result.error.unwrap_or_else(|| "Unknown error".to_string())
                );
            }
        }
        Commands::TraderPublicKey => {
            use alloy::signers::local::PrivateKeySigner;

            let privkey = client.get_env("TRADER_PRIVKEY").unwrap().clone();
            let signer = privkey.parse::<PrivateKeySigner>()?;
            let address = signer.address();
            let pubkey = signer.credential().verifying_key();

            println!("Trader Wallet:");
            println!("  Address:    {}", address);
            println!(
                "  Public Key: 0x{}",
                hex::encode(pubkey.to_encoded_point(false).as_bytes())
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
        Commands::SignerPublicKey { chain_id } => {
            use aspens::commands::config;

            let stack_url = client.stack_url().to_string();
            info!("Fetching signer public key(s) from {stack_url}");
            let response = executor.execute(config::get_signer_public_key(stack_url, chain_id))?;

            println!("Signer Public Keys:");
            for (id, key_info) in response.chain_keys.iter() {
                println!(
                    "  Chain {} ({}): {}",
                    id, key_info.chain_network, key_info.public_key
                );
            }
        }
    }

    Ok(())
}
