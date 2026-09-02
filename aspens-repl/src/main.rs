use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::{
    balance, cancel_order, deposit, send_order, stream_orderbook, stream_trades, withdraw,
};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor, Wallet};
use aspens_cliutil::BinaryContext;
use clap::Parser;
use clap_repl::ClapEditor;
use clap_repl::reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory};
use std::sync::{Arc, Mutex};
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

/// Local thin wrapper over [`aspens_cliutil::format_error`].
fn format_error(err: &eyre::Report, context: &str) -> String {
    aspens_cliutil::format_error(err, context, &BinaryContext::TRADER_REPL)
}

/// Print a friendly error message for missing TRADER_PRIVKEY
fn print_missing_privkey_error() {
    println!();
    println!("No trader wallet configured");
    println!();
    println!("Hints:");
    println!("  - Set TRADER_PRIVKEY (EVM) and/or TRADER_PRIVKEY_SOLANA (Solana)");
    println!("    in your .env file — either one is enough for its chains");
    println!("  - TRADER_PRIVKEY is a 64-character hex string, no '0x' prefix");
    println!("  - TRADER_PRIVKEY_SOLANA is a base58 or JSON 64-byte keypair");
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

/// Load every trader wallet present in the session env: `TRADER_PRIVKEY`
/// (EVM) and/or `TRADER_PRIVKEY_SOLANA` (Solana keypair, base58 or JSON).
/// Order placement picks a wallet per market leg by curve, and a market
/// with a Solana leg is unplaceable with only an EVM key — so the order
/// commands load the full set rather than assuming EVM. Returns `None`
/// (after printing why) when no key is present, or when a key that IS
/// present is malformed — a malformed key is a broken setup to surface,
/// not an absence to shrug off.
fn load_trader_wallets_or_complain(app_state: &AppState) -> Option<Vec<Wallet>> {
    let mut wallets = Vec::new();
    if let Some(key) = app_state.get_env("TRADER_PRIVKEY") {
        match Wallet::from_evm_hex(&key) {
            Ok(w) => wallets.push(w),
            Err(e) => {
                print_error(&format_error(&eyre::eyre!(e), "load TRADER_PRIVKEY"));
                return None;
            }
        }
    }
    if let Some(key) = app_state.get_env("TRADER_PRIVKEY_SOLANA") {
        match Wallet::from_solana_base58(&key).or_else(|_| Wallet::from_solana_json(&key)) {
            Ok(w) => wallets.push(w),
            Err(e) => {
                print_error(&format_error(
                    &eyre::eyre!(e),
                    "load TRADER_PRIVKEY_SOLANA (fix the key, or remove the \
                     variable if you only trade EVM chains)",
                ));
                return None;
            }
        }
    }
    if wallets.is_empty() {
        print_missing_privkey_error();
        return None;
    }
    Some(wallets)
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
) -> eyre::Result<u128> {
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
        let guard = self.client.lock().unwrap_or_else(|p| p.into_inner());
        guard.stack_url().to_string()
    }

    fn get_env(&self, key: &str) -> Option<String> {
        let guard = self.client.lock().unwrap_or_else(|p| p.into_inner());
        guard.get_env(key).cloned()
    }

    fn get_config_sync(
        &self,
    ) -> eyre::Result<aspens::commands::config::config_pb::GetConfigResponse> {
        let guard = self.client.lock().unwrap_or_else(|p| p.into_inner());
        let url = guard.stack_url().to_string();
        drop(guard); // Release lock before async call

        // Block on the async fetch via a tokio runtime.
        tokio::runtime::Runtime::new()
            .map_err(|e| eyre::eyre!("could not start the async runtime: {e}"))?
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

/// The optional settlement-address overrides, shared by every order command.
///
/// An order carries one account address per chain leg; the venue credits
/// fill proceeds to those exact strings. By default both are the trader
/// wallets' own addresses. Overriding the RECEIVING leg settles proceeds
/// to a different address; overriding the GIVING leg may only restate the
/// signing wallet's address (the venue verifies the signature against it).
/// Passing a flag is the acknowledgement that you mean it: nothing can
/// check that anyone holds the key to a redirected address, and funds
/// credited there are withdrawable only by that key's holder.
#[derive(Debug, Clone, Default, clap::Args)]
struct SettleArgs {
    /// Settle the BASE leg to this address (must be valid on the base
    /// chain: 0x-prefixed hex on EVM, base58 pubkey on Solana). On a SELL
    /// this is the giving leg and may only restate your own address.
    #[arg(long)]
    base_address: Option<String>,
    /// Settle the QUOTE leg to this address (same per-chain rules). On a
    /// BUY this is the giving leg and may only restate your own address.
    #[arg(long)]
    quote_address: Option<String>,
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
        /// REQUIRED: the maximum QUOTE you are prepared to spend,
        /// human-readable (e.g. "250.5"). A market buy gives quote and
        /// has no price to size that with, so this — not `amount` — is
        /// what bounds and collateralises it.
        #[arg(long)]
        quote_budget: String,
        /// Invisible order: your fills print in the public trade stream
        /// with your side's identity redacted. A market order never
        /// rests, so orderbook suppression doesn't apply — the flag's
        /// effect here is anonymous taking.
        #[arg(long, default_value_t = false)]
        hidden: bool,
        /// Settlement-address overrides (see the flags' own help).
        #[command(flatten)]
        settle: SettleArgs,
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
        /// Invisible order: excluded from the public orderbook stream and
        /// depth; fills print with your side redacted. Track it via the
        /// returned order id.
        #[arg(long, default_value_t = false)]
        hidden: bool,
        /// Dealroom: fill ONLY against these resting order ids (repeatable).
        /// Each is `0x` + 64 hex characters (32 bytes), exactly as returned
        /// in `SendOrderResponse.order_id`. Requires a limit price; the
        /// remainder is canceled, never rested (IOC).
        #[arg(long = "match-order-id")]
        match_order_ids: Vec<String>,
        /// Settlement-address overrides (see the flags' own help).
        #[command(flatten)]
        settle: SettleArgs,
    },
    /// Send a market SELL order (executes at best available price)
    SellMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
        /// Invisible order: see `buy-market --hidden`.
        #[arg(long, default_value_t = false)]
        hidden: bool,
        /// Settlement-address overrides (see the flags' own help).
        #[command(flatten)]
        settle: SettleArgs,
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
        /// Invisible order: see `buy-limit --hidden`.
        #[arg(long, default_value_t = false)]
        hidden: bool,
        /// Dealroom: see `buy-limit --match-order-id`.
        #[arg(long = "match-order-id")]
        match_order_ids: Vec<String>,
        /// Settlement-address overrides (see the flags' own help).
        #[command(flatten)]
        settle: SettleArgs,
    },
    /// Cancel an existing order by its ID
    CancelOrder {
        /// Market ID the order is on
        market: String,
        /// Order side: "buy" or "sell"
        side: String,
        /// The order's canonical id to cancel: `0x` + 64 hex characters (32
        /// bytes), exactly as returned in `SendOrderResponse.order_id`.
        order_id: String,
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
        /// Optional hex-encoded freshness nonce bound into the quote's REPORTDATA
        /// (any length -- it is pre-hashed; 32 random bytes is conventional)
        #[arg(long)]
        nonce: Option<String>,
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
    // Best-effort: failing here only means logs aren't captured (e.g. a
    // subscriber is already set in-process) — don't abort the REPL over it.
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Build the client
    let mut builder = AspensClient::builder();
    if let Some(ref env_file) = cli.env_file {
        builder = builder.with_env_file(env_file);
    }
    if let Some(ref url) = cli.stack_url {
        builder = match builder.with_url(url.to_string()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: invalid stack URL: {e}");
                std::process::exit(1);
            }
        };
    }
    // A missing/invalid stack URL is a normal misconfiguration, not a bug —
    // print the (actionable) error and exit non-zero instead of panicking with
    // a backtrace. (aspens-cli does this via run() -> Result + ExitCode.)
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "hint: pass --stack <URL>, or set ASPENS_MARKET_STACK_URL (e.g. in a .env file)."
            );
            std::process::exit(1);
        }
    };

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
                // Fall back to in-memory (session-only) history if the history
                // file can't be opened (e.g. a read-only or full temp dir),
                // instead of panicking at startup.
                match FileBackedHistory::with_file(10000, history_path.as_ref().clone()) {
                    Ok(h) => reed.with_history(Box::new(h)),
                    Err(e) => {
                        eprintln!(
                            "warning: could not open REPL history file ({e}); using in-memory history."
                        );
                        reed
                    }
                }
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
        ReplCommand::BuyMarket {
            market,
            amount,
            quote_budget,
            hidden,
            settle,
        } => {
            info!(
                "Sending market BUY order for {amount} on market {market} \
                 (quote_budget={quote_budget}, hidden={hidden})"
            );

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallets = match load_trader_wallets_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let budget = quote_budget.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallets(
                    url,
                    mkt,
                    1, // Buy side
                    amt,
                    None, // No limit price (market order)
                    &wallets.iter().collect::<Vec<&Wallet>>(),
                    config,
                    false, // post_only meaningless for market orders
                    hidden,
                    Some(budget), // what actually bounds a market buy
                    vec![],       // dealroom discretionary requires a limit price; not offered here
                    settle.base_address,
                    settle.quote_address,
                )
                .await
            });
            match res {
                Ok(result) => {
                    info!(
                        "Market buy order sent successfully (order_id: 0x{})",
                        hex::encode(&result.order_id)
                    );
                    if let Some(line) = result.settlement_summary() {
                        info!("{line}");
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
            hidden,
            match_order_ids,
            settle,
        } => {
            info!(
                "Sending limit BUY order for {amount} at price {price} on market {market} \
                 (post_only={post_only}, hidden={hidden}, match_order_ids={match_order_ids:?})"
            );

            let match_order_ids: Vec<[u8; 32]> = match match_order_ids
                .iter()
                .map(|s| aspens_cliutil::parse_order_id("match-order-id", s))
                .collect()
            {
                Ok(ids) => ids,
                Err(e) => {
                    print_error(&format_error(&e, "parse --match-order-id"));
                    return;
                }
            };

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallets = match load_trader_wallets_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let prc = price.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallets(
                    url,
                    mkt,
                    1, // Buy side
                    amt,
                    Some(prc),
                    &wallets.iter().collect::<Vec<&Wallet>>(),
                    config,
                    post_only,
                    hidden,
                    None, // a limit order's budget is derived: quantity x price
                    match_order_ids,
                    settle.base_address,
                    settle.quote_address,
                )
                .await
            });
            match res {
                Ok(result) => {
                    info!(
                        "Limit buy order sent successfully (order_id: 0x{})",
                        hex::encode(&result.order_id)
                    );
                    if let Some(line) = result.settlement_summary() {
                        info!("{line}");
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
        ReplCommand::SellMarket {
            market,
            amount,
            hidden,
            settle,
        } => {
            info!("Sending market SELL order for {amount} on market {market} (hidden={hidden})");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallets = match load_trader_wallets_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallets(
                    url,
                    mkt,
                    2, // Sell side
                    amt,
                    None, // No limit price (market order)
                    &wallets.iter().collect::<Vec<&Wallet>>(),
                    config,
                    false, // post_only meaningless for market orders
                    hidden,
                    None,   // an ASK gives base: its budget IS its quantity
                    vec![], // dealroom discretionary requires a limit price; not offered here
                    settle.base_address,
                    settle.quote_address,
                )
                .await
            });
            match res {
                Ok(result) => {
                    info!(
                        "Market sell order sent successfully (order_id: 0x{})",
                        hex::encode(&result.order_id)
                    );
                    if let Some(line) = result.settlement_summary() {
                        info!("{line}");
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
            hidden,
            match_order_ids,
            settle,
        } => {
            info!(
                "Sending limit SELL order for {amount} at price {price} on market {market} \
                 (post_only={post_only}, hidden={hidden}, match_order_ids={match_order_ids:?})"
            );

            let match_order_ids: Vec<[u8; 32]> = match match_order_ids
                .iter()
                .map(|s| aspens_cliutil::parse_order_id("match-order-id", s))
                .collect()
            {
                Ok(ids) => ids,
                Err(e) => {
                    print_error(&format_error(&e, "parse --match-order-id"));
                    return;
                }
            };

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            let wallets = match load_trader_wallets_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };

            let url = app_state.stack_url();
            let mkt = market.clone();
            let amt = amount.clone();
            let prc = price.clone();
            let res = executor.execute(async move {
                send_order::send_order_with_wallets(
                    url,
                    mkt,
                    2, // Sell side
                    amt,
                    Some(prc),
                    &wallets.iter().collect::<Vec<&Wallet>>(),
                    config,
                    post_only,
                    hidden,
                    None, // a limit order's budget is derived: quantity x price
                    match_order_ids,
                    settle.base_address,
                    settle.quote_address,
                )
                .await
            });
            match res {
                Ok(result) => {
                    info!(
                        "Limit sell order sent successfully (order_id: 0x{})",
                        hex::encode(&result.order_id)
                    );
                    if let Some(line) = result.settlement_summary() {
                        info!("{line}");
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
            let order_id = match aspens_cliutil::parse_order_id("order_id", &order_id) {
                Ok(id) => id,
                Err(e) => {
                    print_error(&format_error(&e, "parse order_id"));
                    return;
                }
            };
            let order_id_hex = format!("0x{}", hex::encode(order_id));
            info!(
                "Canceling order {} ({}) on market {}",
                order_id_hex, side, market
            );

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    print_error(&format_error(&e, "fetch configuration"));
                    return;
                }
            };

            // A cancel is authenticated against the order's COLLATERAL
            // address — the give-leg wallet that signed the order (buy →
            // quote chain, sell → base chain). Loading only the EVM wallet
            // here would make a Solana-signed order placeable from this
            // REPL but not cancelable from it.
            let mut wallets = match load_trader_wallets_or_complain(&app_state) {
                Some(w) => w,
                None => return,
            };
            // `Wallet` is deliberately not Clone (it holds key material);
            // find the index and take ownership out of the vec instead.
            let picked = send_order::parse_side(&side)
                .and_then(|s| send_order::origin_network_for_side(&config, &market, s))
                .and_then(|origin| {
                    let chain = config
                        .get_chain(origin)
                        .ok_or_else(|| eyre::eyre!("chain '{origin}' not in config"))?;
                    let curve = aspens::wallet::chain_curve(chain);
                    wallets
                        .iter()
                        .position(|w| w.curve() == curve)
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "no wallet of curve {curve:?} for chain '{origin}' — set \
                                 TRADER_PRIVKEY (EVM) or TRADER_PRIVKEY_SOLANA (Solana)"
                            )
                        })
                });
            let wallet = match picked {
                Ok(idx) => wallets.swap_remove(idx),
                Err(e) => {
                    print_error(&format_error(&e, "resolve the canceling wallet"));
                    return;
                }
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
                        info!("Order {} canceled successfully", order_id_hex);
                    } else {
                        info!("Order {} was not found or already canceled", order_id_hex);
                    }
                }
                Err(e) => print_error(&format_error(
                    &e,
                    &format!("cancel order {} on {}", order_id_hex, market),
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
        ReplCommand::GetAttestation { nonce, output } => {
            use aspens::commands::config;

            info!("Fetching TEE attestation from signer");

            let stack_url = app_state.stack_url();

            let nonce_bytes = match nonce {
                Some(hex_data) => {
                    let hex_data = hex_data.strip_prefix("0x").unwrap_or(&hex_data);
                    match hex::decode(hex_data) {
                        Ok(data) => Some(data),
                        Err(e) => {
                            println!("Invalid hex data for --nonce: {}", e);
                            return;
                        }
                    }
                }
                None => None,
            };

            match executor.execute(config::get_attestation(stack_url, nonce_bytes)) {
                Ok(response) => match output.as_str() {
                    "json" => match &response.report {
                        Some(report) => {
                            match serde_json::to_string_pretty(&config::attestation_report_json(
                                report,
                            )) {
                                Ok(s) => println!("{}", s),
                                Err(e) => println!("Failed to format JSON: {}", e),
                            }
                        }
                        None => println!("null"),
                    },
                    _ => match &response.report {
                        Some(report) => print!("{}", config::format_attestation_report(report)),
                        None => println!("No attestation report available"),
                    },
                },
                Err(e) => println!("Failed to fetch attestation: {:?}", e),
            }
        }
        ReplCommand::Quit => {
            println!("Goodbye!");
            std::process::exit(0)
        }
    });
}
