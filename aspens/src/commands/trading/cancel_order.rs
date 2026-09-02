use std::fmt;

use super::arborter_pb::arborter_service_client::ArborterServiceClient;
use super::arborter_pb::{CancelOrderRequest, CancelOrderResponse, OrderToCancel, Side};
use eyre::Result;
use prost::Message;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::grpc::create_channel;
use crate::wallet::Wallet;

impl fmt::Display for CancelOrderResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "CancelOrderResponse {{ order_canceled: {} }}",
            self.order_canceled
        )
    }
}

/// Cancel an order using a curve-agnostic wallet (EVM or Solana).
pub async fn call_cancel_order_with_wallet(
    url: String,
    market_id: String,
    side: i32,
    order_id: [u8; 32],
    wallet: &Wallet,
) -> Result<CancelOrderResponse> {
    // Create a channel to connect to the gRPC server
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Create the order to cancel
    let order_to_cancel = OrderToCancel {
        market_id,
        side,
        order_id: order_id.to_vec(),
    };

    // Serialize for signing
    let mut buffer = Vec::new();
    order_to_cancel.encode(&mut buffer)?;

    // Sign the cancel request. Send the full curve-native length — the
    // arborter's curve-aware verifier requires exactly 64 bytes for
    // Ed25519 and 65 for secp256k1 (r||s||v). Previously this code
    // truncated to 64 unconditionally, which dropped the v byte for EVM
    // signatures and would fail verification once the server starts
    // enforcing it.
    let signature_bytes = wallet.sign_message(&buffer).await?;

    // Create the request
    let request = CancelOrderRequest {
        order: Some(order_to_cancel),
        signature_hash: signature_bytes,
    };

    // Create a tonic request
    let request = tonic::Request::new(request);

    // Call the cancel_order endpoint
    let response_data = classify_cancel_result(client.cancel_order(request).await)?;

    tracing::info!("Cancel response received: {}", response_data);

    Ok(response_data)
}

/// Classify the outcome of a `cancel_order` RPC call.
///
/// Matching moved inside the arborter's single-writer actor: a cancel of an
/// order no longer live in the book — replayed, or racing a fill that just
/// landed — now answers gRPC `NOT_FOUND` instead of the old server's
/// `order_canceled: true` on a replay. The order is gone and its collateral
/// released either way, so this is an outcome to report, not a failure to
/// propagate. `order_canceled: false` never occurs on the arborter's `Ok`
/// path (it only ever replies `Ok` with `order_canceled: true`), so a
/// caller can distinguish "already gone" from a real cancel by that field
/// alone, with no string-matching required. Any other status is a genuine
/// error and passes through unchanged.
fn classify_cancel_result(
    result: Result<tonic::Response<CancelOrderResponse>, tonic::Status>,
) -> Result<CancelOrderResponse, tonic::Status> {
    match result {
        Ok(response) => Ok(response.into_inner()),
        Err(status) if status.code() == tonic::Code::NotFound => Ok(CancelOrderResponse {
            order_canceled: false,
        }),
        Err(status) => Err(status),
    }
}

/// Resolve the `Side` value a cancel must carry from the CLI's "buy"/"sell"
/// spelling.
///
/// Shared by the gRPC and FCE cancel paths.
pub(crate) fn resolve_side(side: &str) -> Result<i32> {
    match side.to_lowercase().as_str() {
        "buy" | "bid" => Ok(Side::Bid as i32),
        "sell" | "ask" => Ok(Side::Ask as i32),
        _ => Err(eyre::eyre!(
            "Invalid side '{}'. Must be 'buy' or 'sell'",
            side
        )),
    }
}

/// Cancel an order using configuration from the server with a curve-agnostic wallet.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side ("buy" or "sell")
/// * `order_id` - The order's 32-byte canonical id to cancel, exactly as
///   returned in `SendOrderResponse.order_id`
/// * `wallet` - The user's wallet (EVM or Solana)
/// * `config` - The configuration response from the server
pub async fn call_cancel_order_from_config_with_wallet(
    url: String,
    market_id: String,
    side: String,
    order_id: [u8; 32],
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<CancelOrderResponse> {
    // Look up market info
    let market = super::send_order::lookup_market(&config, &market_id)?;

    let side_value = resolve_side(&side)?;

    tracing::info!(
        "Canceling order: market={}, side={}, order_id=0x{}",
        market.name,
        side,
        hex::encode(order_id)
    );

    call_cancel_order_with_wallet(url, market.market_id.clone(), side_value, order_id, wallet).await
}

#[cfg(test)]
mod resolve_side_tests {
    use super::*;

    /// A BID locked quote-chain funds; the CURVE that signs the cancel still
    /// depends on side (see `origin_network_for_side`), but the token to
    /// release is no longer resolved or sent — the arborter derives it from
    /// the resting order.
    #[test]
    fn a_bid_resolves_to_the_bid_side() {
        for side in ["buy", "bid", "BUY", "Bid"] {
            let s = resolve_side(side).expect(side);
            assert_eq!(s, Side::Bid as i32, "side for {side}");
        }
    }

    /// An ASK — the mirror of the bid case.
    #[test]
    fn an_ask_resolves_to_the_ask_side() {
        for side in ["sell", "ask", "SELL", "Ask"] {
            let s = resolve_side(side).expect(side);
            assert_eq!(s, Side::Ask as i32, "side for {side}");
        }
    }

    #[test]
    fn an_unknown_side_is_rejected() {
        let err = resolve_side("sideways").expect_err("an unknown side must not resolve");
        assert!(err.to_string().contains("sideways"), "got: {err}");
    }
}

#[cfg(test)]
mod classify_cancel_result_tests {
    use super::*;

    /// Matching moved inside the arborter's single-writer actor: a cancel of
    /// an order no longer live in the book — replayed, or racing a fill that
    /// just landed — now answers gRPC NOT_FOUND. That's an outcome, not a
    /// failure: the order is gone and its collateral released either way.
    #[test]
    fn a_not_found_cancel_reports_already_gone_not_failure() {
        let result = classify_cancel_result(Err(tonic::Status::not_found("order not found")));
        let response =
            result.expect("NOT_FOUND must classify as an outcome, not a propagated error");
        assert!(
            !response.order_canceled,
            "already-gone must not read as a successful cancel"
        );
    }

    /// Control: a different status code must still be a real error — the
    /// classification must not swallow failures generally.
    #[test]
    fn a_different_status_code_is_still_an_error() {
        let result = classify_cancel_result(Err(tonic::Status::internal("boom")));
        let status = result.expect_err("an internal error must not classify as success");
        assert_eq!(status.code(), tonic::Code::Internal);
    }

    /// The success path passes the response through unchanged.
    #[test]
    fn a_successful_cancel_passes_through_unchanged() {
        let response = CancelOrderResponse {
            order_canceled: true,
        };
        let result = classify_cancel_result(Ok(tonic::Response::new(response)));
        assert!(result.expect("ok passes through").order_canceled);
    }
}
