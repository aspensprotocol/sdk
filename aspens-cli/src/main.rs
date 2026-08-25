use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::arborter_pb::{SendOrderResponse, Side};
#[cfg(feature = "fce")]
use aspens::commands::trading::fce_actions;
use aspens::commands::trading::send_order::{origin_network_for_side, parse_side};
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
/// Order-flag pair threaded from the command arms into the library call.
/// Named-field construction at each arm keeps the two same-typed bools
/// from ever being transposed positionally, and a future flag is one new
/// field here instead of another positional bool at every call site.
#[derive(Clone, Copy)]
struct OrderFlags {
    /// Reject the order if it would cross at submission (limit only).
    post_only: bool,
    /// Invisible order: excluded from public book exposure; fills print
    /// with this side redacted.
    hidden: bool,
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
#[derive(clap::Args, Clone, Debug, Default)]
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

/// What the CLI needs back from a submitted order, normalized across the two
/// transports.
///
/// gRPC answers with a `SendOrderResponse`; FCE answers with a
/// `PlaceOrderResponse`, which carries no transaction hashes — trading is
/// off-chain until settlement, so there is nothing to link to an explorer. The
/// hash list is simply empty there rather than fabricated.
struct SentOrder {
    /// `0x`-prefixed hex, pre-formatted here so every downstream `{}` print
    /// site stays unchanged from when this carried a `u64`.
    order_id: String,
    tx_hashes: Vec<String>,
}

impl From<SendOrderResponse> for SentOrder {
    fn from(r: SendOrderResponse) -> Self {
        Self {
            order_id: format!("0x{}", hex::encode(&r.order_id)),
            tx_hashes: r.get_formatted_transaction_hashes(),
        }
    }
}

/// The side string `fce_actions` parses ("bid"/"ask").
#[cfg(feature = "fce")]
fn fce_side_str(side: Side) -> Result<&'static str> {
    match side {
        Side::Bid => Ok("bid"),
        Side::Ask => Ok("ask"),
        Side::Unspecified => Err(eyre::eyre!("side must be Bid or Ask")),
    }
}

/// Reject flags the direct-action wire cannot express.
///
/// `--hidden` is the only one: the FCE `PlaceOrderRequest` carries no `hidden`
/// field and the adapter rebuilds the order with `hidden=false`, so accepting
/// the flag here would rest the order VISIBLY while the user believes it is
/// hidden. That is a disclosure, not a degraded feature — refuse instead.
#[cfg(feature = "fce")]
fn check_flags_supported_over_fce(flags: OrderFlags) -> Result<()> {
    if flags.hidden {
        return Err(eyre::eyre!(
            "--hidden is not supported over the FCE transport: the direct-action wire \
             carries no `hidden` field, so the order would rest visibly. Submit it \
             against an arborter reachable over gRPC, or drop --hidden."
        ));
    }
    Ok(())
}

/// Reject `--match-order-id` over the FCE transport: the direct-action
/// `PlaceOrderRequest` the payload builder constructs carries no
/// `matching_order_ids` field, so a dealroom discretionary fill cannot be
/// expressed there at all — refuse rather than silently submitting a
/// normal (non-discretionary) order the caller didn't ask for.
#[cfg(feature = "fce")]
fn check_match_order_ids_supported_over_fce(match_order_ids: &[[u8; 32]]) -> Result<()> {
    if !match_order_ids.is_empty() {
        eyre::bail!("discretionary orders are not supported over the FCE transport");
    }
    Ok(())
}

/// Reject `--base-address` / `--quote-address` over the FCE transport: the
/// payload builder derives both account addresses from the wallets, so an
/// override would be silently DROPPED — and a silently ignored settlement
/// address is exactly the kind of surprise this flag pair exists to
/// prevent.
#[cfg(feature = "fce")]
fn check_settle_args_supported_over_fce(settle: &SettleArgs) -> Result<()> {
    if settle.base_address.is_some() || settle.quote_address.is_some() {
        eyre::bail!(
            "--base-address / --quote-address are not supported over the FCE transport: \
             the direct-action wire derives both account addresses from the wallets, so \
             the override would be ignored rather than honored"
        );
    }
    Ok(())
}

