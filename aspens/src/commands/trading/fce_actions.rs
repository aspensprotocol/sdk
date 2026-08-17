//! Trading actions over the FCE direct-action transport.
//!
//! Builds the **same signed envelopes** as the gRPC commands — reusing
//! `lookup_market`, `convert_to_pair_decimals`, `build_gasless_authorization`,
//! and the shared `sign_encoded` — then submits them through the
//! ext-proxy ([`crate::fce::FceClient`]) instead of arborter gRPC. Signing is
//! byte-identical to the gRPC path (the cross-repo parity invariant, CLAUDE.md):
//! the adapter reconstructs the arborter `Order`/`OrderToCancel` from these
//! fields with `hidden=false` and no `matching_order_ids`, so this path signs
//! the same bytes (DIRECT execution, non-hidden orders only — the FCE
//! `PlaceOrderRequest` carries no `hidden`/`matching_order_ids`).
//!
//! The wire carries no `quote_budget` either, so a **market BID** — the one
//! order that must sign one — is refused here rather than sent as an order the
//! adapter would rebuild differently. See
//! `check_order_expressible_over_fce` below (private: rustdoc refuses an
//! intra-doc link from public docs into a private item).
//!
//! Reads (`book_state`/`my_state`/`export_history`) are one-shot snapshots.

use eyre::{Result, eyre};

use prost::Message as _;

use crate::Wallet;
use crate::client::AspensClient;
use crate::commands::config::config_pb::GetConfigResponse;
use crate::fce::{
    self, CancelOrderResponse, ExportHistoryResponse, GetBookStateResponse, GetMyStateResponse,
    Outcome, PlaceOrderResponse, WithdrawVoucher,
};

use super::gasless::build_gasless_authorization;
use super::send_order::arborter_pb::{Order, OrderToCancel, Side};
use super::send_order::{convert_to_pair_decimals, lookup_market};
use super::stream_orderbook::TopOfBook;

fn fce_client(client: &AspensClient) -> Result<&fce::FceClient> {
    client
        .fce()
        .ok_or_else(|| eyre!("no FCE transport configured on this client"))
}

/// "BID"/"ASK" from the arborter side enum (matches the adapter's `sideFromString`).
fn side_str(side: i32) -> Result<&'static str> {
    match Side::try_from(side) {
        Ok(Side::Bid) => Ok("BID"),
        Ok(Side::Ask) => Ok("ASK"),
        _ => Err(eyre!("side must be BID or ASK")),
    }
}

/// Refuse the orders this transport cannot express.
///
/// A **market BID** is the one order that must carry `Order.quote_budget`
/// (field 11) — it gives quote and has no price to size that with. The
/// direct-action wire has no field for it, and the adapter rebuilds the
/// arborter `Order` from the JSON it does have, so an order signed WITH a
/// budget would be verified against bytes WITHOUT one: the arborter recovers a
/// different address and rejects it for a bad signature, saying nothing about
/// the budget. Refuse it here, the same way the CLI refuses `--hidden` over
/// FCE — a wire that cannot carry the order must say so, not send a different
/// one. A market ASK is fine: its budget is its `quantity`, already on the
/// wire, so nothing new is signed.
fn check_order_expressible_over_fce(side: i32, price: Option<&str>) -> Result<()> {
    if side == Side::Bid as i32 && price.is_none() {
        return Err(eyre!(
            "a market BUY is not supported over the FCE transport: it must commit a \
             quote budget (Order.quote_budget), the direct-action wire carries no such \
             field, and an order signed with one would fail signature verification \
             after the adapter rebuilt it without. Submit a limit buy, or send it \
             against an arborter reachable over gRPC."
        ));
    }
    Ok(())
}

