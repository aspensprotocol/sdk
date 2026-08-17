/// Generated protobuf bindings for the `arborter.v1` trading service.
#[allow(missing_docs)]
pub mod arborter_pb {
    include!("../../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

// `Display` impls + the inherent CLI-formatting helpers on the proto
// types live next door in `display.rs` so this file can focus on the
// call / signing logic.
mod display;

use crate::chain_client::named_chain_for;
use crate::wallet::Wallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderRequest, SendOrderResponse};
use eyre::Result;
use url::Url;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::evm::rpc::MidribV3;
use crate::grpc::create_channel;

/// The two things the send path derives from one set of inputs: the `Order`
/// that gets signed, and the authorization derived over the same budget.
///
/// They are built together, in [`prepare_order`], because they must agree.
/// The budget the id is hashed over and the budget the arborter reserves come
/// from the same resolution of the same numbers — build them apart and the
/// only symptom of a mismatch is an order the venue reads differently from
/// the one the user believed they signed.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct PreparedOrder {
    /// Exactly the bytes the envelope signature is taken over, once encoded.
    pub order: Order,
    /// `order_id` for the request, plus the budget it was derived over.
    pub commitment: crate::commands::trading::gasless::OrderCommitment,
}

/// Resolve the order's budget, derive its id, and build the `Order` that
/// carries them — all pure, no I/O, so the whole shape is testable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_order(
    config: &GetConfigResponse,
    market: &crate::commands::config::config_pb::Market,
    side: i32,
    quantity_raw: String,
    price_raw: Option<String>,
    base_account_address: String,
    quote_account_address: String,
    signing_wallet: &Wallet,
    post_only: bool,
    hidden: bool,
    quote_budget_raw: Option<String>,
) -> Result<PreparedOrder> {
    // The budget the arborter will reserve, and the id derived over it. This
    // is also where a budget on the wrong cell (or a missing one on a market
    // bid) is refused, before anything is signed.
    let commitment = super::gasless::build_gasless_authorization(
        config,
        market,
        side,
        signing_wallet,
        &quantity_raw,
        price_raw.as_deref(),
        quote_budget_raw.as_deref(),
    )?;

    // The order carries the original pair-decimal values.
    // `post_only=false` is the proto3 default and is wire-skipped on encode,
    // so existing callers' signed envelopes are byte-identical to pre-feature
    // builds; only true post-only orders change the envelope digest (which
    // arborter's signature check transparently honors — same encoded bytes
    // hashed on both sides).
    //
    // `quote_budget` (field 11) is the same story: `None` emits nothing, so
    // every order that isn't a market bid signs the exact bytes it always did.
    // Note the units differ from `quantity`/`price` — it is the QUOTE token's
    // own base units, not pair decimals (see `Order.quote_budget`).
    let order = Order {
        side,
        quantity: quantity_raw,
        price: price_raw,
        market_id: market.market_id.clone(),
        base_account_address,
        quote_account_address,
        execution_type: 0,
        matching_order_ids: vec![],
        post_only,
        hidden,
        quote_budget: quote_budget_raw,
    };

    Ok(PreparedOrder { order, commitment })
}

