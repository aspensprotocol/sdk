use aspens::commands::trading::{balance, cancel_order, deposit, send_order, stream_orderbook, withdraw};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor};
use clap::Parser;
use clap_repl::reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory};
use clap_repl::ClapEditor;
use std::sync::{Arc, Mutex};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

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
               - Check server status with 'status' command\n\
               - Verify ASPENS_MARKET_STACK_URL in your .env file",
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
               - Check your internet connection",
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
               - Using HTTP URL for HTTPS server or vice versa\n\
               - Server certificate is invalid or expired\n\n\
             Hints:\n\
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
               - Using HTTPS to connect to an HTTP server\n\n\
             Hints:\n\
               - For remote servers, use https://\n\
               - For local development, use http://",
            context
        ));
    }

    // Timeout errors
    if err_string.contains("timeout") || err_string.contains("timed out") {
        return with_underlying_error(format!(
            "Failed to {}: Request timed out\n\n\
             Possible causes:\n\
               - Server is overloaded or unresponsive\n\
               - Network latency is too high\n\n\
             Hints:\n\
               - Try again in a few moments\n\
               - Check server status with 'status' command",
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
               - Check available chains with 'config' command\n\
               - Verify the network name is spelled correctly",
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
               - Check available tokens with 'config' command\n\
               - Token symbols are case-sensitive (e.g., USDC, not usdc)",
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
               - Check available markets with 'config' command\n\
               - Markets use format: chain_id::token::chain_id::token",
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
               - For testnets, use a faucet to get free test tokens\n\
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
               - Check your balances with 'balance' command\n\
               - For trading: deposit tokens first\n\
               - For deposits: ensure wallet has enough tokens",
            context
        ));
    }

    // Invalid string length (typically from address parsing issues on server)
    if err_string.contains("invalid string length") {
        return with_underlying_error(format!(
            "Failed to {}: Server error during on-chain operation\n\n\
             This error typically occurs when the server fails to parse an address.\n\
             Please check server logs for more details.",
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
               - Contract execution reverted\n\n\
             Hints:\n\
               - Check your wallet balance\n\
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
            "Failed to {}: Invalid private key format\n\n\
             Hints:\n\
               - TRADER_PRIVKEY should be a 64-character hex string\n\
               - Do not include the '0x' prefix\n\
               - Check for extra whitespace or newlines",
            context
        ));
    }

    // Generic fallback
    format!(
        "Failed to {}\n\n\
         Hints:\n\
           - Check server status with 'status' command\n\
           - Verify your .env configuration\n\n\
         Underlying error: {}",
        context, err
    )
}

/// Print a friendly error message for missing TRADER_PRIVKEY
fn print_missing_privkey_error() {
    println!();
    println!("TRADER_PRIVKEY not found");
    println!();
    println!("Hints:");
    println!("  - Set TRADER_PRIVKEY in your .env file");
    println!("  - The private key should be a 64-character hex string");
    println!("  - Do not include the '0x' prefix");
    println!();
}

/// Print a friendly error message
fn print_error(message: &str) {
    println!();
    for line in message.lines() {
        println!("{}", line);
    }
    println!();
}

/// Print friendly status error with hints
fn print_status_error(error_msg: &str) {
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
    println!("  - For local: http://localhost:50051");
    println!("  - For remote: https://your-server:50051");
}

struct AppState {
    client: Arc<Mutex<AspensClient>>,
}

impl AppState {
    fn new(client: AspensClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }

    fn stack_url(&self) -> String {
        let guard = self.client.lock().unwrap();
        guard.stack_url().to_string()
    }

    fn get_env(&self, key: &str) -> Option<String> {
        let guard = self.client.lock().unwrap();
        guard.get_env(key).cloned()
    }

    fn get_config_sync(
        &self,
    ) -> eyre::Result<aspens::commands::config::config_pb::GetConfigResponse> {
        let guard = self.client.lock().unwrap();
        let url = guard.stack_url().to_string();
        drop(guard); // Release lock before async call

        // Use tokio runtime to block on async operation
        tokio::runtime::Runtime::new()?
            .block_on(async { aspens::commands::config::call_get_config(url).await })
    }
}

#[derive(Debug, Parser)]
#[command(name = "aspens-repl")]
#[command(about = "Aspens REPL for interactive trading operations")]
struct ReplCli {
    /// The Aspens stack URL (overrides ASPENS_MARKET_STACK_URL from .env)
    #[arg(short = 's', long = "stack")]
    stack_url: Option<url::Url>,

    /// Path to environment file (defaults to .env in current directory)
    #[arg(short = 'e', long = "env-file")]
    env_file: Option<String>,
}

