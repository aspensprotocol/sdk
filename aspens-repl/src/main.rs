use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::{
    balance, cancel_order, deposit, send_order, stream_orderbook, stream_trades, withdraw,
};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor, Wallet};
use aspens_cliutil::BinaryContext;
use clap::Parser;
use clap_repl::reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory};
use clap_repl::ClapEditor;
use std::sync::{Arc, Mutex};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

/// Local thin wrapper over [`aspens_cliutil::format_error`].
fn format_error(err: &eyre::Report, context: &str) -> String {
    aspens_cliutil::format_error(err, context, &BinaryContext::TRADER_REPL)
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

/// Pull `TRADER_PRIVKEY` from the REPL's session env (not process env, so
/// `.env` changes during the session are honoured) and build an EVM
/// [`Wallet`]. Returns `None` after printing a user-friendly error if the
/// key is missing or malformed — call sites just `return` in that case.
fn load_trader_wallet_or_complain(app_state: &AppState) -> Option<Wallet> {
    let key = match app_state.get_env("TRADER_PRIVKEY") {
        Some(k) => k,
        None => {
            print_missing_privkey_error();
            return None;
        }
    };
    match Wallet::from_evm_hex(&key) {
        Ok(w) => Some(w),
        Err(e) => {
            print_error(&format_error(&eyre::eyre!(e), "load TRADER_PRIVKEY"));
            None
        }
    }
}

/// Print a friendly error message
fn print_error(message: &str) {
    println!();
    for line in message.lines() {
        println!("{}", line);
    }
    println!();
}

/// Local thin wrapper over [`aspens_cliutil::resolve_token_amount`].
fn resolve_token_amount(
    config: &GetConfigResponse,
    network: &str,
    token_symbol: &str,
    amount: &str,
) -> eyre::Result<u64> {
    aspens_cliutil::resolve_token_amount(config, network, token_symbol, amount)
}

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
            .block_on(async { aspens::commands::config::get_config(url).await })
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
        /// Amount in human-readable units (e.g., "10", "10.5"). Scaled
        /// by the token's `decimals` from the chain config.
        amount: String,
    },
    /// Withdraw tokens to a local wallet (requires network, token, amount)
    Withdraw {
        /// The network name to withdraw from (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to withdraw (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount in human-readable units (e.g., "10", "10.5"). Scaled
        /// by the token's `decimals` from the chain config.
        amount: String,
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
        /// Post-only: reject the order if it would cross at submission.
        /// Use this to guarantee maker-side execution; arborter returns
        /// FAILED_PRECONDITION (no on-chain lock) if it would cross.
        #[arg(long)]
        post_only: bool,
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
        /// Post-only: see `buy-limit --post-only`.
        #[arg(long)]
        post_only: bool,
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
        /// Optional chain network to filter by (e.g., "base-sepolia"). If not provided, returns all chains.
        #[arg(long)]
        chain_network: Option<String>,
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
    /// Stream executed trades in real-time (press Ctrl+C to stop)
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

    let history_path = Arc::new(std::env::temp_dir().join("aspens-repl-history"));
    let rl = ClapEditor::<ReplCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook({
            let history_path = history_path.clone();
            move |reed| {
                reed.with_history(Box::new(
                    FileBackedHistory::with_file(10000, history_path.as_ref().clone()).unwrap(),
                ))
            }
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

            let amount_base = match resolve_token_amount(&config, &network, &token, &amount) {
                Ok(v) => v,
                Err(e) => {
                    print_error(&format_error(
                        &e,
                        &format!("deposit {} {} on {}", amount, token, network),
                    ));
                    return;
                }
            };

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            // `async move` so `wallet` moves into the future and the
            // executor sees a `'static` future. The library's
            // `*_with_wallet` API takes `&Wallet`, so we re-borrow inside
            // the closure.
            let net = network.clone();
            let tok = token.clone();
            let res = executor.execute(async move {
                deposit::call_deposit_from_config_with_wallet(
                    net,
                    tok,
                    amount_base,
                    &wallet,
                    config,
                )
                .await
            });
            match res {
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

            let amount_base = match resolve_token_amount(&config, &network, &token, &amount) {
                Ok(v) => v,
                Err(e) => {
                    print_error(&format_error(
                        &e,
                        &format!("withdraw {} {} from {}", amount, token, network),
                    ));
                    return;
                }
            };

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let stack_url = app_state.stack_url();
            let net = network.clone();
            let tok = token.clone();
            let res = executor.execute(async move {
                withdraw::call_withdraw_from_config_with_wallet(
                    stack_url,
                    net,
                    tok,
                    amount_base,
                    &wallet,
                    config,
                )
                .await
            });
            match res {
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

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallet(
                    url, mkt, 1, // Buy side
                    amt, None, // No limit price (market order)
                    &wallet, config, false, // post_only meaningless for market orders
                )
                .await
            });
            match res {
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
            post_only,
        } => {
            info!(
                "Sending limit BUY order for {amount} at price {price} on market {market} \
                 (post_only={post_only})"
            );

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let prc = price.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallet(
                    url,
                    mkt,
                    1, // Buy side
                    amt,
                    Some(prc),
                    &wallet,
                    config,
                    post_only,
                )
                .await
            });
            match res {
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

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallet(
                    url, mkt, 2, // Sell side
                    amt, None, // No limit price (market order)
                    &wallet, config, false, // post_only meaningless for market orders
                )
                .await
            });
            match res {
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
            post_only,
        } => {
            info!(
                "Sending limit SELL order for {amount} at price {price} on market {market} \
                 (post_only={post_only})"
            );

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let prc = price.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallet(
                    url,
                    mkt,
                    2, // Sell side
                    amt,
                    Some(prc),
                    &wallet,
                    config,
                    post_only,
                )
                .await
            });
            match res {
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

            let wallet = match load_trader_wallet_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let sd = side.clone();
            let res = executor.execute(async move {
                cancel_order::call_cancel_order_from_config_with_wallet(
                    url, mkt, sd, order_id, &wallet, config,
                )
                .await
            });
            match res {
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
                    let wallet = match load_trader_wallet_or_complain(&app_state) {
                        Some(w) => w,
                        None => return,
                    };
                    let res = executor.execute(async move {
                        let wallets: [&Wallet; 1] = [&wallet];
                        balance::balance_from_config_with_wallets(config, &wallets).await
                    });
                    if let Err(e) = res {
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
        ReplCommand::SignerPublicKey { chain_network } => {
            use aspens::commands::config;

            let stack_url = app_state.stack_url();
            info!(
                "Fetching signer public key(s) and gas balances from {}",
                stack_url
            );
            match executor.execute(config::get_signer_public_key_with_balances(
                stack_url,
                chain_network,
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
        ReplCommand::StreamTrades {
            market,
            historical,
            trader,
        } => {
            info!("Streaming trades for market {}", market);
            if historical {
                info!("Including historical closed trades");
            }
            if let Some(ref t) = trader {
                info!("Filtering by trader: {}", t);
            }

            let stack_url = app_state.stack_url();
            let options = stream_trades::StreamTradesOptions {
                market_id: market.clone(),
                historical_closed_trades: historical,
                filter_by_trader: trader,
            };

            println!("Streaming trades for market: {}", market);
            println!("Press Ctrl+C to stop");
            println!();
            println!("{}", "-".repeat(140));

            match executor.execute(stream_trades::stream_trades(stack_url, options, |trade| {
                println!("{}", stream_trades::format_trade(&trade));
            })) {
                Ok(_) => info!("Stream ended"),
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("stream trades for market {}", market),
                )),
            }
        }
        ReplCommand::GetAttestation {
            report_data,
            output,
        } => {
            use aspens::commands::config;

            info!("Fetching TEE attestation from signer");

            let stack_url = app_state.stack_url();

            // Parse report_data from hex if provided
            let report_data_bytes = if let Some(hex_data) = report_data {
                let hex_data = hex_data.strip_prefix("0x").unwrap_or(&hex_data);
                match hex::decode(hex_data) {
                    Ok(data) => {
                        if data.len() > 64 {
                            print_error(&format!(
                                "Report data too long: {} bytes (max 64 bytes)",
                                data.len()
                            ));
                            return;
                        }
                        Some(data)
                    }
                    Err(e) => {
                        print_error(&format!(
                            "Invalid hex data for --report-data: {}\n\n\
                             Hints:\n\
                               - Provide data as hex string (with or without 0x prefix)\n\
                               - Maximum 64 bytes (128 hex characters)",
                            e
                        ));
                        return;
                    }
                }
            } else {
                None
            };

            match executor.execute(config::get_attestation(stack_url, report_data_bytes)) {
                Ok(response) => match output.as_str() {
                    "json" => {
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
                            match serde_json::to_string_pretty(&json) {
                                Ok(s) => println!("{}", s),
                                Err(e) => println!("Failed to format JSON: {}", e),
                            }
                        } else {
                            println!("null");
                        }
                    }
                    _ => {
                        if let Some(report) = &response.report {
                            print!("{}", config::format_attestation_report(report));
                        } else {
                            println!("No attestation report available");
                        }
                    }
                },
                Err(e) => print_error(&format_error(&e, "fetch TEE attestation")),
            }
        }
        ReplCommand::Quit => {
            println!("Goodbye!");
            std::process::exit(0)
        }
    });
}