/// Place a buy/sell order via FCE. `side` is "buy"/"sell"/"bid"/"ask"; amounts
/// are display units (converted to pair decimals here, as in the gRPC path).
///
/// A market BID is refused up front — see `check_order_expressible_over_fce`.
pub async fn place_order(
    client: &AspensClient,
    wallets: &[&Wallet],
    market_id: &str,
    side: &str,
    quantity: &str,
    price: Option<&str>,
    post_only: bool,
) -> Result<Outcome<PlaceOrderResponse>> {
    let fce = fce_client(client)?;
    let side_i = super::send_order::parse_side(side)? as i32;

    if post_only && price.is_none() {
        return Err(eyre!(
            "post_only is incompatible with market orders (no price)"
        ));
    }
    // Before any network work, including the config fetch (over FCE that is a
    // queued direct action).
    check_order_expressible_over_fce(side_i, price)?;

    let config = client.get_config().await?;

    let market = lookup_market(&config, market_id)?;
    let pair_decimals = market.pair_decimals as u32;
    let quantity_raw = convert_to_pair_decimals(quantity, pair_decimals)
        .map_err(|e| eyre!("invalid quantity '{quantity}': {e}"))?;
    let price_raw = price
        .map(|p| convert_to_pair_decimals(p, pair_decimals))
        .transpose()
        .map_err(|e| eyre!("invalid price: {e}"))?;

    // Resolve the base/quote wallets by chain curve (same mapping as gRPC).
    let base_chain = config
        .get_chain(&market.base_chain_network)
        .ok_or_else(|| eyre!("base chain '{}' not in config", market.base_chain_network))?;
    let quote_chain = config
        .get_chain(&market.quote_chain_network)
        .ok_or_else(|| eyre!("quote chain '{}' not in config", market.quote_chain_network))?;
    let base_wallet = wallets
        .iter()
        .copied()
        .find(|w| w.curve() == crate::wallet::chain_curve(base_chain))
        .ok_or_else(|| eyre!("no wallet for base chain '{}'", market.base_chain_network))?;
    let quote_wallet = wallets
        .iter()
        .copied()
        .find(|w| w.curve() == crate::wallet::chain_curve(quote_chain))
        .ok_or_else(|| eyre!("no wallet for quote chain '{}'", market.quote_chain_network))?;
    // Bid locks quote, Ask locks base — the signing side.
    let signing_wallet = if side_i == Side::Bid as i32 {
        quote_wallet
    } else {
        base_wallet
    };

    // Build the arborter Order EXACTLY as `call_send_order` does (the adapter
    // rebuilds this with hidden=false / matching_order_ids=[] — keep in lockstep).
    let order = Order {
        side: side_i,
        quantity: quantity_raw.clone(),
        price: price_raw.clone(),
        market_id: market.market_id.clone(),
        base_account_address: base_wallet.address(),
        quote_account_address: quote_wallet.address(),
        execution_type: 0,
        matching_order_ids: vec![],
        post_only,
        hidden: false,
        // Always `None` here: the market bid, the only cell that sets it, is
        // refused above. Setting it would change the signed bytes without
        // changing what the adapter rebuilds.
        quote_budget: None,
    };
    let signature_hash = super::sign_encoded(&order, signing_wallet).await?;

    let commitment = build_gasless_authorization(
        &config,
        market,
        side_i,
        signing_wallet,
        &quantity_raw,
        price_raw.as_deref(),
        None,
    )?;

    let req = fce::PlaceOrderRequest {
        side: side_str(side_i)?.to_string(),
        quantity: quantity_raw,
        price: price_raw,
        market_id: market.market_id.clone(),
        base_account_address: order.base_account_address.clone(),
        quote_account_address: order.quote_account_address.clone(),
        execution_type: None,
        post_only: if post_only { Some(true) } else { None },
        signature_hash,
        order_id: commitment.authorization.order_id,
        // The adapter's JSON still has this field (infra
        // `stacks/fcc/extension/pkg/types/types.go`), and it forwards it into
        // `OrderAuthorization.amount_in` — a proto field the arborter has since
        // deleted, so the value is now inert on arrival. Keep sending the real
        // budget until infra syncs the proto and drops the field: an omitted key
        // would reach a deployed adapter as `""`.
        amount_in: commitment.input_amount.to_string(),
    };
    fce.place_order(&req).await
}