// Internal RPC dispatcher: signs the prepared order and sends it.
async fn call_send_order(
    url: String,
    order_for_sending: Order,
    wallet: &Wallet,
    authorization: Option<arborter_pb::OrderAuthorization>,
) -> Result<SendOrderResponse> {
    // Create a channel to connect to the gRPC server (with TLS support for HTTPS)
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Encode + sign the order via the shared helper (the single sign site so
    // the gRPC and FCE paths produce byte-identical envelopes). EVM signatures
    // are 65 bytes (r||s||v); Solana Ed25519 are 64 — the arborter's curve-aware
    // verifier requires exactly those lengths, so send the full curve-native
    // length with no truncation.
    let signature_bytes = super::sign_encoded(&order_for_sending, wallet).await?;

    // Create the request with the original order and signature
    let request = SendOrderRequest {
        order: Some(order_for_sending),
        signature_hash: signature_bytes,
        authorization,
    };

    // Create a tonic request
    let request = tonic::Request::new(request);

    // Call the send_order endpoint
    let response = client.send_order(request).await?;

    // Get the response data
    let response_data = response.into_inner();

    // Print the response from the server
    tracing::info!("Response received: {}", response_data);

    Ok(response_data)
}

/// Query deposited (available) balance for a token on a specific chain
async fn query_deposited_balance(
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    user_address: Address,
    chain_id: u32,
) -> Result<U256> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;

    // Chain metadata only when alloy knows the id; `tradeBalance` is a plain
    // eth_call and needs none. This used to `unwrap_or(BaseSepolia)`, silently
    // reading balances through another chain's provider config on any id
    // outside alloy's registry (HyperEVM testnet 998, anvil, a new rollup).
    let provider = match named_chain_for(chain_id as u64) {
        Some(named) => ProviderBuilder::new()
            .with_chain(named)
            .connect_http(rpc_url)
            .erased(),
        None => ProviderBuilder::new().connect_http(rpc_url).erased(),
    };
    let contract = MidribV3::new(contract_addr, &provider);
    let result = contract
        .tradeBalance(user_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

/// Format a U256 balance with decimals for display
fn format_balance_for_display(balance: U256, decimals: u32) -> String {
    let balance_u128: u128 = balance.try_into().unwrap_or(u128::MAX);
    let divisor = 10_u128.pow(decimals);
    let integer_part = balance_u128 / divisor;
    let fractional_part = balance_u128 % divisor;
    format!(
        "{}.{:0width$}",
        integer_part,
        fractional_part,
        width = decimals as usize
    )
}

/// Convert a human-readable amount (e.g., `"1.001"`) to pair decimals format
/// as a decimal string suitable for the gRPC payload.
///
/// Thin wrapper over [`crate::decimals::parse_decimal_amount`]; kept as
/// a private alias so the existing `String`-returning order-encoding path
/// stays untouched.
pub(crate) fn convert_to_pair_decimals(amount: &str, decimals: u32) -> Result<String> {
    Ok(crate::decimals::parse_decimal_amount(amount, decimals)?.to_string())
}

/// Scale a human-readable quote budget (e.g. `"250.5"`) into the decimal
/// string `Order.quote_budget` carries.
///
/// **Not pair decimals.** `quote_budget` is denominated in the QUOTE TOKEN's
/// own base units, and the arborter converts the wire value back with
/// `market.quote_chain_token_decimals` (`get_market_decimals` →
/// `quote_budget_in_pair_units`). Scaling here with any other number — pair
/// decimals, the base token's decimals — would commit a budget orders of
/// magnitude away from the one the user typed, on every market where those
/// differ. So this reads the same field the arborter does.
pub(crate) fn quote_budget_to_raw(
    market: &crate::commands::config::config_pb::Market,
    budget: &str,
) -> Result<String> {
    let decimals = u32::try_from(market.quote_chain_token_decimals).map_err(|_| {
        eyre::eyre!(
            "market '{}' has a negative quote_chain_token_decimals ({})",
            market.name,
            market.quote_chain_token_decimals
        )
    })?;
    let raw = crate::decimals::parse_decimal_amount(budget, decimals)
        .map_err(|e| eyre::eyre!("Invalid quote budget '{}': {}", budget, e))?;
    Ok(raw.to_string())
}

/// Look up a market from the configuration
///
/// Supports multiple formats:
/// - Shorthand: `base_network/base_symbol::quote_network/quote_symbol`
///   e.g. `flare-coston2/fXRP::flare-coston2-quote/USDT0`
/// - Full market ID: `base_network::token_address::quote_network::token_address`
/// - Market name: e.g. `Base Sepolia USDC - OP Sepolia USDC`
///
/// Returns the market info or an error listing available markets.
pub fn lookup_market<'a>(
    config: &'a GetConfigResponse,
    market_id: &str,
) -> Result<&'a crate::commands::config::config_pb::Market> {
    // Strip surrounding quotes if present (e.g. from shell env vars)
    let market_id = market_id.trim_matches('"').trim_matches('\'');

    // Try shorthand format: base_network/symbol::quote_network/symbol
    if let Some((base, quote)) = market_id.split_once("::")
        && let (Some((base_network, base_symbol)), Some((quote_network, quote_symbol))) =
            (base.split_once('/'), quote.split_once('/'))
        && let Some(market) =
            config.get_market_by_tokens(base_network, base_symbol, quote_network, quote_symbol)
    {
        return Ok(market);
    }

    // Try exact market_id match
    if let Some(market) = config.get_market_by_id(market_id) {
        return Ok(market);
    }

    // Try market name match
    if let Some(market) = config.get_market(market_id) {
        return Ok(market);
    }

    let available_markets = config
        .config
        .as_ref()
        .map(|c| {
            c.markets
                .iter()
                .map(|m| {
                    format!(
                        "{}/{}::{}/{}",
                        m.base_chain_network,
                        m.base_chain_token_symbol,
                        m.quote_chain_network,
                        m.quote_chain_token_symbol
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    Err(eyre::eyre!(
        "Market '{}' not found in configuration. Available markets: {}",
        market_id,
        available_markets
    ))
}

/// Resolve the origin chain network for a (market, side) pair — the chain
/// where the user signs their lock instruction.
///
/// Mirrors `gasless::resolve_order` and the per-side branches in
/// `cancel_order::call_cancel_order_with_wallet`:
/// - `Side::Bid` (BUY) locks the quote token → origin = quote chain
/// - `Side::Ask` (SELL) locks the base token → origin = base chain
///
/// Returns `Side::Unspecified` as an error since the origin is undefined.
pub fn origin_network_for_side<'a>(
    config: &'a GetConfigResponse,
    market_id: &str,
    side: arborter_pb::Side,
) -> Result<&'a str> {
    let market = lookup_market(config, market_id)?;
    Ok(match side {
        arborter_pb::Side::Bid => &market.quote_chain_network,
        arborter_pb::Side::Ask => &market.base_chain_network,
        arborter_pb::Side::Unspecified => {
            return Err(eyre::eyre!("Side::Unspecified has no origin chain"));
        }
    })
}

/// Parse a string side ("buy"/"bid" or "sell"/"ask", case-insensitive) into
/// the typed enum. Used by binaries that take side as a CLI / REPL arg;
/// useful as a single source of truth for accepted aliases.
pub fn parse_side(s: &str) -> Result<arborter_pb::Side> {
    match s.to_lowercase().as_str() {
        "buy" | "bid" => Ok(arborter_pb::Side::Bid),
        "sell" | "ask" => Ok(arborter_pb::Side::Ask),
        other => Err(eyre::eyre!(
            "invalid side '{}' (expected 'buy'/'bid' or 'sell'/'ask')",
            other
        )),
    }
}

/// Derive the account address from a private key
pub fn derive_address(privkey: &str) -> Result<(Address, String)> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let address = signer.address();
    let checksum = address.to_checksum(None);
    Ok((address, checksum))
}

/// Send an order using a curve-agnostic wallet.
///
/// Thin wrapper over [`send_order_with_wallets`] for the common case where
/// both legs of the market live on chains that share the same curve as the
/// supplied wallet (e.g. EVM↔EVM with an EVM wallet). For cross-chain
/// markets that span Secp256k1 + Ed25519 chains, callers must use
/// `send_order_with_wallets` and supply both wallets.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier
/// * `side` - Order side (1 for BUY, 2 for SELL)
/// * `quantity` - The quantity to trade (human-readable, e.g., "1.5")
/// * `price` - Optional limit price (human-readable, e.g., "100.50")
/// * `wallet` - The user's wallet (EVM or Solana)
/// * `config` - The configuration response from the server
/// * `post_only` - Reject if the order would cross at submission. Limit
///   orders only; ignored semantics-wise for market orders (arborter
///   rejects post-only without a price).
/// * `hidden` - Invisible order: matched normally (normal price-time
///   priority) but excluded from the public orderbook stream and
///   response-embedded books; appears in NO stream, not even your own —
///   track it via `SendOrderResponse.order_id`. Fills print publicly
///   with your side's identity redacted.
/// * `quote_budget` - The maximum QUOTE to spend, human-readable (e.g.
///   "250.5"), scaled here by the market's quote-token decimals.
///   **Required for a market buy** (`side == 1` with no `price`) and
///   rejected on every other order — see [`send_order_with_wallets`].
// Public top-level API — the argument list is the documented entry
// point; introducing a builder/struct here would break every caller
// (CLI, REPL, examples) for no behavioural gain.
#[allow(clippy::too_many_arguments)]
pub async fn send_order_with_wallet(
    url: String,
    market_id: String,
    side: i32,
    quantity: String,
    price: Option<String>,
    wallet: &Wallet,
    config: GetConfigResponse,
    post_only: bool,
    hidden: bool,
    quote_budget: Option<String>,
) -> Result<SendOrderResponse> {
    send_order_with_wallets(
        url,
        market_id,
        side,
        quantity,
        price,
        &[wallet],
        config,
        post_only,
        hidden,
        quote_budget,
    )
    .await
}

/// Send an order with one or more wallets, picking the right curve per chain.
///
/// Cross-chain orders need separate user-addresses on the base and quote
/// chains, which may use different curves (e.g. an EVM ↔ Solana market needs
/// both an EVM hex address and a Solana base58 pubkey). Pass all available
/// wallets in `wallets`; the function selects the matching curve for each
/// leg of the market by reading chain architectures from `config`. The
/// origin chain's wallet (the side that locks tokens for this order) is
/// used to sign both the gasless lock and the outer envelope.
///
/// Errors if a wallet of the right curve is missing for either chain.
///
/// `post_only`: see [`send_order_with_wallet`].
/// `hidden`: see [`send_order_with_wallet`]. No client-side validation —
/// hidden combines legally with market, limit, and post_only orders.
///
/// `quote_budget`: human-readable quote amount, scaled here by the market's
/// `quote_chain_token_decimals` — the SAME field the arborter reads the wire
/// value back with, so the two sides cannot disagree about what "250.5" meant.
/// One rule decides whether it is needed: **an order commits a budget,
/// denominated in the asset it gives.** A market buy gives quote and has no
/// price to size that with, so it must state the budget; a sell gives base
/// (`quantity` IS its budget) and a limit buy derives `quantity x price`, so
/// both reject a stated one. Note the budget bounds the spend — a market buy's
/// `quantity` bounds nothing, though it must still be non-zero and is still
/// signed.
// Public top-level API — same rationale as `send_order_with_wallet`
// for keeping the argument list flat.
#[allow(clippy::too_many_arguments)]
pub async fn send_order_with_wallets(
    url: String,
    market_id: String,
    side: i32,
    quantity: String,
    price: Option<String>,
    wallets: &[&Wallet],
    config: GetConfigResponse,
    post_only: bool,
    hidden: bool,
    quote_budget: Option<String>,
) -> Result<SendOrderResponse> {
    if wallets.is_empty() {
        return Err(eyre::eyre!(
            "send_order_with_wallets requires at least one wallet"
        ));
    }

    // Reject post-only without a price up front — arborter will reject it
    // anyway, but failing here saves a round-trip + signature work and
    // surfaces a clearer error to scripts.
    if post_only && price.is_none() {
        return Err(eyre::eyre!(
            "post_only is incompatible with market orders (no price); \
             pass an explicit limit price or set post_only=false"
        ));
    }

    // Look up market
    let market = lookup_market(&config, &market_id)?;
    let pair_decimals = market.pair_decimals as u32;

    // Convert amounts
    let quantity_raw = convert_to_pair_decimals(&quantity, pair_decimals)
        .map_err(|e| eyre::eyre!("Invalid quantity '{}': {}", quantity, e))?;
    let price_raw = price
        .as_ref()
        .map(|p| convert_to_pair_decimals(p, pair_decimals))
        .transpose()
        .map_err(|e| eyre::eyre!("Invalid price: {}", e))?;
    let quote_budget_raw = quote_budget
        .as_deref()
        .map(|b| quote_budget_to_raw(market, b))
        .transpose()?;

    // Pick the wallet whose curve matches each chain's architecture. The
    // SDK's `chain_curve` helper is the single source of truth for the
    // arch→curve mapping; using it here keeps order routing aligned with
    // deposit / balance / cancel flows.
    let base_chain = config
        .get_chain(&market.base_chain_network)
        .ok_or_else(|| eyre::eyre!("base chain '{}' not in config", market.base_chain_network))?;
    let quote_chain = config
        .get_chain(&market.quote_chain_network)
        .ok_or_else(|| eyre::eyre!("quote chain '{}' not in config", market.quote_chain_network))?;
    let base_curve = crate::wallet::chain_curve(base_chain);
    let quote_curve = crate::wallet::chain_curve(quote_chain);
    let base_wallet = wallets
        .iter()
        .copied()
        .find(|w| w.curve() == base_curve)
        .ok_or_else(|| {
            eyre::eyre!(
                "no wallet of curve {:?} available for base chain '{}'",
                base_curve,
                market.base_chain_network
            )
        })?;
    let quote_wallet = wallets
        .iter()
        .copied()
        .find(|w| w.curve() == quote_curve)
        .ok_or_else(|| {
            eyre::eyre!(
                "no wallet of curve {:?} available for quote chain '{}'",
                quote_curve,
                market.quote_chain_network
            )
        })?;

    // The signing wallet is whichever side locks for this order:
    //   Bid (BUY)  → origin = quote chain  → user locks quote
    //   Ask (SELL) → origin = base  chain  → user locks base
    let signing_wallet = if side == 1 { quote_wallet } else { base_wallet };

    let base_account_address = base_wallet.address();
    let quote_account_address = quote_wallet.address();

    tracing::info!(
        "Sending order: market={}, side={}, quantity={} (raw: {}), price={:?} (raw: {:?}), \
         base_account={}, quote_account={}, signing_curve={:?}",
        market.name,
        if side == 1 { "BUY" } else { "SELL" },
        quantity,
        quantity_raw,
        price,
        price_raw,
        base_account_address,
        quote_account_address,
        signing_wallet.curve()
    );

    // Build what gets signed and the authorization that accompanies it. Under
    // the optimistic ledger the arborter authenticates via the outer envelope
    // signature and reads only `order_id` from the authorization — there is no
    // per-order on-chain lock signature, and no declared amount either
    // (`OrderAuthorization.amount_in` was deleted: it sat outside the signed
    // `Order`, so nothing bound the caller to it). The budget it commits is
    // signed, inside the order.
    let prepared = prepare_order(
        &config,
        market,
        side,
        quantity_raw.clone(),
        price_raw.clone(),
        base_account_address,
        quote_account_address,
        signing_wallet,
        post_only,
        hidden,
        quote_budget_raw,
    )?;

    let result = call_send_order(
        url,
        prepared.order,
        signing_wallet,
        Some(prepared.commitment.authorization),
    )
    .await;

    // Enhance balance errors with actual balance info (EVM only — Solana balance
    // queries via Alloy don't apply). Use the EVM-side wallet's address since
    // that's the one whose balance the helper actually queries on-chain.
    if let Err(ref e) = result {
        let err_str = e.to_string().to_lowercase();
        if (err_str.contains("insufficient") || err_str.contains("balance"))
            && let Some(evm_wallet) = wallets
                .iter()
                .copied()
                .find(|w| w.curve() == crate::wallet::CurveType::Secp256k1)
        {
            // Re-parse the EVM address for the balance enhancement helper.
            if let Ok(user_address) = evm_wallet.address().parse::<Address>()
                && let Some(enhanced) = enhance_balance_error(
                    &config,
                    market,
                    side,
                    &quantity_raw,
                    price_raw.as_deref(),
                    user_address,
                    pair_decimals,
                )
                .await
            {
                return Err(enhanced);
            }
        }
    }

    result
}

/// Try to provide a more helpful error message for balance issues
async fn enhance_balance_error(
    config: &GetConfigResponse,
    market: &crate::commands::config::config_pb::Market,
    side: i32,
    quantity_raw: &str,
    price_raw: Option<&str>,
    user_address: Address,
    pair_decimals: u32,
) -> Option<eyre::Report> {
    // BUY: need quote token, SELL: need base token
    let (chain_network, token_symbol, token_decimals) = if side == 1 {
        (
            &market.quote_chain_network,
            &market.quote_chain_token_symbol,
            market.quote_chain_token_decimals as u32,
        )
    } else {
        (
            &market.base_chain_network,
            &market.base_chain_token_symbol,
            market.base_chain_token_decimals as u32,
        )
    };

    let chain = config.get_chain(chain_network)?;
    let trade_contract = chain.trade_contract.as_ref()?;
    let token = chain.tokens.get(token_symbol)?;

    // The arborter masks rpc_url; this returns Option, so an unresolvable
    // endpoint degrades to "no balance shown" rather than a hard failure.
    let rpc = crate::chain_client::chain_rpc_url(chain).ok()?;
    let deposited_balance = query_deposited_balance(
        &rpc,
        &token.address,
        &trade_contract.address,
        user_address,
        chain.chain_id,
    )
    .await
    .ok()?;

    let deposited_formatted = format_balance_for_display(deposited_balance, token_decimals);

    let required_amount = if side == 1 {
        // BUY: need quantity * price
        if let Some(p) = price_raw {
            let qty: u128 = quantity_raw.parse().unwrap_or(0);
            let prc: u128 = p.parse().unwrap_or(0);
            let pair_dec_factor = 10_u128.pow(pair_decimals);
            Some(U256::from(qty * prc / pair_dec_factor))
        } else {
            None
        }
    } else {
        // SELL: need quantity
        let qty: u128 = quantity_raw.parse().unwrap_or(0);
        Some(U256::from(qty))
    };

    // Only enhance if we can confirm the balance is actually insufficient
    let required = required_amount?;
    if deposited_balance >= required {
        return None;
    }

    let required_str = format_balance_for_display(required, token_decimals);

    Some(eyre::eyre!(
        "Insufficient deposited balance on {}.\n\
         Token: {}\n\
         Required: {} {}\n\
         Available: {} {}\n\n\
         Deposit more {} on {} before placing this order.",
        chain_network,
        token_symbol,
        required_str,
        token_symbol,
        deposited_formatted,
        token_symbol,
        token_symbol,
        chain_network
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_order_response_order_id() {
        let response = SendOrderResponse {
            order_id: 12345,
            order_in_book: true,
            order: None,
            trades: vec![],
            transaction_hashes: vec![],
            current_orderbook: vec![],
        };

        assert_eq!(response.order_id, 12345);
    }

    #[test]
    fn test_send_order_response_order_id_zero() {
        let response = SendOrderResponse {
            order_id: 0,
            order_in_book: false,
            order: None,
            trades: vec![],
            transaction_hashes: vec![],
            current_orderbook: vec![],
        };

        assert_eq!(response.order_id, 0);
    }

    #[test]
    fn test_send_order_response_order_id_max() {
        let response = SendOrderResponse {
            order_id: u64::MAX,
            order_in_book: true,
            order: None,
            trades: vec![],
            transaction_hashes: vec![],
            current_orderbook: vec![],
        };

        assert_eq!(response.order_id, u64::MAX);
    }

    #[test]
    fn test_send_order_response_display_includes_order_id() {
        let response = SendOrderResponse {
            order_id: 98765,
            order_in_book: true,
            order: None,
            trades: vec![],
            transaction_hashes: vec![],
            current_orderbook: vec![],
        };

        let display_str = format!("{}", response);
        assert!(
            display_str.contains("order_id: 98765"),
            "Display output should contain order_id: {}",
            display_str
        );
    }

    #[test]
    fn test_send_order_response_with_order_and_order_id() {
        let order = Order {
            side: 1,
            quantity: "1000".to_string(),
            price: Some("50000".to_string()),
            market_id: "test_market".to_string(),
            base_account_address: "0x1234".to_string(),
            quote_account_address: "0x5678".to_string(),
            execution_type: 0,
            matching_order_ids: vec![],
            post_only: false,
            hidden: false,
            quote_budget: None,
        };

        let response = SendOrderResponse {
            order_id: 42,
            order_in_book: true,
            order: Some(order),
            trades: vec![],
            transaction_hashes: vec![],
            current_orderbook: vec![],
        };

        assert_eq!(response.order_id, 42);
        assert!(response.order.is_some());
        assert!(response.order_in_book);
    }

    #[test]
    fn test_convert_to_pair_decimals_integer() {
        // 6 decimals (like USDC)
        assert_eq!(convert_to_pair_decimals("1", 6).unwrap(), "1000000");
        assert_eq!(convert_to_pair_decimals("100", 6).unwrap(), "100000000");
        assert_eq!(convert_to_pair_decimals("0", 6).unwrap(), "0");
    }

    #[test]
    fn test_convert_to_pair_decimals_with_fraction() {
        // 6 decimals
        assert_eq!(convert_to_pair_decimals("1.5", 6).unwrap(), "1500000");
        assert_eq!(convert_to_pair_decimals("1.001", 6).unwrap(), "1001000");
        assert_eq!(convert_to_pair_decimals("0.5", 6).unwrap(), "500000");
        assert_eq!(convert_to_pair_decimals("0.000001", 6).unwrap(), "1");
    }

    #[test]
    fn test_convert_to_pair_decimals_truncates_extra_precision() {
        // 6 decimals - extra precision should be truncated
        assert_eq!(convert_to_pair_decimals("1.0000001", 6).unwrap(), "1000000");
        assert_eq!(convert_to_pair_decimals("1.1234567", 6).unwrap(), "1123456");
    }

    #[test]
    fn test_convert_to_pair_decimals_18_decimals() {
        // 18 decimals (like ETH)
        assert_eq!(
            convert_to_pair_decimals("1", 18).unwrap(),
            "1000000000000000000"
        );
        assert_eq!(
            convert_to_pair_decimals("0.1", 18).unwrap(),
            "100000000000000000"
        );
    }

    #[test]
    fn test_convert_to_pair_decimals_whitespace() {
        assert_eq!(convert_to_pair_decimals("  1.5  ", 6).unwrap(), "1500000");
    }

    // ----- quote_budget_to_raw: the scale that must NOT be pair decimals ---

    /// Base 18, quote 6, pair 8 — three different scales, so a budget scaled
    /// by the wrong one lands on a different integer and the assertions below
    /// can actually tell them apart.
    fn market_with_decimals(
        base_dec: i32,
        quote_dec: i32,
        pair_dec: i32,
    ) -> crate::commands::config::config_pb::Market {
        crate::commands::config::config_pb::Market {
            name: "BASE/QUOTE".into(),
            base_chain_network: "base-net".into(),
            quote_chain_network: "quote-net".into(),
            base_chain_token_symbol: "BASE".into(),
            quote_chain_token_symbol: "QUOTE".into(),
            base_chain_token_decimals: base_dec,
            quote_chain_token_decimals: quote_dec,
            pair_decimals: pair_dec,
            market_id: "base-net::0xbase::quote-net::0xquote".into(),
        }
    }

    /// `Order.quote_budget` is in the QUOTE token's own base units. The
    /// arborter reads it back with `market.quote_chain_token_decimals`, so
    /// scaling by pair decimals (10^8 here) or the base token's (10^18) would
    /// commit a budget 100x or 10^12x away from what the user typed — and
    /// nothing would report an error, because every one of those is a
    /// well-formed u128.
    #[test]
    fn quote_budget_scales_by_the_quote_tokens_decimals() {
        let market = market_with_decimals(18, 6, 8);
        assert_eq!(quote_budget_to_raw(&market, "7.5").unwrap(), "7500000");
        assert_eq!(quote_budget_to_raw(&market, "1").unwrap(), "1000000");
        // Pair decimals would have given 750000000; base decimals
        // 7500000000000000000. Neither is this.
        assert_ne!(quote_budget_to_raw(&market, "7.5").unwrap(), "750000000");
    }

    /// Same input, a market whose quote token has different decimals: the
    /// answer must follow the token, not a constant baked in here.
    #[test]
    fn quote_budget_follows_the_market_not_a_constant() {
        let six = market_with_decimals(18, 6, 8);
        let eighteen = market_with_decimals(6, 18, 8);
        assert_eq!(quote_budget_to_raw(&six, "7.5").unwrap(), "7500000");
        assert_eq!(
            quote_budget_to_raw(&eighteen, "7.5").unwrap(),
            "7500000000000000000"
        );
    }

    #[test]
    fn quote_budget_rejects_a_non_numeric_amount() {
        let market = market_with_decimals(18, 6, 8);
        assert!(quote_budget_to_raw(&market, "banana").is_err());
    }

    // ----- prepare_order: the budget must reach BOTH the signed order and
    //       the id derived alongside it -----------------------------------

    /// An anvil dev key — this is a signing-shape fixture, not a secret.
    const TEST_PRIVKEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn prepared(
        side: i32,
        price_raw: Option<&str>,
        quote_budget_raw: Option<&str>,
    ) -> Result<PreparedOrder> {
        // base 18, quote 6, pair 8 — three different scales.
        let (config, market) =
            crate::commands::trading::gasless::tests::config_with_market(18, 6, 8);
        let wallet = Wallet::from_evm_hex(TEST_PRIVKEY).expect("test key parses");
        prepare_order(
            &config,
            &market,
            side,
            "100000000".to_string(), // 1.0 base at pair decimals 8
            price_raw.map(str::to_string),
            "0xbaseaccount".to_string(),
            "0xquoteaccount".to_string(),
            &wallet,
            false,
            false,
            quote_budget_raw.map(str::to_string),
        )
    }

    /// The budget has to land in TWO places at once: on the `Order` (where
    /// the envelope signature covers it and the arborter reserves against
    /// it) and in the id derived beside it. Dropping either one is silent —
    /// an order the arborter reads differently from the one that was signed.
    #[test]
    fn prepare_market_bid_carries_the_budget_into_order_and_id() {
        let p = prepared(1, None, Some("7500000")).expect("market bid with a budget");
        assert_eq!(p.order.quote_budget.as_deref(), Some("7500000"));
        assert_eq!(p.commitment.input_amount, 7_500_000);
        // And it survives encoding — this is what actually gets signed.
        let mut buf = Vec::new();
        prost::Message::encode(&p.order, &mut buf).unwrap();
        let decoded = <Order as prost::Message>::decode(&*buf).unwrap();
        assert_eq!(decoded.quote_budget.as_deref(), Some("7500000"));
    }

    /// Every other order sets no budget, so its encoding — and therefore its
    /// signature — is byte-for-byte what it was before the field existed.
    #[test]
    fn prepare_limit_order_sets_no_budget() {
        let p = prepared(1, Some("200000000"), None).expect("limit bid");
        assert_eq!(p.order.quote_budget, None);
        // Decoding back to `None` is exact proof no field-11 record was
        // written: an emitted-but-empty field would decode as `Some("")`.
        let mut buf = Vec::new();
        prost::Message::encode(&p.order, &mut buf).unwrap();
        let decoded = <Order as prost::Message>::decode(&*buf).unwrap();
        assert_eq!(decoded.quote_budget, None);
        // 1.0 base x 2.0 price = 2.0 quote, at the quote token's 6 decimals.
        assert_eq!(p.commitment.input_amount, 2_000_000);
    }

    /// The refusals are enforced on the path that actually signs, not only in
    /// the resolver: a budget on a limit order, and a market bid with none.
    #[test]
    fn prepare_enforces_the_budget_rule() {
        let err = prepared(1, Some("200000000"), Some("7500000")).unwrap_err();
        assert!(err.to_string().contains("only for a MARKET bid"), "{err}");
        let err = prepared(1, None, None).unwrap_err();
        assert!(
            err.to_string().contains("must set Order.quote_budget"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod order_flag_wire_pinning_tests {
    //! Wire-encoding pinning tests for the boolean `Order` flags
    //! (`post_only` = field 9, `hidden` = field 10).
    //!
    //! The envelope signature in `call_send_order` is computed over the
    //! prost-encoded Order proto, so two invariants must hold for every
    //! flag or signed orders silently fail arborter's verifier:
    //!
    //!   1. **flag = false is wire-skipped** (proto3 default scalars are
    //!      omitted), so pre-feature SDK users keep byte-for-byte the
    //!      same envelope digest they always had.
    //!   2. **flag = true actually reaches the wire** — if a build-script
    //!      regression dropped the field we'd silently send `false`
    //!      always, with no error.
    //!
    //! `assert_bool_flag_wire_pinned` proves both at once with a single
    //! exact-bytes comparison; add one call per future flag.

    use super::*;
    use prost::Message;

    /// All scalar/enum fields default; both flags false. A flag under
    /// test must be the highest-numbered non-default field so that its
    /// varint appends at the end of the encoding (prost emits fields in
    /// tag order).
    fn sample_order() -> Order {
        Order {
            side: 1,
            quantity: "1000".to_string(),
            price: Some("50000".to_string()),
            market_id: "base::0xaa::quote::0xbb".to_string(),
            base_account_address: "0xb".to_string(),
            quote_account_address: "0xq".to_string(),
            execution_type: 0,
            matching_order_ids: vec![],
            post_only: false,
            hidden: false,
            quote_budget: None,
        }
    }

    /// Pin a bool flag's wire behavior: encoding with the flag set must
    /// equal the plain encoding plus an appended `[tag, 1]` varint.
    /// That single equality proves both invariants — the tag byte cannot
    /// occur in the plain encoding (a `flag=false` field on the wire
    /// would make the buffers differ in place, not by a 2-byte append),
    /// and the flag genuinely reaches the wire when true.
    fn assert_bool_flag_wire_pinned(field_number: u32, set: impl Fn(&mut Order, bool)) {
        let mut plain = sample_order();
        set(&mut plain, false);
        let mut flagged = sample_order();
        set(&mut flagged, true);

        let mut buf_plain = Vec::new();
        plain.encode(&mut buf_plain).unwrap();
        let mut buf_flagged = Vec::new();
        flagged.encode(&mut buf_flagged).unwrap();

        // Varint wire type = 0, so the tag byte is (field_number << 3).
        let tag = u8::try_from(field_number << 3).expect("tag must fit one byte");
        let mut expected = buf_plain.clone();
        expected.extend_from_slice(&[tag, 1]);
        assert_eq!(
            buf_flagged, expected,
            "flag=true must encode as the plain bytes + appended [0x{tag:02x}, 0x01]"
        );

        // Round-trips preserve the flag in both states.
        assert_eq!(Order::decode(&*buf_plain).unwrap(), plain);
        assert_eq!(Order::decode(&*buf_flagged).unwrap(), flagged);
    }

    #[test]
    fn post_only_wire_pinned() {
        assert_bool_flag_wire_pinned(9, |o, v| o.post_only = v);
    }

    #[test]
    fn hidden_wire_pinned() {
        assert_bool_flag_wire_pinned(10, |o, v| o.hidden = v);
    }
}
