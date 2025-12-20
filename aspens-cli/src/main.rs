use aspens::commands::trading::{
    balance, cancel_order, deposit, send_order, stream_orderbook, stream_trades, withdraw,
};
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use clap::Parser;
use eyre::Result;
use std::process::ExitCode;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

/// Analyze an error and return a user-friendly message with hints
fn format_error(err: &eyre::Report, context: &str) -> String {
    let err_string = err.to_string().to_lowercase();
    let root_cause = err.root_cause().to_string().to_lowercase();

    // Helper to append the underlying error to the message
    let with_underlying_error =
        |msg: String| -> String { format!("{}\n\nUnderlying error: {}", msg, err) };

    // Connection errors
    if err_string.contains("failed to connect")
        || err_string.contains("connection refused")
        || root_cause.contains("connection refused")
    {
        return with_underlying_error(format!(
            "Failed to {}: Could not connect to the server\n\n\
             Possible causes:\n\
             - The Aspens server is not running\n\
             - The server URL is incorrect\n\
             - A firewall is blocking the connection\n\n\
             Hints:\n\
             - Check that the server is running\n\
             - Verify the stack URL with 'aspens-cli status'\n\
             - Check ASPENS_MARKET_STACK_URL in your .env file",
            context
        ));
    }

    // DNS/hostname resolution errors
    if err_string.contains("dns error")
        || err_string.contains("no such host")
        || err_string.contains("name or service not known")
        || root_cause.contains("dns")
    {
        return with_underlying_error(format!(
            "Failed to {}: Could not resolve server hostname\n\n\
             Possible causes:\n\
             - The server hostname is incorrect\n\
             - DNS is not configured properly\n\
             - No internet connection\n\n\
             Hints:\n\
             - Verify the stack URL is correct\n\
             - Check your internet connection\n\
             - Try using an IP address instead of hostname",
            context
        ));
    }

    // TLS/SSL errors
    if err_string.contains("tls")
        || err_string.contains("ssl")
        || err_string.contains("certificate")
        || root_cause.contains("certificate")
    {
        return with_underlying_error(format!(
            "Failed to {}: TLS/SSL error\n\n\
             Possible causes:\n\
             - The server's SSL certificate is invalid or expired\n\
             - Certificate chain is incomplete\n\
             - Using HTTP URL for HTTPS server or vice versa\n\n\
             Hints:\n\
             - Verify you're using the correct protocol (http:// vs https://)\n\
             - For local development, use http://localhost:50051\n\
             - For remote servers, use https://",
            context
        ));
    }

    // Protocol/compression errors (HTTP/HTTPS mismatch)
    if err_string.contains("compression flag")
        || err_string.contains("protocol error")
        || err_string.contains("invalid compression")
    {
        return with_underlying_error(format!(
            "Failed to {}: Protocol mismatch\n\n\
             Possible causes:\n\
             - Using HTTP to connect to an HTTPS server\n\
             - Using HTTPS to connect to an HTTP server\n\
             - The server is not a gRPC endpoint\n\n\
             Hints:\n\
             - For remote servers, use https://\n\
             - For local development, use http://\n\
             - Verify ASPENS_MARKET_STACK_URL in your .env file",
            context
        ));
    }

    // Timeout errors
    if err_string.contains("timeout") || err_string.contains("timed out") {
        return with_underlying_error(format!(
            "Failed to {}: Request timed out\n\n\
             Possible causes:\n\
             - The server is overloaded or unresponsive\n\
             - Network latency is too high\n\
             - The operation is taking longer than expected\n\n\
             Hints:\n\
             - Try again in a few moments\n\
             - Check server status with 'aspens-cli status'\n\
             - Verify network connectivity",
            context
        ));
    }

    // Chain/network not found
    if err_string.contains("chain not found")
        || err_string.contains("network not found")
        || (err_string.contains("not found") && err_string.contains("chain"))
    {
        return with_underlying_error(format!(
            "Failed to {}: Chain/network not found\n\n\
             Hints:\n\
             - Check available chains with 'aspens-cli config'\n\
             - Verify the network name is spelled correctly\n\
             - The chain may not be configured on this server",
            context
        ));
    }

    // Token not found
    if err_string.contains("token not found")
        || (err_string.contains("not found") && err_string.contains("token"))
    {
        return with_underlying_error(format!(
            "Failed to {}: Token not found\n\n\
             Hints:\n\
             - Check available tokens with 'aspens-cli config'\n\
             - Verify the token symbol is spelled correctly (case-sensitive)\n\
             - The token may not be configured on this chain",
            context
        ));
    }

    // Market not found
    if err_string.contains("market not found")
        || (err_string.contains("not found") && err_string.contains("market"))
    {
        return with_underlying_error(format!(
            "Failed to {}: Market not found\n\n\
             Hints:\n\
             - Check available markets with 'aspens-cli config'\n\
             - Verify the market ID is correct\n\
             - Markets are identified by their full ID (e.g., chain_id::token::chain_id::token)",
            context
        ));
    }

    // Insufficient gas (check before general insufficient balance)
    if err_string.contains("insufficient gas") {
        return with_underlying_error(format!(
            "Failed to {}: Insufficient gas for transaction fees\n\n\
             Your wallet needs native tokens (ETH, FLR, etc.) to pay for gas.\n\n\
             Hints:\n\
             - Fund your wallet with native tokens on the target chain\n\
             - For testnets, use a faucet to get free test tokens:\n\
               - Base Sepolia: https://www.alchemy.com/faucets/base-sepolia\n\
               - Flare Coston2: https://faucet.flare.network",
            context
        ));
    }

    // Insufficient token balance
    if err_string.contains("insufficient")
        || err_string.contains("not enough")
        || err_string.contains("balance too low")
    {
        return with_underlying_error(format!(
            "Failed to {}: Insufficient balance\n\n\
             Hints:\n\
             - Check your balances with 'aspens-cli balance'\n\
             - For trading: ensure you have deposited tokens first\n\
             - For deposits: ensure your wallet has enough tokens",
            context
        ));
    }

    // Invalid string length (typically from decimal/amount formatting issues)
    if err_string.contains("invalid string length") {
        return with_underlying_error(format!(
            "Failed to {}: Invalid amount format\n\n\
             The server rejected the order due to an invalid amount format.\n\n\
             Possible causes:\n\
             - Amount or price is too small or has too few digits\n\
             - Values need to be in the correct decimal format\n\n\
             Hints:\n\
             - Use decimal notation for amounts (e.g., '1.5' instead of '1')\n\
             - Check 'aspens-cli config' to see the market's pairDecimals setting\n\
             - For market with pairDecimals=4: '1' becomes '10000', '0.5' becomes '5000'",
            context
        ));
    }

    // Transaction/RPC errors
    if err_string.contains("transaction")
        || err_string.contains("revert")
        || err_string.contains("execution reverted")
    {
        return with_underlying_error(format!(
            "Failed to {}: Transaction failed\n\n\
             Possible causes:\n\
             - Insufficient token balance or allowance\n\
             - Contract execution reverted\n\
             - Gas estimation failed\n\n\
             Hints:\n\
             - Check your wallet balance\n\
             - Verify you have approved the contract to spend tokens\n\
             - Try with a smaller amount",
            context
        ));
    }

    // Private key errors
    if err_string.contains("invalid private key")
        || err_string.contains("privkey")
        || err_string.contains("secret key")
        || err_string.contains("hex decode")
    {
        return with_underlying_error(format!(
            "Failed to {}: Invalid private key\n\n\
             Hints:\n\
             - Ensure TRADER_PRIVKEY is set correctly in your .env file\n\
             - The private key should be a 64-character hex string\n\
             - Do not include the '0x' prefix",
            context
        ));
    }

    // Generic fallback with the original error
    format!(
        "Failed to {}\n\n\
         Hints:\n\
         - Check server status with 'aspens-cli status'\n\
         - Verify your configuration in .env file\n\
         - Use -v flag for more detailed output\n\n\
         Underlying error: {}",
        context, err
    )
}

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The Aspens stack URL (overrides ASPENS_MARKET_STACK_URL from .env)
    #[arg(short = 's', long = "stack", global = true)]
    stack_url: Option<Url>,

    /// Path to environment file (defaults to .env in current directory)
    #[arg(short = 'e', long = "env-file", global = true)]
    env_file: Option<String>,

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
    /// Cancel an existing order by its ID
    CancelOrder {
        /// Market ID the order is on
        market: String,
        /// Order side: "buy" or "sell"
        side: String,
        /// The internal order ID to cancel
        order_id: u64,
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
    /// Stream orderbook entries in real-time
    StreamOrderbook {
        /// Market ID to stream orders for
        market: String,
        /// Include historical open orders when stream starts
        #[arg(long, short = 'H')]
        historical: bool,
        /// Filter by a specific trader address
        #[arg(long, short = 't')]
        trader: Option<String>,
    },
    /// Stream executed trades in real-time
    StreamTrades {
        /// Market ID to stream trades for
        market: String,
        /// Include historical closed trades when stream starts
        #[arg(long, short = 'H')]
        historical: bool,
        /// Filter by a specific trader address
        #[arg(long, short = 't')]
        trader: Option<String>,
    },
    /// Get TEE attestation report from the signer
    GetAttestation {
        /// Optional hex-encoded data to bind to the attestation report (max 64 bytes)
        #[arg(long)]
        report_data: Option<String>,
        /// Output format: "text" (default) or "json"
        #[arg(long, short = 'o', default_value = "text")]
        output: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
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

    if let Some(ref env_file) = cli.env_file {
        builder = builder.with_env_file(env_file);
    }

    if let Some(ref url) = cli.stack_url {
        builder = builder.with_url(url.to_string())?;
    }

    let client = builder.build()?;
    let executor = DirectExecutor;

    // Helper to get trader private key with friendly error
    let get_trader_privkey = || -> Result<String> {
        client.get_env("TRADER_PRIVKEY").cloned().ok_or_else(|| {
            eyre::eyre!(
                "TRADER_PRIVKEY not found\n\n\
                 Hints:\n\
                 - Set TRADER_PRIVKEY in your .env file\n\
                 - The private key should be a 64-character hex string (without 0x prefix)\n\
                 - This should be the private key for your trading wallet"
            )
        })
    };

    match cli.command {
        Commands::Deposit {
            network,
            token,
            amount,
        } => {
            info!("Depositing {amount} {token} on {network}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            executor
                .execute(deposit::call_deposit_from_config(
                    network.clone(),
                    token.clone(),
                    amount,
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("deposit {} {} on {}", amount, token, network)
                    ))
                })?;

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
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            executor
                .execute(withdraw::call_withdraw_from_config(
                    network.clone(),
                    token.clone(),
                    amount,
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("withdraw {} {} from {}", amount, token, network)
                    ))
                })?;

            info!("Withdraw was successful");
        }
        Commands::BuyMarket { market, amount } => {
            info!("Sending market BUY order for {amount} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            let result = executor
                .execute(send_order::call_send_order_from_config(
                    stack_url,
                    market.clone(),
                    1, // Buy side
                    amount.clone(),
                    None, // No limit price (market order)
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("send market buy order for {} on {}", amount, market)
                    ))
                })?;

            info!(
                "Market buy order sent successfully (order_id: {})",
                result.order_id
            );

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
                info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }
        }
        Commands::BuyLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit BUY order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            let result = executor
                .execute(send_order::call_send_order_from_config(
                    stack_url,
                    market.clone(),
                    1, // Buy side
                    amount.clone(),
                    Some(price.clone()),
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!(
                            "send limit buy order for {} at {} on {}",
                            amount, price, market
                        )
                    ))
                })?;

            info!(
                "Limit buy order sent successfully (order_id: {})",
                result.order_id
            );

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
                info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }
        }
        Commands::SellMarket { market, amount } => {
            info!("Sending market SELL order for {amount} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            let result = executor
                .execute(send_order::call_send_order_from_config(
                    stack_url,
                    market.clone(),
                    2, // Sell side
                    amount.clone(),
                    None, // No limit price (market order)
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("send market sell order for {} on {}", amount, market)
                    ))
                })?;

            info!(
                "Market sell order sent successfully (order_id: {})",
                result.order_id
            );

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
                info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }
        }
        Commands::SellLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit SELL order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            let result = executor
                .execute(send_order::call_send_order_from_config(
                    stack_url,
                    market.clone(),
                    2, // Sell side
                    amount.clone(),
                    Some(price.clone()),
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!(
                            "send limit sell order for {} at {} on {}",
                            amount, price, market
                        )
                    ))
                })?;

            info!(
                "Limit sell order sent successfully (order_id: {})",
                result.order_id
            );

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
                info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }
        }
        Commands::CancelOrder {
            market,
            side,
            order_id,
        } => {
            info!("Canceling order {order_id} ({side}) on market {market}");

            // Fetch configuration from server
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::call_get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            let result = executor
                .execute(cancel_order::call_cancel_order_from_config(
                    stack_url,
                    market.clone(),
                    side.clone(),
                    order_id,
                    privkey,
                    config,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("cancel order {} on {}", order_id, market)
                    ))
                })?;

            if result.order_canceled {
                info!("Order {} canceled successfully", order_id);
            } else {
                info!("Order {} was not found or already canceled", order_id);
            }

            // Log transaction hashes if available
            if !result.transaction_hashes.is_empty() {
                info!("Transaction hashes:");
                for formatted_hash in result.get_formatted_transaction_hashes() {
                    info!("  {}", formatted_hash);
                }
                info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
            }
        }
        Commands::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(config::get_config(stack_url))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let privkey = get_trader_privkey()?;

            executor
                .execute(balance::balance_from_config(config, privkey))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch balances")))?;
        }
        Commands::Status => {
            println!("Configuration Status:");
            println!("  Stack URL: {}", client.stack_url());

            // Ping the gRPC server
            let ping_result = executor.execute(aspens::health::ping_grpc_server(
                client.stack_url().to_string(),
            ));
            if ping_result.success {
                println!(
                    "  Connection: OK ({}ms)",
                    ping_result.latency_ms.unwrap_or(0)
                );
            } else {
                let error_msg = ping_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());

                println!("  Connection: FAILED");
                println!();

                if error_msg.contains("Connection refused") {
                    println!("Could not connect to the server.");
                    println!();
                    println!("Possible causes:");
                    println!("  - The Aspens server is not running");
                    println!("  - The server URL is incorrect");
                    println!("  - A firewall is blocking the connection");
                } else if error_msg.contains("dns") || error_msg.contains("resolve") {
                    println!("Could not resolve the server hostname.");
                    println!();
                    println!("Possible causes:");
                    println!("  - The hostname is incorrect");
                    println!("  - DNS is not configured properly");
                    println!("  - No internet connection");
                } else if error_msg.contains("tls")
                    || error_msg.contains("ssl")
                    || error_msg.contains("certificate")
                {
                    println!("TLS/SSL error: {}", error_msg);
                    println!();
                    println!("Possible causes:");
                    println!("  - Using wrong protocol (http vs https)");
                    println!("  - Server certificate is invalid");
                } else if error_msg.contains("timeout") {
                    println!("Connection timed out.");
                    println!();
                    println!("Possible causes:");
                    println!("  - Server is overloaded or unresponsive");
                    println!("  - Network latency is too high");
                } else {
                    println!("Error: {}", error_msg);
                }

                println!();
                println!("Hints:");
                println!("  - Verify ASPENS_MARKET_STACK_URL in your .env file");
                println!("  - Use --stack flag to specify a different URL");
                println!("  - For local: http://localhost:50051");
                println!("  - For remote: https://your-server:50051");
            }
        }
        Commands::TraderPublicKey => {
            use alloy::signers::local::PrivateKeySigner;

            let privkey = get_trader_privkey()?;
            let signer = privkey.parse::<PrivateKeySigner>().map_err(|e| {
                eyre::eyre!(
                    "Invalid TRADER_PRIVKEY format\n\n\
                     Error: {}\n\n\
                     Hints:\n\
                     - The private key should be a 64-character hex string\n\
                     - Do not include the '0x' prefix\n\
                     - Check for any extra whitespace or newlines",
                    e
                )
            })?;
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
            let config = executor
                .execute(config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            // If output_file is provided, save to file
            if let Some(ref path) = output_file {
                executor
                    .execute(config::download_config(stack_url.clone(), path.clone()))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("save configuration to '{}'", path)
                        ))
                    })?;
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
            info!("Fetching signer public key(s) and gas balances from {stack_url}");
            let signer_infos = executor
                .execute(config::get_signer_public_key_with_balances(
                    stack_url, chain_id,
                ))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch signer public key(s)")))?;

            println!("Signer Public Keys:");
            for info in &signer_infos {
                println!("  Chain {} ({}):", info.chain_id, info.chain_network);
                println!("    Address:     {}", info.public_key);
                println!("    Gas Balance: {} (native)", info.formatted_gas_balance());
            }
        }
        Commands::StreamOrderbook {
            market,
            historical,
            trader,
        } => {
            info!("Streaming orderbook for market {market}");
            if historical {
                info!("Including historical open orders");
            }
            if let Some(ref t) = trader {
                info!("Filtering by trader: {}", t);
            }

            let stack_url = client.stack_url().to_string();
            let options = stream_orderbook::StreamOrderbookOptions {
                market_id: market.clone(),
                historical_open_orders: historical,
                filter_by_trader: trader,
            };

            println!("Streaming orderbook for market: {}", market);
            println!("Press Ctrl+C to stop");
            println!();
            println!("{}", "-".repeat(120));

            executor
                .execute(stream_orderbook::stream_orderbook(
                    stack_url,
                    options,
                    |entry| {
                        println!("{}", stream_orderbook::format_orderbook_entry(&entry));
                    },
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("stream orderbook for market {}", market)
                    ))
                })?;
        }
        Commands::StreamTrades {
            market,
            historical,
            trader,
        } => {
            info!("Streaming trades for market {market}");
            if historical {
                info!("Including historical closed trades");
            }
            if let Some(ref t) = trader {
                info!("Filtering by trader: {}", t);
            }

            let stack_url = client.stack_url().to_string();
            let options = stream_trades::StreamTradesOptions {
                market_id: market.clone(),
                historical_closed_trades: historical,
                filter_by_trader: trader,
            };

            println!("Streaming trades for market: {}", market);
            println!("Press Ctrl+C to stop");
            println!();
            println!("{}", "-".repeat(140));

            executor
                .execute(stream_trades::stream_trades(stack_url, options, |trade| {
                    println!("{}", stream_trades::format_trade(&trade));
                }))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("stream trades for market {}", market)
                    ))
                })?;
        }
        Commands::GetAttestation {
            report_data,
            output,
        } => {
            use aspens::commands::config;

            info!("Fetching TEE attestation from signer");

            let stack_url = client.stack_url().to_string();

            // Parse report_data from hex if provided
            let report_data_bytes = if let Some(hex_data) = report_data {
                let hex_data = hex_data.strip_prefix("0x").unwrap_or(&hex_data);
                Some(hex::decode(hex_data).map_err(|e| {
                    eyre::eyre!(
                        "Invalid hex data for --report-data: {}\n\n\
                         Hints:\n\
                         - Provide data as hex string (with or without 0x prefix)\n\
                         - Maximum 64 bytes (128 hex characters)",
                        e
                    )
                })?)
            } else {
                None
            };

            // Validate report_data length
            if let Some(ref data) = report_data_bytes {
                if data.len() > 64 {
                    return Err(eyre::eyre!(
                        "Report data too long: {} bytes (max 64 bytes)\n\n\
                         Hints:\n\
                         - Maximum report data length is 64 bytes\n\
                         - Your data is {} hex characters, which is {} bytes",
                        data.len(),
                        data.len() * 2,
                        data.len()
                    ));
                }
            }

            let response = executor
                .execute(config::get_attestation(stack_url, report_data_bytes))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch TEE attestation")))?;

            match output.as_str() {
                "json" => {
                    // Output as JSON
                    if let Some(report) = &response.report {
                        let json = serde_json::json!({
                            "tee_tcb_svn": report.tee_tcb_svn,
                            "mr_seam": report.mr_seam,
                            "mr_signer_seam": report.mr_signer_seam,
                            "seam_attributes": report.seam_attributes,
                            "td_attributes": report.td_attributes,
                            "xfam": report.xfam,
                            "mr_td": report.mr_td,
                            "mr_config_id": report.mr_config_id,
                            "mr_owner": report.mr_owner,
                            "mr_owner_config": report.mr_owner_config,
                            "rt_mr0": report.rt_mr0,
                            "rt_mr1": report.rt_mr1,
                            "rt_mr2": report.rt_mr2,
                            "rt_mr3": report.rt_mr3,
                            "report_data": report.report_data,
                        });
                        println!("{}", serde_json::to_string_pretty(&json)?);
                    } else {
                        println!("null");
                    }
                }
                _ => {
                    // Default text output
                    if let Some(report) = &response.report {
                        print!("{}", config::format_attestation_report(report));
                    } else {
                        println!("No attestation report available");
                    }
                }
            }
        }
    }

    Ok(())
}
