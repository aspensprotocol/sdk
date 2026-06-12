use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::send_order::{
    arborter_pb::{SendOrderResponse, Side},
    origin_network_for_side, parse_side,
};
use aspens::commands::trading::{
    balance, cancel_order, deposit, send_order, stream_orderbook, stream_trades, withdraw,
};
use aspens::tdx_verify::reportdata::CurveTag;
use aspens::{
    AspensClient, AsyncExecutor, CurveType, DirectExecutor, Wallet, load_trader_wallet,
    load_trader_wallet_for_network,
};
use aspens_cliutil::BinaryContext;
use clap::Parser;
use eyre::Result;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;
use url::Url;

/// Local thin wrapper over [`aspens_cliutil::format_error`] so existing
/// call sites don't have to pass [`BinaryContext::TRADER_CLI`] explicitly.
fn format_error(err: &eyre::Report, context: &str) -> String {
    aspens_cliutil::format_error(err, context, &BinaryContext::TRADER_CLI)
}

/// Decode a hex string (with or without `0x`) for `--{label}`.
fn parse_hex(label: &str, s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(s).map_err(|e| eyre::eyre!("invalid hex for --{label}: {e}"))
}

/// Decode an optional fixed-width hex value (`N` bytes) for `--{label}`.
fn parse_fixed<const N: usize>(label: &str, s: &Option<String>) -> Result<Option<[u8; N]>> {
    match s {
        None => Ok(None),
        Some(s) => {
            let bytes = parse_hex(label, s)?;
            let arr: [u8; N] = bytes.as_slice().try_into().map_err(|_| {
                eyre::eyre!(
                    "--{label} must be {N} bytes ({} hex chars), got {}",
                    N * 2,
                    bytes.len()
                )
            })?;
            Ok(Some(arr))
        }
    }
}

/// Parse an `--expected-pubkey <curve>:<hex>` argument into a curve tag + raw
/// pubkey bytes. Accepts `secp256k1`/`evm`/`k1` and `ed25519`/`solana`/`sol`.
fn parse_expected_pubkey(s: &str) -> Result<(CurveTag, Vec<u8>)> {
    let (curve, hex_str) = s.split_once(':').ok_or_else(|| {
        eyre::eyre!("--expected-pubkey must be `<curve>:<hex>` (e.g. secp256k1:04ab…), got `{s}`")
    })?;
    let tag = match curve.trim().to_ascii_lowercase().as_str() {
        "secp256k1" | "evm" | "k1" => CurveTag::Secp256k1,
        "ed25519" | "solana" | "sol" => CurveTag::Ed25519,
        other => {
            return Err(eyre::eyre!(
                "unknown curve `{other}` in --expected-pubkey (use secp256k1/evm or ed25519/solana)"
            ));
        }
    };
    let bytes = parse_hex("expected-pubkey", hex_str)?;
    if bytes.is_empty() {
        return Err(eyre::eyre!("--expected-pubkey has empty key bytes"));
    }
    Ok((tag, bytes))
}

/// Read a raw TD quote from a file: hex text if the whole (trimmed) file decodes
/// as hex, otherwise the raw bytes verbatim.
fn read_quote_file(path: &std::path::Path) -> Result<Vec<u8>> {
    let raw = std::fs::read(path)
        .map_err(|e| eyre::eyre!("reading quote file {}: {e}", path.display()))?;
    if let Ok(text) = std::str::from_utf8(&raw) {
        let trimmed = text.trim();
        let candidate = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        if !candidate.is_empty()
            && candidate.len() % 2 == 0
            && candidate.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(decoded) = hex::decode(candidate)
        {
            return Ok(decoded);
        }
    }
    Ok(raw)
}