/// Cancel an order via FCE. `side` is "buy"/"sell"/"bid"/"ask".
pub async fn cancel_order(
    client: &AspensClient,
    wallet: &Wallet,
    market_id: &str,
    side: &str,
    token_address: &str,
    order_id: u64,
) -> Result<Outcome<CancelOrderResponse>> {
    let fce = fce_client(client)?;
    let side_i = super::send_order::parse_side(side)? as i32;

    // Sign the same OrderToCancel bytes the gRPC path signs.
    let to_cancel = OrderToCancel {
        market_id: market_id.to_string(),
        side: side_i,
        token_address: token_address.to_string(),
        order_id,
    };
    let signature_hash = super::sign_encoded(&to_cancel, wallet).await?;

    let req = fce::CancelOrderRequest {
        market_id: market_id.to_string(),
        side: side_str(side_i)?.to_string(),
        token_address: token_address.to_string(),
        order_id,
        signature_hash,
    };
    fce.cancel_order(&req).await
}

/// Request a MidribV3 withdrawal voucher via FCE. Signs the canonical
/// `network|token|account|amount` (as the gRPC withdraw does); the returned
/// voucher is presented to `MidribV3.withdraw(voucher, signature)` on-chain
/// (that submission stays a separate step, unchanged). `token`/`account`/
/// `amount` are the on-chain resolved values.
pub async fn withdraw(
    client: &AspensClient,
    wallet: &Wallet,
    network: &str,
    token: &str,
    account: &str,
    amount: &str,
) -> Result<Outcome<WithdrawVoucher>> {
    let fce = fce_client(client)?;
    let canonical = format!("{network}|{token}|{account}|{amount}");
    let signature = wallet.sign_message(canonical.as_bytes()).await?;
    let req = fce::WithdrawRequest {
        network: network.to_string(),
        token: token.to_string(),
        account: account.to_string(),
        amount: amount.to_string(),
        signature,
    };
    fce.withdraw(&req).await
}

/// Cancel an order via FCE, resolving the released token from the config the
/// way the gRPC path does.
///
/// Delegates the side→token mapping to the gRPC path's own
/// `super::cancel_order::resolve_side_and_token` rather than repeating it: the
/// arborter matches the resting order on the signed `OrderToCancel`, so a
/// transport that resolved a different token would sign a well-formed request
/// that cancels nothing at all.
pub async fn cancel_order_from_config(
    client: &AspensClient,
    wallet: &Wallet,
    market_id: &str,
    side: &str,
    order_id: u64,
    config: &GetConfigResponse,
) -> Result<Outcome<CancelOrderResponse>> {
    let market = lookup_market(config, market_id)?;
    let (_, token_address) = super::cancel_order::resolve_side_and_token(config, market, side)?;
    // Sign against the RESOLVED market id, as the gRPC path does — the caller's
    // `market_id` may be the shorthand form.
    cancel_order(
        client,
        wallet,
        &market.market_id,
        side,
        &token_address,
        order_id,
    )
    .await
}

/// Top-of-book for `market_id` from a one-shot FCE book snapshot.
///
/// Returns the same [`TopOfBook`] the gRPC stream path produces, so the
/// slippage-cap math in the marketable-order helpers is shared verbatim. The
/// gRPC path has to listen for a collection window because it is draining a
/// live stream; a snapshot needs no window.
///
/// Zero prices and zero-quantity levels are skipped, matching
/// [`super::stream_orderbook::fetch_top_of_book`] — only resting, non-zero
/// levels can match an aggressor.
pub async fn top_of_book(client: &AspensClient, market_id: &str) -> Result<TopOfBook> {
    let outcome = book_state(client, market_id, 0).await?;
    if !outcome.ok() {
        return Err(eyre!("book snapshot failed: {}", outcome.log));
    }
    let book = outcome.into_data()?;
    Ok(top_of_book_from_snapshot(&book))
}