#[derive(Debug, Parser)]
#[command(name = "", author, version, about, long_about = None)]
enum ReplCommand {
    /// Fetch and display the configuration from the server
    Config {
        /// Optional path to save the configuration file (supports .json or .toml)
        #[arg(short, long)]
        output_file: Option<String>,
    },
    /// Deposit tokens to make them available for trading (requires network, token, amount)
    Deposit {
        /// The network name to deposit to (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to deposit (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to deposit
        amount: u64,
    },
    /// Withdraw tokens to a local wallet (requires network, token, amount)
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
    /// Stream orderbook entries in real-time (press Ctrl+C to stop)
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
    /// Quit the REPL
    Quit,
}

fn main() {
    let cli = ReplCli::parse();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    // Build the client
    let mut builder = AspensClient::builder();
    if let Some(ref env_file) = cli.env_file {
        builder = builder.with_env_file(env_file);
    }
    if let Some(ref url) = cli.stack_url {
        builder = builder
            .with_url(url.to_string())
            .expect("Invalid stack URL");
    }
    let client = builder.build().expect("Failed to build AspensClient");

    let app_state = AppState::new(client);
    let executor = BlockingExecutor::new();

    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("aspens".to_owned()),
        ..DefaultPrompt::default()
    };

    let rl = ClapEditor::<ReplCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            reed.with_history(Box::new(
                FileBackedHistory::with_file(10000, "/tmp/aspens-repl-history".into()).unwrap(),
            ))
        })
        .build();

    rl.repl(|command| match command {
        ReplCommand::Config { output_file } => {
            use aspens::commands::config;

            let stack_url = app_state.stack_url();
            info!("Fetching configuration from {}", stack_url);
            match executor.execute(config::get_config(stack_url.clone())) {
                Ok(config) => {
                    // If output_file is provided, save to file
                    if let Some(ref path) = output_file {
                        match executor
                            .execute(config::download_config(stack_url.clone(), path.clone()))
                        {
                            Ok(_) => info!("Configuration saved to: {}", path),
                            Err(e) => print_error(&format_error(
                                &e,
                                &format!("save configuration to '{}'", path),
                            )),
                        }
                    } else {
                        // Display config as JSON
                        match serde_json::to_string_pretty(&config) {
                            Ok(json) => println!("{}", json),
                            Err(e) => println!("Failed to format config as JSON: {}", e),
                        }
                    }
                }
                Err(e) => print_error(&format_error(&e, "fetch configuration")),
            }
        }
        ReplCommand::Deposit {
            network,
            token,
            amount,
        } => {
            info!("Depositing {amount} {token} on {network}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(deposit::call_deposit_from_config(
                network.clone(),
                token.clone(),
                amount,
                privkey,
                config,
            )) {
                Ok(_) => info!("Deposit successful"),
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("deposit {} {} on {}", amount, token, network),
                )),
            }
        }
        ReplCommand::Withdraw {
            network,
            token,
            amount,
        } => {
            info!("Withdrawing {amount} {token} from {network}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(withdraw::call_withdraw_from_config(
                network.clone(),
                token.clone(),
                amount,
                privkey,
                config,
            )) {
                Ok(_) => info!("Withdraw successful"),
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("withdraw {} {} from {}", amount, token, network),
                )),
            }
        }
        ReplCommand::BuyMarket { market, amount } => {
            info!("Sending market BUY order for {amount} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market.clone(),
                1, // Buy side
                amount.clone(),
                None, // No limit price (market order)
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!(
                        "Market buy order sent successfully (order_id: {})",
                        result.order_id
                    );
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("send market buy order for {} on {}", amount, market),
                )),
            }
        }
        ReplCommand::BuyLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit BUY order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market.clone(),
                1, // Buy side
                amount.clone(),
                Some(price.clone()),
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!(
                        "Limit buy order sent successfully (order_id: {})",
                        result.order_id
                    );
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!(
                        "send limit buy order for {} at {} on {}",
                        amount, price, market
                    ),
                )),
            }
        }
        ReplCommand::SellMarket { market, amount } => {
            info!("Sending market SELL order for {amount} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market.clone(),
                2, // Sell side
                amount.clone(),
                None, // No limit price (market order)
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!(
                        "Market sell order sent successfully (order_id: {})",
                        result.order_id
                    );
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("send market sell order for {} on {}", amount, market),
                )),
            }
        }
        ReplCommand::SellLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit SELL order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market.clone(),
                2, // Sell side
                amount.clone(),
                Some(price.clone()),
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!(
                        "Limit sell order sent successfully (order_id: {})",
                        result.order_id
                    );
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!(
                        "send limit sell order for {} at {} on {}",
                        amount, price, market
                    ),
                )),
            }
        }
        ReplCommand::CancelOrder {
            market,
            side,
            order_id,
        } => {
            info!(
                "Canceling order {} ({}) on market {}",
                order_id, side, market
            );

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    print_missing_privkey_error();
                    return;
                }
            };

            match executor.execute(cancel_order::call_cancel_order_from_config(
                app_state.stack_url(),
                market.clone(),
                side.clone(),
                order_id,
                privkey,
                config,
            )) {
                Ok(result) => {
                    if result.order_canceled {
                        info!("Order {} canceled successfully", order_id);
                    } else {
                        info!("Order {} was not found or already canceled", order_id);
                    }
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("cancel order {} on {}", order_id, market),
                )),
            }
        }
        ReplCommand::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = app_state.stack_url();
            match executor.execute(config::get_config(stack_url.clone())) {
                Ok(config) => {
                    let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                        Some(key) => key,
                        None => {
                            print_missing_privkey_error();
                            return;
                        }
                    };
                    if let Err(e) = executor.execute(balance::balance_from_config(config, privkey))
                    {
                        print_error(&format_error(&e, "fetch balances"));
                    }
                }
                Err(e) => print_error(&format_error(&e, "fetch configuration")),
            }
        }
        ReplCommand::Status => {
            println!("Configuration Status:");
            println!("  Server URL: {}", app_state.stack_url());

            // Ping the gRPC server
            let ping_result =
                executor.execute(aspens::health::ping_grpc_server(app_state.stack_url()));
            if ping_result.success {
                println!(
                    "  Connection: OK ({}ms)",
                    ping_result.latency_ms.unwrap_or(0)
                );
            } else {
                let error_msg = ping_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());
                print_status_error(&error_msg);
            }
        }
        ReplCommand::TraderPublicKey => {
            use alloy::signers::local::PrivateKeySigner;

            match app_state.get_env("TRADER_PRIVKEY") {
                Some(privkey) => match privkey.parse::<PrivateKeySigner>() {
                    Ok(signer) => {
                        let address = signer.address();
                        let pubkey = signer.credential().verifying_key();

                        println!("Trader Wallet:");
                        println!("  Address:    {}", address);
                        println!(
                            "  Public Key: 0x{}",
                            hex::encode(pubkey.to_encoded_point(false).as_bytes())
                        );
                    }
                    Err(e) => {
                        println!();
                        println!("Invalid TRADER_PRIVKEY format");
                        println!();
                        println!("Error: {}", e);
                        println!();
                        println!("Hints:");
                        println!("  - The private key should be a 64-character hex string");
                        println!("  - Do not include the '0x' prefix");
                        println!("  - Check for extra whitespace or newlines");
                        println!();
                    }
                },
                None => print_missing_privkey_error(),
            }
        }
        ReplCommand::SignerPublicKey { chain_id } => {
            use aspens::commands::config;

            let stack_url = app_state.stack_url();
            info!(
                "Fetching signer public key(s) and gas balances from {}",
                stack_url
            );
            match executor.execute(config::get_signer_public_key_with_balances(
                stack_url, chain_id,
            )) {
                Ok(signer_infos) => {
                    println!("Signer Public Keys:");
                    for info in &signer_infos {
                        println!("  Chain {} ({}):", info.chain_id, info.chain_network);
                        println!("    Address:     {}", info.public_key);
                        println!("    Gas Balance: {} (native)", info.formatted_gas_balance());
                    }
                }
                Err(e) => print_error(&format_error(&e, "fetch signer public key(s)")),
            }
        }
        ReplCommand::StreamOrderbook {
            market,
            historical,
            trader,
        } => {
            info!("Streaming orderbook for market {}", market);
            if historical {
                info!("Including historical open orders");
            }
            if let Some(ref t) = trader {
                info!("Filtering by trader: {}", t);
            }

            let stack_url = app_state.stack_url();
            let options = stream_orderbook::StreamOrderbookOptions {
                market_id: market.clone(),
                historical_open_orders: historical,
                filter_by_trader: trader,
            };

            println!("Streaming orderbook for market: {}", market);
            println!("Press Ctrl+C to stop");
            println!();
            println!("{}", "-".repeat(120));

            match executor.execute(stream_orderbook::stream_orderbook(
                stack_url,
                options,
                |entry| {
                    println!("{}", stream_orderbook::format_orderbook_entry(&entry));
                },
            )) {
                Ok(_) => info!("Stream ended"),
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("stream orderbook for market {}", market),
                )),
            }
        }
        ReplCommand::Quit => {
            println!("Goodbye!");
            std::process::exit(0)
        }
    });
}