/// Shared shape for buy-market / buy-limit / sell-market / sell-limit:
/// fetch config → load wallets for both chains in the market → submit via
/// `send_order_with_wallets`. Cross-chain markets that span EVM + Solana
/// need *both* a Secp256k1 wallet (for the EVM leg's address) and an
/// Ed25519 wallet (for the Solana leg). The CLI loads each opportunistically
/// and the lib selects the right one per chain.
fn dispatch_send_order(
    executor: &DirectExecutor,
    client: &AspensClient,
    market: String,
    side: Side,
    amount: String,
    price: Option<String>,
    post_only: bool,
) -> Result<SendOrderResponse> {
    let stack_url = client.stack_url().to_string();
    let config = executor
        .execute(aspens::commands::config::get_config(stack_url.clone()))
        .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
    // Load both wallets if available. The lib picks whichever one matches
    // each chain's architecture (and errors if neither matches).
    let evm = load_trader_wallet(CurveType::Secp256k1).ok();
    let solana = load_trader_wallet(CurveType::Ed25519).ok();
    if evm.is_none() && solana.is_none() {
        return Err(eyre::eyre!(
            "No trader wallet configured. Set TRADER_PRIVKEY (EVM) and/or \
             TRADER_PRIVKEY_SOLANA (Solana) in your .env file."
        ));
    }
    let context = match (side, &price) {
        (Side::Bid, Some(p)) => {
            format!("send limit buy order for {} at {} on {}", amount, p, market)
        }
        (Side::Bid, None) => format!("send market buy order for {} on {}", amount, market),
        (Side::Ask, Some(p)) => {
            format!(
                "send limit sell order for {} at {} on {}",
                amount, p, market
            )
        }
        (Side::Ask, None) => format!("send market sell order for {} on {}", amount, market),
        (Side::Unspecified, _) => format!("send order on {}", market),
    };
    executor
        .execute(async move {
            let wallets: Vec<&Wallet> = [evm.as_ref(), solana.as_ref()]
                .into_iter()
                .flatten()
                .collect();
            send_order::send_order_with_wallets(
                stack_url,
                market,
                side as i32,
                amount,
                price,
                &wallets,
                config,
                post_only,
            )
            .await
        })
        .map_err(|e| eyre::eyre!(format_error(&e, &context)))
}

/// Resolve a slippage-capped limit price for the `buy-marketable` /
/// `sell-marketable` CLI commands.
///
/// Snapshots the resting orderbook for `market` (short collection
/// window — 1.5s is enough for the matching engine to flush its
/// historical-open-orders burst), reads the top-of-book on the side
/// the user will be taking from, applies a basis-points slippage cap,
/// and returns the resulting limit price as a human-readable decimal
/// string fed back into `dispatch_send_order` (which re-scales via
/// the existing `convert_to_pair_decimals` path on the way to the
/// gRPC `SendOrderRequest`).
///
/// Why this is a wrapper: the gasless cross-chain protocol rejects
/// `buy-market` / `sell-market` at the SDK layer (see
/// `gasless::resolve_order` — true market orders can't pre-commit a
/// lock amount the contract will verify). Marketable-limit is the
/// supported equivalent — it commits the user to an explicit price
/// ceiling / floor that the contract can verify, and the slippage
/// cap is how the user controls "how aggressively will I cross the
/// spread".
fn resolve_marketable_price(
    executor: &DirectExecutor,
    client: &AspensClient,
    market_id: &str,
    side: Side,
    slippage_bps: u32,
) -> Result<String> {
    let stack_url = client.stack_url().to_string();

    // Need the market's pair_decimals to format the raw pair-scale
    // price back to a human-readable string. `dispatch_send_order`
    // will re-scale via `convert_to_pair_decimals` on the way out;
    // round-tripping through human-readable form keeps the API
    // surface consistent with what users see from the buy-limit /
    // sell-limit commands.
    let config = executor
        .execute(aspens::commands::config::get_config(stack_url.clone()))
        .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
    let market = send_order::lookup_market(&config, market_id)
        .map_err(|e| eyre::eyre!("lookup market {market_id}: {e}"))?;
    let pair_decimals = market.pair_decimals as u32;

    let collection_window = std::time::Duration::from_millis(1_500);
    let top = executor
        .execute(stream_orderbook::fetch_top_of_book(
            stack_url,
            market.market_id.clone(),
            collection_window,
        ))
        .map_err(|e| eyre::eyre!(format_error(&e, "fetch top-of-book")))?;

    let (is_buy, reference, label) = match side {
        Side::Bid => (
            true,
            top.best_ask.ok_or_else(|| {
                eyre::eyre!(
                    "no resting asks on {} — cannot compute a marketable buy price. \
                         Place a limit buy at a price you're willing to pay.",
                    market_id
                )
            })?,
            "best ask",
        ),
        Side::Ask => (
            false,
            top.best_bid.ok_or_else(|| {
                eyre::eyre!(
                    "no resting bids on {} — cannot compute a marketable sell price. \
                         Place a limit sell at a price you're willing to accept.",
                    market_id
                )
            })?,
            "best bid",
        ),
        Side::Unspecified => return Err(eyre::eyre!("side must be Bid or Ask")),
    };

    let raw_capped = stream_orderbook::apply_slippage(reference, slippage_bps, is_buy)
        .map_err(|e| eyre::eyre!("apply slippage: {e}"))?;
    let price = aspens::decimals::format_decimal_amount(raw_capped, pair_decimals);
    info!(
        "marketable price resolved: {} = {} (raw {}), slippage cap {} bps -> limit price {} (raw {})",
        label, reference, reference, slippage_bps, price, raw_capped
    );
    Ok(price)
}