/// `quote_budget` is the human-readable maximum QUOTE a market buy may spend,
/// and belongs to exactly that one cell of the order table: a sell's budget is
/// its `amount` (base) and a limit buy's is `amount x price`, both derived from
/// what is already signed, so those pass `None` and the library rejects a
/// budget on them.
#[allow(clippy::too_many_arguments)]
async fn dispatch_send_order(
    executor: &DirectExecutor,
    client: &AspensClient,
    market: String,
    side: Side,
    amount: String,
    price: Option<String>,
    flags: OrderFlags,
    quote_budget: Option<String>,
    match_order_ids: Vec<[u8; 32]>,
    settle: SettleArgs,
) -> Result<SentOrder> {
    // Reject flags this transport can't express BEFORE any network work — an
    // unsupported argument should not cost a config round-trip to discover, and
    // over FCE that round-trip is a queued direct action.
    #[cfg(feature = "fce")]
    if client.uses_fce() {
        check_flags_supported_over_fce(flags)?;
        check_match_order_ids_supported_over_fce(&match_order_ids)?;
        check_settle_args_supported_over_fce(&settle)?;
    }

    let stack_url = client.stack_url().to_string();
    let config = client
        .get_config()
        .await
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

    // FCE stacks run the arborter inside a Confidential Space image with no
    // reachable gRPC, so route the same signed envelope through the ext-proxy
    // instead. Signing is shared (`trading::sign_encoded`) — only the transport
    // differs.
    #[cfg(feature = "fce")]
    if client.uses_fce() {
        check_flags_supported_over_fce(flags)?;
        check_match_order_ids_supported_over_fce(&match_order_ids)?;
        check_settle_args_supported_over_fce(&settle)?;
        let wallets: Vec<&Wallet> = [evm.as_ref(), solana.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        let outcome = fce_actions::place_order(
            client,
            &wallets,
            &market,
            fce_side_str(side)?,
            &amount,
            price.as_deref(),
            flags.post_only,
        )
        .await
        .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
        if !outcome.ok() {
            return Err(eyre::eyre!(format_error(
                &eyre::eyre!("{}", outcome.log),
                &context
            )));
        }
        let resp = outcome
            .into_data()
            .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
        info!(
            "FCE order accepted (resting in book: {}, immediate fills: {})",
            resp.order_in_book, resp.fills
        );
        return Ok(SentOrder {
            order_id: resp.order_id,
            tx_hashes: vec![],
        });
    }

    let resp = executor
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
                flags.post_only,
                flags.hidden,
                quote_budget,
                match_order_ids,
                settle.base_address,
                settle.quote_address,
            )
            .await
        })
        .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
    if let Some(line) = resp.settlement_summary() {
        info!("{line}");
    }
    Ok(SentOrder::from(resp))
}