/// The pure part of [`top_of_book`] — split out so the level filtering and the
/// max-bid / min-ask choice are testable without a proxy.
fn top_of_book_from_snapshot(book: &GetBookStateResponse) -> TopOfBook {
    /// A level counts only if both price and quantity parse and are non-zero.
    fn price_of(level: &crate::fce::BookLevel) -> Option<u128> {
        let qty: u128 = level.quantity.parse().ok()?;
        let price: u128 = level.price.parse().ok()?;
        (qty > 0 && price > 0).then_some(price)
    }

    // Don't assume the adapter sorts the levels — take the extremes explicitly.
    TopOfBook {
        best_bid: book.bids.iter().filter_map(price_of).max(),
        best_ask: book.asks.iter().filter_map(price_of).min(),
    }
}

/// One-shot orderbook snapshot via FCE (not a live stream). `depth` caps levels
/// per side (0 => default).
pub async fn book_state(
    client: &AspensClient,
    market_id: &str,
    depth: i64,
) -> Result<Outcome<GetBookStateResponse>> {
    fce_client(client)?
        .get_book_state(&fce::GetBookStateRequest {
            market_id: market_id.to_string(),
            depth,
        })
        .await
}

/// One-shot open-orders snapshot for `trader` via FCE.
pub async fn my_state(
    client: &AspensClient,
    market_id: &str,
    trader: &str,
) -> Result<Outcome<GetMyStateResponse>> {
    fce_client(client)?
        .get_my_state(&fce::GetMyStateRequest {
            market_id: market_id.to_string(),
            trader: trader.to_string(),
        })
        .await
}

/// One-shot trade-history snapshot for `trader` via FCE.
pub async fn export_history(
    client: &AspensClient,
    market_id: &str,
    trader: &str,
) -> Result<Outcome<ExportHistoryResponse>> {
    fce_client(client)?
        .export_history(&fce::ExportHistoryRequest {
            market_id: market_id.to_string(),
            trader: trader.to_string(),
        })
        .await
}

/// Fetch the arborter config over FCE and decode it into the SAME generated
/// type the gRPC path returns.
///
/// This is what makes an FCE-only client possible. `place_order` above needs the
/// market's pair decimals and the base/quote chains' curves to build and sign an
/// order; before this existed those came only from arborter gRPC, so a client
/// could read over `/direct` but had to reach the arborter directly to write.
pub async fn get_config(client: &AspensClient) -> Result<GetConfigResponse> {
    let outcome = fce_client(client)?.get_config().await?;
    if !outcome.ok() {
        return Err(eyre!("get-config failed: {}", outcome.log));
    }
    let envelope = outcome
        .data
        .ok_or_else(|| eyre!("get-config returned no data (log: {})", outcome.log))?;
    // Fail loudly rather than yielding a default config: an empty config makes
    // `lookup_market` fail with "market not found", pointing at the caller's
    // market id instead of at the transport.
    GetConfigResponse::decode(envelope.config_proto.as_slice())
        .map_err(|e| eyre!("decoding GetConfigResponse protobuf from the FCE adapter: {e}"))
}

#[cfg(test)]
mod expressible_over_fce_tests {
    use super::*;

    /// The refusal is exactly one cell wide. Refusing more would push callers
    /// off a transport that handles their order fine; refusing less sends an
    /// order whose signature cannot verify, and the failure reads as "invalid
    /// signature" rather than "this wire can't carry a budget".
    #[test]
    fn only_the_market_bid_is_refused() {
        let bid = Side::Bid as i32;
        let ask = Side::Ask as i32;

        let err = check_order_expressible_over_fce(bid, None).unwrap_err();
        assert!(err.to_string().contains("market BUY is not supported"));

        // Limit bid, limit ask, market ask — all expressible.
        assert!(check_order_expressible_over_fce(bid, Some("1000")).is_ok());
        assert!(check_order_expressible_over_fce(ask, Some("1000")).is_ok());
        assert!(check_order_expressible_over_fce(ask, None).is_ok());
    }
}

#[cfg(test)]
mod top_of_book_tests {
    use super::*;
    use crate::fce::BookLevel;

