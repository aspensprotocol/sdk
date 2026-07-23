//! Trading actions over the FCE direct-action transport.
//!
//! Builds the **same signed envelopes** as the gRPC commands — reusing
//! `lookup_market`, `convert_to_pair_decimals`, `build_gasless_authorization`,
//! and the shared [`super::sign_encoded`] — then submits them through the
//! ext-proxy ([`crate::fce::FceClient`]) instead of arborter gRPC. Signing is
//! byte-identical to the gRPC path (the cross-repo parity invariant, CLAUDE.md):
//! the adapter reconstructs the arborter `Order`/`OrderToCancel` from these
//! fields with `hidden=false` and no `matching_order_ids`, so this path signs
//! the same bytes (DIRECT execution, non-hidden orders only — the FCE
//! `PlaceOrderRequest` carries no `hidden`/`matching_order_ids`).
//!
//! Reads (`book_state`/`my_state`/`export_history`) are one-shot snapshots.

use eyre::{Result, eyre};

use crate::Wallet;
use crate::client::AspensClient;
use crate::fce::{
    self, CancelOrderResponse, ExportHistoryResponse, GetBookStateResponse, GetMyStateResponse,
    Outcome, PlaceOrderResponse, WithdrawVoucher,
};

use super::gasless::build_gasless_authorization;
use super::send_order::arborter_pb::{Order, OrderToCancel, Side};
use super::send_order::{convert_to_pair_decimals, lookup_market};

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

/// Place a buy/sell order via FCE. `side` is "buy"/"sell"/"bid"/"ask"; amounts
/// are display units (converted to pair decimals here, as in the gRPC path).
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
    let config = client.get_config().await?;
    let side_i = super::send_order::parse_side(side)? as i32;

    if post_only && price.is_none() {
        return Err(eyre!(
            "post_only is incompatible with market orders (no price)"
        ));
    }

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
    };
    let signature_hash = super::sign_encoded(&order, signing_wallet).await?;

    let authorization = build_gasless_authorization(
        &config,
        market,
        side_i,
        signing_wallet,
        &quantity_raw,
        price_raw.as_deref(),
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
        order_id: authorization.order_id,
        amount_in: authorization.amount_in,
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