/// Top-of-book via the gRPC orderbook stream. The 1.5s window is enough for the
/// matching engine to flush its historical-open-orders burst; live updates after
/// the deadline are ignored.
fn grpc_top_of_book(
    executor: &DirectExecutor,
    stack_url: String,
    market_id: String,
) -> Result<stream_orderbook::TopOfBook> {
    executor
        .execute(stream_orderbook::fetch_top_of_book(
            stack_url,
            market_id,
            std::time::Duration::from_millis(1_500),
        ))
        .map_err(|e| eyre::eyre!(format_error(&e, "fetch top-of-book")))
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
/// Why this exists alongside `buy-market` / `sell-market`: those bound the
/// order by a BUDGET (quote for a buy, base for a sell), which caps the
/// spend but says nothing about the price paid. Marketable-limit caps the
/// PRICE instead, and the slippage cap is how the user controls "how
/// aggressively will I cross the spread".
async fn resolve_marketable_price(
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
    let config = client
        .get_config()
        .await
        .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
    let market = send_order::lookup_market(&config, market_id)
        .map_err(|e| eyre::eyre!(format_error(&e, &format!("look up market {market_id}"))))?;
    let pair_decimals = market.pair_decimals as u32;

    // Over FCE the book arrives as a one-shot snapshot, so there is no stream to
    // drain and no collection window to wait out. Both branches produce the same
    // `TopOfBook`, so the slippage math below is shared verbatim.
    #[cfg(feature = "fce")]
    let top = if client.uses_fce() {
        fce_actions::top_of_book(client, &market.market_id)
            .await
            .map_err(|e| eyre::eyre!(format_error(&e, "fetch top-of-book")))?
    } else {
        grpc_top_of_book(executor, stack_url, market.market_id.clone())?
    };
    #[cfg(not(feature = "fce"))]
    let top = grpc_top_of_book(executor, stack_url, market.market_id.clone())?;

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
) -> Result<u128> {
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
        /// Solana WSOL (native SOL) only: keep the withdrawn funds as WSOL
        /// instead of unwrapping. By default the WSOL ATA is closed after the
        /// withdraw, converting its ENTIRE wrapped balance + rent back to SOL.
        #[arg(long, default_value_t = false)]
        no_unwrap: bool,
    },
    /// Send a market BUY order (executes at best available price)
    BuyMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
        /// REQUIRED: the maximum QUOTE you are prepared to spend,
        /// human-readable (e.g. "250.5"), scaled by the market's
        /// quote-token decimals. A market buy gives quote and has no
        /// price to size that with, so this — not `amount` — is what
        /// bounds the spend and what gets collateralised.
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
        /// Guarantees you pay the maker side of the fee schedule and
        /// never accidentally take. Arborter returns FAILED_PRECONDITION
        /// (no on-chain lock, no gas spent) if the price would cross.
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
    /// Marketable BUY: snapshot the resting book, cap slippage off the
    /// best ask, submit as a buy-limit. Turns "take the top of book with a
    /// 0.5% slippage cap" into the equivalent priced order — use this when
    /// you want to bound the PRICE; use `buy-market --quote-budget` when you
    /// want to bound the SPEND.
    BuyMarketable {
        /// Market ID to trade on
        market: String,
        /// Amount to buy (human-readable)
        amount: String,
        /// Maximum slippage above best ask, in basis points
        /// (10_000 = 100%). Default 50 = 0.5%.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
        /// Invisible order: the synthesized limit order is hidden — fills
        /// print with your side redacted, and any unfilled remainder
        /// rests invisibly (track it via the returned order id).
        #[arg(long, default_value_t = false)]
        hidden: bool,
        /// Settlement-address overrides (see the flags' own help).
        #[command(flatten)]
        settle: SettleArgs,
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
        /// Invisible order: see `buy-marketable --hidden`.
        #[arg(long, default_value_t = false)]
        hidden: bool,
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
        /// Optional hex-encoded freshness nonce bound into the quote's REPORTDATA
        /// (any length -- it is pre-hashed; 32 random bytes is conventional)
        #[arg(long)]
        nonce: Option<String>,
        /// Write the raw TD quote to a file (binary) for offline verification
        /// (feed it to `verify-attestation --quote`)
        #[arg(long, value_name = "FILE")]
        save_quote: Option<std::path::PathBuf>,
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
        /// Pinned RTMR0 (48-byte hex).
        #[arg(long)]
        rtmr0: Option<String>,
        /// Pinned RTMR1 (48-byte hex).
        #[arg(long)]
        rtmr1: Option<String>,
        /// Pinned RTMR2 (48-byte hex).
        #[arg(long)]
        rtmr2: Option<String>,
        /// Pinned RTMR3 (48-byte hex).
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
    /// Arm the per-token per-epoch withdrawal cap on a Solana instance,
    /// signing DIRECTLY with the offline operator-admin key.
    ///
    /// Semantics you need before using this:
    ///
    /// * CAP = 0 MEANS UNLIMITED — the shipped default for every mint, and
    ///   the same sentinel EVM's MidribV3.setWithdrawEpochCap uses. Passing 0
    ///   disarms the cap; it does not block withdrawals.
    ///
    /// * The cap is per (instance, mint) and per epoch. An epoch is 9,000
    ///   slots, roughly 1 hour. It is per TOKEN because one scalar cannot be
    ///   sane across a 6-decimal and an 18-decimal mint at once.
    ///
    /// * The window is TUMBLING, not sliding: the running total resets at the
    ///   epoch boundary, so a withdrawal of the full cap just before a
    ///   rollover and another just after puts 2x CAP out inside one hour. To
    ///   guarantee at most X per hour, SET CAP = X/2. EVM behaves identically.
    ///
    /// * The signer must equal the instance's on-chain `operator_admin`
    ///   (checked before submitting). IF THAT IS THE ARBORTER'S OWN KEY — the
    ///   default the Solana deployer takes when no operator admin is
    ///   configured — THE CAP PROVIDES NO CONTAINMENT against a compromised
    ///   signer, which can simply raise it again. It still bounds bugs and
    ///   operator error. The command warns when it detects that shape.
    ///
    /// Reads the key from OPERATOR_ADMIN_PRIVKEY_SOLANA (base58 or JSON byte
    /// array) — deliberately NOT the trader or admin key. Never goes through
    /// the arborter.
    SetWithdrawEpochCap {
        /// The Solana network name (e.g. solana-local, solana-devnet)
        network: String,
        /// Token symbol the cap applies to (e.g. USDC, WSOL)
        token: String,
        /// Cap in human-readable units (e.g. "50000", "12.5"), scaled by the
        /// token's `decimals` from the chain config. "0" = UNLIMITED.
        cap: String,
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
    // Best-effort: failing here only means logs aren't captured (e.g. a
    // subscriber is already set in-process) — don't abort the command over it.
    let _ = tracing::subscriber::set_global_default(subscriber);

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

            let config = client
                .get_config()
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let context = format!("deposit {} {} on {}", amount, token, network);
            let amount_base = resolve_token_amount(&config, &network, &token, &amount)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
            let wallet = load_trader_wallet_for_network(&config, &network)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
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
            no_unwrap,
        } => {
            info!("Withdrawing {amount} {token} from {network}");

            let stack_url = client.stack_url().to_string();
            let config = client
                .get_config()
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let context = format!("withdraw {} {} from {}", amount, token, network);
            let amount_base = resolve_token_amount(&config, &network, &token, &amount)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
            let wallet = load_trader_wallet_for_network(&config, &network)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
            let opts = withdraw::WithdrawOpts {
                unwrap_native: !no_unwrap,
            };

            // Only the voucher request goes over FCE; the on-chain submit that
            // follows it is the same code either way.
            #[cfg(feature = "fce")]
            if let Some(fce) = client.fce() {
                withdraw::call_withdraw_from_config_with_fce_opts(
                    fce,
                    network,
                    token,
                    amount_base,
                    &wallet,
                    config,
                    opts,
                )
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
                info!("Withdraw was successful");
                return Ok(());
            }

            executor
                .execute(async move {
                    withdraw::call_withdraw_from_config_with_wallet_opts(
                        stack_url,
                        network,
                        token,
                        amount_base,
                        &wallet,
                        config,
                        opts,
                    )
                    .await
                })
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

            info!("Withdraw was successful");
        }
        Commands::BuyMarket {
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
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Bid,
                amount,
                None,
                OrderFlags {
                    post_only: false, // meaningless for market orders
                    hidden,
                },
                Some(quote_budget),
                vec![], // dealroom discretionary requires a limit price; not offered here
                settle,
            )
            .await?;
            info!(
                "Market buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::BuyLimit {
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
            let match_order_ids = match_order_ids
                .iter()
                .map(|s| aspens_cliutil::parse_order_id("match-order-id", s))
                .collect::<Result<Vec<_>>>()?;
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Bid,
                amount,
                Some(price),
                OrderFlags { post_only, hidden },
                None, // a limit order's budget is derived from (amount, price)
                match_order_ids,
                settle,
            )
            .await?;
            info!(
                "Limit buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::SellMarket {
            market,
            amount,
            hidden,
            settle,
        } => {
            info!("Sending market SELL order for {amount} on market {market} (hidden={hidden})");
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Ask,
                amount,
                None,
                OrderFlags {
                    post_only: false, // meaningless for market orders
                    hidden,
                },
                None,   // an ASK gives base: its budget IS its quantity
                vec![], // dealroom discretionary requires a limit price; not offered here
                settle,
            )
            .await?;
            info!(
                "Market sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::SellLimit {
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
            let match_order_ids = match_order_ids
                .iter()
                .map(|s| aspens_cliutil::parse_order_id("match-order-id", s))
                .collect::<Result<Vec<_>>>()?;
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Ask,
                amount,
                Some(price),
                OrderFlags { post_only, hidden },
                None, // a limit order's budget is derived from (amount, price)
                match_order_ids,
                settle,
            )
            .await?;
            info!(
                "Limit sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::BuyMarketable {
            market,
            amount,
            slippage_bps,
            hidden,
            settle,
        } => {
            let price =
                resolve_marketable_price(&executor, &client, &market, Side::Bid, slippage_bps)
                    .await?;
            info!(
                "Sending marketable BUY for {amount} on {market} (slippage cap {} bps -> price {}, hidden={})",
                slippage_bps, price, hidden
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
                OrderFlags {
                    post_only: false,
                    hidden,
                },
                None,   // priced, so the budget is derived from (amount, price)
                vec![], // dealroom discretionary is not offered on the marketable path
                settle,
            )
            .await?;
            info!(
                "Marketable buy order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::SellMarketable {
            market,
            amount,
            slippage_bps,
            hidden,
            settle,
        } => {
            let price =
                resolve_marketable_price(&executor, &client, &market, Side::Ask, slippage_bps)
                    .await?;
            info!(
                "Sending marketable SELL for {amount} on {market} (slippage cap {} bps -> price {}, hidden={})",
                slippage_bps, price, hidden
            );
            let result = dispatch_send_order(
                &executor,
                &client,
                market,
                Side::Ask,
                amount,
                Some(price),
                OrderFlags {
                    post_only: false,
                    hidden,
                },
                None,   // priced, so the budget is derived from (amount, price)
                vec![], // dealroom discretionary is not offered on the marketable path
                settle,
            )
            .await?;
            info!(
                "Marketable sell order sent successfully (order_id: {})",
                result.order_id
            );
            log_tx_hashes(&result.tx_hashes);
        }
        Commands::CancelOrder {
            market,
            side,
            order_id,
        } => {
            let order_id = aspens_cliutil::parse_order_id("order_id", &order_id)?;
            let order_id_hex = format!("0x{}", hex::encode(order_id));
            info!("Canceling order {order_id_hex} ({side}) on market {market}");

            let stack_url = client.stack_url().to_string();
            let config = client
                .get_config()
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let context = format!("cancel order {} on {}", order_id_hex, market);
            let origin = origin_network_for_side(&config, &market, parse_side(&side)?)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
            let wallet = load_trader_wallet_for_network(&config, origin)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

            #[cfg(feature = "fce")]
            if client.uses_fce() {
                let outcome = fce_actions::cancel_order_from_config(
                    &client, &wallet, &market, &side, order_id, &config,
                )
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
                if !outcome.ok() {
                    return Err(eyre::eyre!(format_error(
                        &eyre::eyre!("{}", outcome.log),
                        &context
                    )));
                }
                let canceled = outcome
                    .into_data()
                    .map_err(|e| eyre::eyre!(format_error(&e, &context)))?
                    .canceled;
                if canceled {
                    info!("Order {} canceled successfully", order_id_hex);
                } else {
                    info!("Order {} was not found or already canceled", order_id_hex);
                }
                // No transaction hashes over FCE — cancels are off-chain.
                return Ok(());
            }

            let result = executor
                .execute(async move {
                    cancel_order::call_cancel_order_from_config_with_wallet(
                        stack_url, market, side, order_id, &wallet, config,
                    )
                    .await
                })
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;

            if result.order_canceled {
                info!("Order {} canceled successfully", order_id_hex);
            } else {
                // The arborter answered NOT_FOUND: the order is no longer
                // live in the book (replayed cancel, or racing a fill that
                // just landed). `cancel_order::call_cancel_order_with_wallet`
                // classifies that as this outcome rather than an error — the
                // order is gone and its collateral released either way.
                info!(
                    "Order {} already gone (filled or previously cancelled)",
                    order_id_hex
                );
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
            info!("Fetching balances for all tokens across all chains");
            let config = client
                .get_config()
                .await
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
            let stack_url = client.stack_url().to_string();
            info!("Fetching configuration from {stack_url}");
            let config = client
                .get_config()
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            // If output_file is provided, save to file
            if let Some(ref path) = output_file {
                executor
                    .execute(aspens::commands::config::download_config(
                        stack_url.clone(),
                        path.clone(),
                    ))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("save configuration to '{}'", path)
                        ))
                    })?;
                info!("Configuration saved to: {}", path);
            } else {
                // Display config as JSON
                let json = serde_json::to_string_pretty(&config)
                    .map_err(|e| eyre::eyre!("failed to format configuration as JSON: {e}"))?;
                println!("{}", json);
            }
        }
        Commands::SignerPublicKey { chain_network } => {
            let stack_url = client.stack_url().to_string();
            info!("Fetching signer public key(s) and gas balances from {stack_url}");
            let signer_infos = executor
                .execute(
                    aspens::commands::config::get_signer_public_key_with_balances(
                        stack_url,
                        chain_network,
                    ),
                )
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
            let config = client
                .get_config()
                .await
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
            let config = client
                .get_config()
                .await
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
            nonce,
            save_quote,
            output,
        } => {
            info!("Fetching TEE attestation from signer");

            let stack_url = client.stack_url().to_string();

            let nonce_bytes = match nonce {
                Some(hex_data) => {
                    let hex_data = hex_data.strip_prefix("0x").unwrap_or(&hex_data);
                    Some(hex::decode(hex_data).map_err(|e| {
                        eyre::eyre!(
                            "Invalid hex data for --nonce: {}\n\n\
                             Hint: provide the nonce as a hex string (with or without 0x prefix)",
                            e
                        )
                    })?)
                }
                None => None,
            };

            let response = executor
                .execute(aspens::commands::config::get_attestation(
                    stack_url,
                    nonce_bytes,
                ))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch TEE attestation")))?;

            let report = response.report;
            if let (Some(path), Some(report)) = (&save_quote, &report) {
                if report.raw_quote.is_empty() {
                    return Err(eyre::eyre!(
                        "signer returned an empty TD quote; nothing to save to {}",
                        path.display()
                    ));
                }
                std::fs::write(path, &report.raw_quote)
                    .map_err(|e| eyre::eyre!("writing quote to {}: {e}", path.display()))?;
                info!("raw TD quote saved to {}", path.display());
            }

            match output.as_str() {
                "json" => match &report {
                    Some(report) => println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &aspens::commands::config::attestation_report_json(report)
                        )
                        .map_err(|e| eyre::eyre!(
                            "failed to format attestation report as JSON: {e}"
                        ))?
                    ),
                    None => println!("null"),
                },
                _ => match &report {
                    Some(report) => print!(
                        "{}",
                        aspens::commands::config::format_attestation_report(report)
                    ),
                    None => println!("No attestation report available"),
                },
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
                        let resp = aspens::commands::config::get_attestation(
                            stack_url,
                            Some(nonce_for_request),
                        )
                        .await?;
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
                    nonce: nonce_bytes,
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
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).map_err(|e| eyre::eyre!(
                            "failed to format attestation report as JSON: {e}"
                        ))?
                    );
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
        Commands::SetWithdrawEpochCap {
            network,
            token,
            cap,
        } => {
            let config = client
                .get_config()
                .await
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;
            let context = format!("set withdraw epoch cap for {} on {}", token, network);

            // Human units -> the mint's base units, the scale the program
            // accumulates withdrawals in. `0` survives as `0` = UNLIMITED.
            let cap_base = resolve_token_amount(&config, &network, &token, &cap)
                .map_err(|e| eyre::eyre!(format_error(&e, &context)))?;
            let cap_base: u64 = cap_base.try_into().map_err(|_| {
                eyre::eyre!(
                    "cap {} {} is {} base units, which exceeds u64 — Solana amounts \
                     are u64, so no such cap is representable (and any cap above \
                     the mint's supply is unlimited in practice; pass 0 for that)",
                    cap,
                    token,
                    cap_base
                )
            })?;

            if cap_base == 0 {
                info!(
                    "Setting {token} withdrawal cap on {network} to 0 = UNLIMITED \
                     (this DISARMS the cap)"
                );
            } else {
                info!(
                    "Setting {token} withdrawal cap on {network} to {cap} ({cap_base} \
                     base units) per ~1h epoch. Tumbling window: up to {} base units \
                     can leave across a boundary.",
                    cap_base.saturating_mul(2)
                );
            }

            // Direct-signing, never through the arborter: the cap's authority
            // must be a key the TEE does not hold, or it bounds nothing.
            //
            // These two errors deliberately bypass `format_error`. Its
            // key-related branch fires on any message containing "privkey" and
            // rewrites the hints to "ensure TRADER_PRIVKEY is set … a
            // 64-character hex string" — the wrong variable and the wrong
            // format for an Ed25519 operator-admin key, and precisely the
            // trader/admin conflation this command exists to prevent. The
            // messages raised below already name the right variable.
            let wallet = aspens::load_operator_admin_wallet_solana()?;

            let outcome = executor.execute(async move {
                aspens::operator::set_withdraw_epoch_cap(
                    &config, &network, &token, cap_base, &wallet,
                )
                .await
            })?;

            info!("Transaction: {}", outcome.signature);
            info!("  instance:       {}", outcome.instance);
            info!("  mint:           {}", outcome.mint);
            info!("  withdraw_epoch: {}", outcome.withdraw_epoch);
            match outcome.state {
                Some(state) if state.cap == 0 => info!(
                    "  cap on chain:   0 (UNLIMITED); epoch {} has {} base units withdrawn",
                    state.epoch, state.withdrawn
                ),
                Some(state) => info!(
                    "  cap on chain:   {} base units; epoch {} has {} withdrawn",
                    state.cap, state.epoch, state.withdrawn
                ),
                None => info!(
                    "  cap on chain:   could not be read back (the transaction confirmed; \
                     re-check with a fresh RPC read)"
                ),
            }
            if outcome.admin_is_tee_signer {
                eprintln!(
                    "warning: this instance's operator_admin IS its TEE signer, so the cap \
                     bounds bugs and operator error but NOT a compromised signer — that key \
                     can raise the cap again. Configure a distinct operator-admin key for \
                     real containment."
                );
            }
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "fce"))]
mod fce_dispatch_tests {
    use super::*;