    fn level(price: &str, quantity: &str) -> BookLevel {
        BookLevel {
            price: price.into(),
            quantity: quantity.into(),
        }
    }

    fn book(bids: Vec<BookLevel>, asks: Vec<BookLevel>) -> GetBookStateResponse {
        GetBookStateResponse {
            market_id: "m".into(),
            bids,
            asks,
        }
    }

    /// Best bid is the HIGHEST bid, best ask the LOWEST ask. Inverting either
    /// would hand the slippage cap a price on the wrong side of the spread and
    /// submit a limit order that can never fill (or fills far too aggressively).
    #[test]
    fn takes_the_highest_bid_and_the_lowest_ask() {
        let top = top_of_book_from_snapshot(&book(
            vec![level("100", "1"), level("300", "1"), level("200", "1")],
            vec![level("900", "1"), level("700", "1"), level("800", "1")],
        ));
        assert_eq!(top.best_bid, Some(300));
        assert_eq!(top.best_ask, Some(700));
    }

    /// Don't assume the adapter sorts: the same levels in any order must give
    /// the same answer.
    #[test]
    fn does_not_depend_on_level_ordering() {
        let ascending = top_of_book_from_snapshot(&book(
            vec![level("100", "1"), level("300", "1")],
            vec![level("700", "1"), level("900", "1")],
        ));
        let descending = top_of_book_from_snapshot(&book(
            vec![level("300", "1"), level("100", "1")],
            vec![level("900", "1"), level("700", "1")],
        ));
        assert_eq!(ascending.best_bid, descending.best_bid);
        assert_eq!(ascending.best_ask, descending.best_ask);
    }

    /// Zero-quantity levels don't match an aggressor, so they must not set the
    /// reference price — same filter the gRPC stream path applies.
    #[test]
    fn skips_zero_quantity_levels() {
        let top = top_of_book_from_snapshot(&book(
            vec![level("300", "0"), level("100", "5")],
            vec![level("700", "0"), level("900", "5")],
        ));
        assert_eq!(top.best_bid, Some(100));
        assert_eq!(top.best_ask, Some(900));
    }

    /// A zero price is not a real level either.
    #[test]
    fn skips_zero_prices() {
        let top = top_of_book_from_snapshot(&book(
            vec![level("0", "5"), level("100", "5")],
            vec![level("0", "5"), level("900", "5")],
        ));
        assert_eq!(top.best_bid, Some(100));
        assert_eq!(top.best_ask, Some(900));
    }

    /// An empty (or one-sided) book yields None rather than a bogus 0 — the
    /// caller must be able to say "no resting liquidity" instead of pricing
    /// off zero.
    #[test]
    fn an_empty_side_is_none() {
        let top = top_of_book_from_snapshot(&book(vec![], vec![level("900", "5")]));
        assert_eq!(top.best_bid, None);
        assert_eq!(top.best_ask, Some(900));

        let empty = top_of_book_from_snapshot(&book(vec![], vec![]));
        assert_eq!(empty.best_bid, None);
        assert_eq!(empty.best_ask, None);
    }

    /// Unparseable amounts are skipped, not fatal — one malformed level must
    /// not blind the caller to the rest of the book.
    #[test]
    fn skips_unparseable_levels() {
        let top = top_of_book_from_snapshot(&book(
            vec![level("banana", "5"), level("100", "5")],
            vec![level("900", "melon"), level("950", "5")],
        ));
        assert_eq!(top.best_bid, Some(100));
        assert_eq!(top.best_ask, Some(950));
    }

    /// Prices exceeding u64 must survive — pair-decimal prices are u128 on this
    /// wire, and truncating one would misprice the cap by orders of magnitude.
    #[test]
    fn handles_prices_beyond_u64() {
        let big = "340282366920938463463374607431768211455"; // u128::MAX
        let top = top_of_book_from_snapshot(&book(vec![level(big, "1")], vec![level(big, "1")]));
        assert_eq!(top.best_bid, Some(u128::MAX));
        assert_eq!(top.best_ask, Some(u128::MAX));
    }
}