/// Local thin wrapper over [`aspens_cliutil::resolve_token_amount`].
/// Kept so existing call sites don't have to change.
fn resolve_token_amount(
    config: &GetConfigResponse,
    network: &str,
    token_symbol: &str,
    amount: &str,
) -> Result<u64> {
    aspens_cliutil::resolve_token_amount(config, network, token_symbol, amount)
}

/// Print the transaction-hash footer that all order/cancel commands share.
fn log_tx_hashes(formatted: &[String]) {
    if formatted.is_empty() {
        return;
    }
    info!("Transaction hashes:");
    for hash in formatted {
        info!("  {}", hash);
    }
    info!("Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)");
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
    verbose: clap_verbosity::Verbosity<clap_verbosity::InfoLevel>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Parser)]
// `verify-attestation` carries many optional measurement/policy args, making its
// variant larger than the others. Subcommands are parsed once; boxing the fields
// would only fight clap's derive for no real benefit.
#[allow(clippy::large_enum_variant)]
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
        /// Amount in human-readable units (e.g., "10", "10.5"). Scaled
        /// by the token's `decimals` from the chain config.
        amount: String,
    },
    /// Withdraw tokens to a local wallet (requires NETWORK TOKEN AMOUNT)
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
        /// Guarantees you pay the maker side of the fee schedule and
        /// never accidentally take. Arborter returns FAILED_PRECONDITION
        /// (no on-chain lock, no gas spent) if the price would cross.
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
    /// Marketable BUY: snapshot the resting book, cap slippage off the
    /// best ask, submit as a buy-limit. The gasless cross-chain
    /// protocol rejects true market orders (no honest amount to sign at
    /// price-unknown time) — this helper turns "take the top of book
    /// with a 0.5% slippage cap" into the equivalent priced order.
    BuyMarketable {
        /// Market ID to trade on
        market: String,
        /// Amount to buy (human-readable)
        amount: String,
        /// Maximum slippage above best ask, in basis points
        /// (10_000 = 100%). Default 50 = 0.5%.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
    },
    /// Marketable SELL: snapshot the resting book, cap slippage off
    /// the best bid, submit as a sell-limit. See `buy-marketable` for
    /// the rationale.
    SellMarketable {
        /// Market ID to trade on
        market: String,
        /// Amount to sell (human-readable)
        amount: String,
        /// Maximum slippage below best bid, in basis points
        /// (10_000 = 100%). Default 50 = 0.5%.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
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
    /// Verify a signer's TDX attestation, fail-closed: DCAP quote/TCB check, then
    /// pinned measurements, then the REPORTDATA binding (tx pubkeys + images +
    /// nonce). The quote is fetched from the stack (or read with --quote); its DCAP
    /// collateral is fetched from a PCCS (or read with --collateral).
    VerifyAttestation {
        /// Expected tx pubkey the quote must bind, as `<curve>:<hex>` where curve is
        /// `secp256k1`/`evm` or `ed25519`/`solana`. Repeatable (one per chain key).
        /// Operator-known and supplied out of band — never read from the attested
        /// stack (that would be circular). Raw pubkey bytes (65-byte uncompressed
        /// secp256k1 / 32-byte Ed25519), matching the signer's manifest.
        #[arg(long = "expected-pubkey", value_name = "CURVE:HEX")]
        expected_pubkey: Vec<String>,
        /// Pinned MRTD (48-byte hex). Pinning MRTD + the RTMRs is effectively
        /// mandatory — a valid signature over *some* TD is not enough.
        #[arg(long)]
        mr_td: Option<String>,
        /// Pinned RTMR[0] (48-byte hex).
        #[arg(long)]
        rtmr0: Option<String>,
        /// Pinned RTMR[1] (48-byte hex).
        #[arg(long)]
        rtmr1: Option<String>,
        /// Pinned RTMR[2] (48-byte hex).
        #[arg(long)]
        rtmr2: Option<String>,
        /// Pinned RTMR[3] (48-byte hex).
        #[arg(long)]
        rtmr3: Option<String>,
        /// Pinned MRSEAM (48-byte hex).
        #[arg(long)]
        mr_seam: Option<String>,
        /// Pinned MRSIGNERSEAM (48-byte hex).
        #[arg(long)]
        mr_signer_seam: Option<String>,
        /// Pinned TD attributes (8-byte hex).
        #[arg(long)]
        td_attributes: Option<String>,
        /// Pinned XFAM (8-byte hex).
        #[arg(long)]
        xfam: Option<String>,
        /// Expected running image digest(s) bound in REPORTDATA (hex). Default: empty.
        #[arg(long)]
        image_digest: Option<String>,
        /// REPORTDATA nonce (hex) the quote binds. Fetching from the stack: a fresh
        /// random 32-byte nonce is minted if omitted. With --quote: defaults to empty.
        #[arg(long)]
        nonce: Option<String>,
        /// Read the raw TD quote from a file (hex text or raw binary) instead of
        /// fetching it from the stack.
        #[arg(long, value_name = "FILE")]
        quote: Option<PathBuf>,
        /// Read DCAP collateral from a JSON file (QuoteCollateralV3) instead of
        /// fetching it from a PCCS — for air-gapped / offline verification.
        #[arg(long, value_name = "FILE")]
        collateral: Option<PathBuf>,
        /// PCCS base URL to fetch collateral from (default: Phala's public PCCS).
        #[arg(long, default_value = "https://pccs.phala.network")]
        pccs_url: String,
        /// Acceptable TCB status (repeatable). Default: UpToDate only. OutOfDate /
        /// Revoked must never be allow-listed.
        #[arg(long = "accept-tcb", value_name = "STATUS")]
        accept_tcb: Vec<String>,
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

    match cli.command {
        Commands::Deposit {
            network,
            token,
            amount,
        } => {
            info!("Depositing {amount} {token} on {network}");

            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let amount_base = resolve_token_amount(&config, &network, &token, &amount)?;
            let wallet = load_trader_wallet_for_network(&config, &network)?;
            let context = format!("deposit {} {} on {}", amount, token, network);
            executor
                .execute(async move {
                    deposit::call_deposit_from_config_with_wallet(
                        network,
                        token,
                        amount_base,
                        &wallet,
                        config,
                    )
                    .await
                })
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

            info!("Deposit was successful");
        }
        Commands::Withdraw {
            network,
            token,
            amount,
        } => {
            info!("Withdrawing {amount} {token} from {network}");

            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let amount_base = resolve_token_amount(&config, &network, &token, &amount)?;
            let wallet = load_trader_wallet_for_network(&config, &network)?;
            let context = format!("withdraw {} {} from {}", amount, token, network);
            executor
                .execute(async move {
                    withdraw::call_withdraw_from_config_with_wallet(
                        stack_url,
                        network,
                        token,
                        amount_base,
                        &wallet,
                        config,
                    )
                    .await
                })
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

            info!("Withdraw was successful");
        }
        Commands::BuyMarket { market, amount } => {
            info!("Sending market BUY order for {amount} on market {market}");
            let result =
                dispatch_send_order(&executor, &client, market, Side::Bid, amount, None, false)?;
            info!(
                "Market buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::BuyLimit {
            market,
            amount,
            price,
            post_only,
        } => {
            info!(
                "Sending limit BUY order for {amount} at price {price} on market {market} \
                 (post_only={post_only})"
            );
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Bid,
                amount,
                Some(price),
                post_only,
            )?;
            info!(
                "Limit buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::SellMarket { market, amount } => {
            info!("Sending market SELL order for {amount} on market {market}");
            let result =
                dispatch_send_order(&executor, &client, market, Side::Ask, amount, None, false)?;
            info!(
                "Market sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::SellLimit {
            market,
            amount,
            price,
            post_only,
        } => {
            info!(
                "Sending limit SELL order for {amount} at price {price} on market {market} \
                 (post_only={post_only})"
            );
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Ask,
                amount,
                Some(price),
                post_only,
            )?;
            info!(
                "Limit sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::BuyMarketable {
            market,
            amount,
            slippage_bps,
        } => {
            let price =
                resolve_marketable_price(&executor, &client, &market, Side::Bid, slippage_bps)?;
            info!(
                "Sending marketable BUY for {amount} on {market} (slippage cap {} bps -> price {})",
                slippage_bps, price
            );
            // Marketable orders are explicitly designed to cross — post-only
            // would defeat the purpose, so we hard-code false.
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Bid,
                amount,
                Some(price),
                false,
            )?;
            info!(
                "Marketable buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::SellMarketable {
            market,
            amount,
            slippage_bps,
        } => {
            let price =
                resolve_marketable_price(&executor, &client, &market, Side::Ask, slippage_bps)?;
            info!(
                "Sending marketable SELL for {amount} on {market} (slippage cap {} bps -> price {})",
                slippage_bps, price
            );
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Ask,
                amount,
                Some(price),
                false,
            )?;
            info!(
                "Marketable sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.get_formatted_transaction_hashes());
        }
        Commands::CancelOrder {
            market,
            side,
            order_id,
        } => {
            info!("Canceling order {order_id} ({side}) on market {market}");

            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let origin = origin_network_for_side(&config, &market, parse_side(&side)?)?;
            let wallet = load_trader_wallet_for_network(&config, origin)?;
            let context = format!("cancel order {} on {}", order_id, market);
            let result = executor
                .execute(async move {
                    cancel_order::call_cancel_order_from_config_with_wallet(
                        stack_url, market, side, order_id, &wallet, config,
                    )
                    .await
                })
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

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
                info!(
                    "Paste these hashes into your chain's block explorer (e.g., Etherscan, Basescan)"
                );
            }
        }
        Commands::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = client.stack_url().to_string();
            let config = executor
                .execute(config::get_config(stack_url))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            // Chains whose architecture has no matching wallet are rendered
            // with the lib's `error` placeholder; we only require at least one.
            let evm = load_trader_wallet(CurveType::Secp256k1).ok();
            let solana = load_trader_wallet(CurveType::Ed25519).ok();
            if evm.is_none() && solana.is_none() {
                return Err(eyre::eyre!(
                    "No trader wallet configured. Set TRADER_PRIVKEY (EVM) and/or \
                     TRADER_PRIVKEY_SOLANA (Solana) in your .env file."
                ));
            }
            executor
                .execute(async move {
                    let wallets: Vec<&Wallet> = [evm.as_ref(), solana.as_ref()]
                        .into_iter()
                        .flatten()
                        .collect();
                    balance::balance_from_config_with_wallets(config, &wallets).await
                })
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

            let privkey = client.get_env("TRADER_PRIVKEY").cloned().ok_or_else(|| {
                eyre::eyre!(
                    "TRADER_PRIVKEY not found\n\n\
                     Hints:\n\
                     - Set TRADER_PRIVKEY in your .env file\n\
                     - The private key should be a 64-character hex string (without 0x prefix)"
                )
            })?;
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
        Commands::SignerPublicKey { chain_network } => {
            use aspens::commands::config;

            let stack_url = client.stack_url().to_string();
            info!("Fetching signer public key(s) and gas balances from {stack_url}");
            let signer_infos = executor
                .execute(config::get_signer_public_key_with_balances(
                    stack_url,
                    chain_network,
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
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let resolved_market = send_order::lookup_market(&config, &market)
                .map_err(|e| eyre::eyre!(format_error(&e, "look up market")))?;
            let resolved_market_id = resolved_market.market_id.clone();

            let options = stream_orderbook::StreamOrderbookOptions {
                market_id: resolved_market_id,
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
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let resolved_market = send_order::lookup_market(&config, &market)
                .map_err(|e| eyre::eyre!(format_error(&e, "look up market")))?;
            let resolved_market_id = resolved_market.market_id.clone();

            let options = stream_trades::StreamTradesOptions {
                market_id: resolved_market_id,
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
            if let Some(ref data) = report_data_bytes
                && data.len() > 64
            {
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
        Commands::VerifyAttestation {
            expected_pubkey,
            mr_td,
            rtmr0,
            rtmr1,
            rtmr2,
            rtmr3,
            mr_seam,
            mr_signer_seam,
            td_attributes,
            xfam,
            image_digest,
            nonce,
            quote,
            collateral,
            pccs_url,
            accept_tcb,
            output,
        } => {
            use aspens::commands::config;
            use aspens::tdx_verify::collateral::{collateral_from_json, fetch_collateral};
            use aspens::tdx_verify::dcap::DcapQuoteVerifier;
            use aspens::tdx_verify::{ExpectedReportData, MeasurementPolicy, verify_attestation};

            // Expected tx pubkeys (claim 3) — operator-known, supplied out of band.
            if expected_pubkey.is_empty() {
                return Err(eyre::eyre!(
                    "at least one --expected-pubkey is required (the tx pubkey(s) the quote must \
                     bind, supplied out of band — never read from the attested stack)"
                ));
            }
            let pubkeys = expected_pubkey
                .iter()
                .map(|s| parse_expected_pubkey(s))
                .collect::<Result<Vec<_>>>()?;

            // Measurement policy (claim 2).
            let policy = MeasurementPolicy {
                mr_td: parse_fixed("mr-td", &mr_td)?,
                rt_mr: [
                    parse_fixed("rtmr0", &rtmr0)?,
                    parse_fixed("rtmr1", &rtmr1)?,
                    parse_fixed("rtmr2", &rtmr2)?,
                    parse_fixed("rtmr3", &rtmr3)?,
                ],
                mr_seam: parse_fixed("mr-seam", &mr_seam)?,
                mr_signer_seam: parse_fixed("mr-signer-seam", &mr_signer_seam)?,
                td_attributes: parse_fixed("td-attributes", &td_attributes)?,
                xfam: parse_fixed("xfam", &xfam)?,
            };
            if policy.mr_td.is_none() && policy.rt_mr.iter().all(|m| m.is_none()) {
                eprintln!(
                    "warning: no MRTD/RTMR pinned (--mr-td/--rtmr*); any genuine TDX TD whose \
                     REPORTDATA matches will pass. Pin measurements for a meaningful check."
                );
            }

            let image_digests = match &image_digest {
                Some(s) => parse_hex("image-digest", s)?,
                None => Vec::new(),
            };

            // REPORTDATA nonce: explicit, else a fresh random nonce when we fetch
            // the quote live, else empty for an offline --quote.
            let nonce_bytes = match &nonce {
                Some(s) => parse_hex("nonce", s)?,
                None if quote.is_none() => {
                    let mut buf = [0u8; 32];
                    getrandom::fill(&mut buf).map_err(|e| eyre::eyre!("generating nonce: {e}"))?;
                    info!("minted fresh anti-replay nonce: {}", hex::encode(buf));
                    buf.to_vec()
                }
                None => Vec::new(),
            };
            if nonce_bytes.len() > 64 {
                return Err(eyre::eyre!(
                    "--nonce is {} bytes; the REPORTDATA input is at most 64",
                    nonce_bytes.len()
                ));
            }

            let accepted_tcb = if accept_tcb.is_empty() {
                vec!["UpToDate".to_string()]
            } else {
                accept_tcb.clone()
            };

            // Read file inputs up front so the async block stays Send + 'static.
            let quote_from_file = match &quote {
                Some(p) => Some(read_quote_file(p)?),
                None => None,
            };
            let collateral_json = match &collateral {
                Some(p) => Some(
                    std::fs::read_to_string(p)
                        .map_err(|e| eyre::eyre!("reading collateral file {}: {e}", p.display()))?,
                ),
                None => None,
            };

            let stack_url = client.stack_url().to_string();
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| eyre::eyre!("system clock is before the unix epoch: {e}"))?
                .as_secs();

            // The verifier-chosen nonce is bound on the stack side, so fetch with a
            // clone and keep the original as the expected REPORTDATA input.
            let nonce_for_request = nonce_bytes.clone();
            let result = executor.execute(async move {
                // 1. Raw quote: from the stack unless --quote supplied one.
                let raw_quote = match quote_from_file {
                    Some(q) => q,
                    None => {
                        let resp =
                            config::get_attestation(stack_url, Some(nonce_for_request)).await?;
                        resp.report
                            .ok_or_else(|| eyre::eyre!("stack returned no attestation report"))?
                            .raw_quote
                    }
                };
                if raw_quote.is_empty() {
                    return Err(eyre::eyre!(
                        "quote is empty — the signer produced no TD Quote (is TDX active?)"
                    ));
                }

                // 2. Collateral: from --collateral file, else fetched from the PCCS.
                let collateral = match collateral_json {
                    Some(j) => collateral_from_json(&j)?,
                    None => fetch_collateral(&pccs_url, &raw_quote).await?,
                };

                // 3. Verify fail-closed: DCAP+TCB -> measurements -> REPORTDATA.
                let verifier =
                    DcapQuoteVerifier::new(collateral, now_secs).accept_tcb_statuses(accepted_tcb);
                let expected = ExpectedReportData {
                    pubkeys,
                    image_digests,
                    report_data: nonce_bytes,
                };
                let verified = verify_attestation(&raw_quote, &verifier, &policy, &expected)?;
                Ok::<_, eyre::Report>(verified)
            });

            let verified =
                result.map_err(|e| eyre::eyre!(format_error(&e, "verify attestation")))?;

            match output.as_str() {
                "json" => {
                    let json = serde_json::json!({
                        "verified": true,
                        "mr_td": hex::encode(verified.mr_td),
                        "rt_mr": [
                            hex::encode(verified.rt_mr[0]),
                            hex::encode(verified.rt_mr[1]),
                            hex::encode(verified.rt_mr[2]),
                            hex::encode(verified.rt_mr[3]),
                        ],
                        "mr_seam": hex::encode(verified.mr_seam),
                        "mr_signer_seam": hex::encode(verified.mr_signer_seam),
                        "td_attributes": hex::encode(verified.td_attributes),
                        "xfam": hex::encode(verified.xfam),
                        "report_data": hex::encode(verified.report_data),
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                _ => {
                    println!(
                        "✓ attestation verified (DCAP chain + TCB, measurement policy, REPORTDATA)"
                    );
                    println!("  MRTD:          {}", hex::encode(verified.mr_td));
                    println!("  RTMR[0]:       {}", hex::encode(verified.rt_mr[0]));
                    println!("  RTMR[1]:       {}", hex::encode(verified.rt_mr[1]));
                    println!("  RTMR[2]:       {}", hex::encode(verified.rt_mr[2]));
                    println!("  RTMR[3]:       {}", hex::encode(verified.rt_mr[3]));
                    println!("  MRSEAM:        {}", hex::encode(verified.mr_seam));
                    println!("  MRSIGNERSEAM:  {}", hex::encode(verified.mr_signer_seam));
                    println!("  TD attributes: {}", hex::encode(verified.td_attributes));
                    println!("  XFAM:          {}", hex::encode(verified.xfam));
                    println!("  REPORTDATA:    {}", hex::encode(verified.report_data));
                }
            }
        }
    }

    Ok(())
}