    /// The FCE wire has no `hidden` field, so a hidden order submitted over it
    /// would rest VISIBLY. Refusing is the only safe answer — dropping the flag
    /// silently discloses the order.
    #[test]
    fn hidden_is_refused_over_fce() {
        let err = check_flags_supported_over_fce(OrderFlags {
            post_only: false,
            hidden: true,
        })
        .expect_err("--hidden must not be accepted over FCE");
        let msg = err.to_string();
        assert!(
            msg.contains("--hidden"),
            "error should name the flag: {msg}"
        );
        assert!(
            msg.contains("gRPC"),
            "error should say where it does work: {msg}"
        );
    }

    /// post_only IS expressible on the FCE wire, so it must pass through.
    #[test]
    fn post_only_is_allowed_over_fce() {
        check_flags_supported_over_fce(OrderFlags {
            post_only: true,
            hidden: false,
        })
        .expect("post_only is carried by the FCE PlaceOrderRequest");
    }

    /// The side strings must be the ones `send_order::parse_side` accepts —
    /// `fce_actions::place_order` parses them with exactly that function.
    #[test]
    fn side_strings_round_trip_through_parse_side() {
        assert_eq!(
            parse_side(fce_side_str(Side::Bid).unwrap()).unwrap(),
            Side::Bid
        );
        assert_eq!(
            parse_side(fce_side_str(Side::Ask).unwrap()).unwrap(),
            Side::Ask
        );
        assert!(fce_side_str(Side::Unspecified).is_err());
    }
}
