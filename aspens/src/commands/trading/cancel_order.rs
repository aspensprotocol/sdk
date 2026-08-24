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
            "CancelOrderResponse {{\n  order_canceled: {},\n  transaction_hashes: [{}]\n}}",
            self.order_canceled,
            self.transaction_hashes
                .iter()
                .map(|th| format!("{}: {}", th.hash_type, th.hash_value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl CancelOrderResponse {
    /// Get formatted transaction hashes for CLI display
    pub fn get_formatted_transaction_hashes(&self) -> Vec<String> {
        self.transaction_hashes
            .iter()
            .map(|th| format!("[{}] {}", th.hash_type.to_uppercase(), th.hash_value))
            .collect()
    }
}

/// Cancel an order using a curve-agnostic wallet (EVM or Solana).
pub async fn call_cancel_order_with_wallet(
    url: String,
    market_id: String,
    side: i32,
    token_address: String,
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
        token_address,
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
            transaction_hashes: Vec::new(),
            current_orderbook: Vec::new(),
        }),
        Err(status) => Err(status),
    }
}

/// Resolve the `(side, token_address)` pair a cancel must carry: a BID locked
/// quote-chain funds, an ASK locked base-chain funds, so the token to release is
/// the one on that side's chain.
///
/// Shared by the gRPC and FCE cancel paths. Keep it shared: the arborter
/// authenticates the signed `OrderToCancel`, so a transport that resolved a
/// different token would sign a well-formed request that simply never matches
/// the resting order — a silent no-op cancel, not an error.
pub(crate) fn resolve_side_and_token(
    config: &GetConfigResponse,
    market: &crate::commands::config::config_pb::Market,
    side: &str,
) -> Result<(i32, String)> {
    let (side_value, network, symbol) = match side.to_lowercase().as_str() {
        "buy" | "bid" => (
            Side::Bid as i32,
            &market.quote_chain_network,
            &market.quote_chain_token_symbol,
        ),
        "sell" | "ask" => (
            Side::Ask as i32,
            &market.base_chain_network,
            &market.base_chain_token_symbol,
        ),
        _ => {
            return Err(eyre::eyre!(
                "Invalid side '{}'. Must be 'buy' or 'sell'",
                side
            ));
        }
    };

    let chain = config
        .get_chain(network)
        .ok_or_else(|| eyre::eyre!("Chain '{}' not found in configuration", network))?;
    let token = chain
        .tokens
        .get(symbol)
        .ok_or_else(|| eyre::eyre!("Token '{}' not found on chain '{}'", symbol, network))?;

    Ok((side_value, token.address.clone()))
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

    let (side_value, token_address) = resolve_side_and_token(&config, market, &side)?;

    tracing::info!(
        "Canceling order: market={}, side={}, order_id=0x{}, token_address={}",
        market.name,
        side,
        hex::encode(order_id),
        token_address
    );

    call_cancel_order_with_wallet(
        url,
        market.market_id.clone(),
        side_value,
        token_address,
        order_id,
        wallet,
    )
    .await
}

#[cfg(test)]
mod resolve_side_and_token_tests {
    use super::*;
    use crate::commands::config::config_pb::{Chain, Configuration, Market, Token};

    fn chain(network: &str, symbol: &str, address: &str) -> Chain {
        let mut c = Chain {
            network: network.to_string(),
            ..Default::default()
        };
        c.tokens.insert(
            symbol.to_string(),
            Token {
                symbol: symbol.to_string(),
                address: address.to_string(),
                ..Default::default()
            },
        );
        c
    }

    fn fixture() -> (GetConfigResponse, Market) {
        let market = Market {
            base_chain_network: "base-net".into(),
            quote_chain_network: "quote-net".into(),
            base_chain_token_symbol: "BASE".into(),
            quote_chain_token_symbol: "QUOTE".into(),
            market_id: "m".into(),
            ..Default::default()
        };
        let config = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![
                    chain("base-net", "BASE", "0xbase"),
                    chain("quote-net", "QUOTE", "0xquote"),
                ],
                markets: vec![market.clone()],
            }),
        };
        (config, market)
    }

    /// A BID locked quote-chain funds, so cancelling one releases the QUOTE
    /// token. Getting this backwards signs a valid request that matches no
    /// resting order — the cancel silently does nothing.
    #[test]
    fn a_bid_resolves_the_quote_chain_token() {
        let (config, market) = fixture();
        for side in ["buy", "bid", "BUY", "Bid"] {
            let (s, token) = resolve_side_and_token(&config, &market, side).expect(side);
            assert_eq!(s, Side::Bid as i32, "side for {side}");
            assert_eq!(token, "0xquote", "token for {side}");
        }
    }

    /// An ASK locked base-chain funds — the mirror of the bid case.
    #[test]
    fn an_ask_resolves_the_base_chain_token() {
        let (config, market) = fixture();
        for side in ["sell", "ask", "SELL", "Ask"] {
            let (s, token) = resolve_side_and_token(&config, &market, side).expect(side);
            assert_eq!(s, Side::Ask as i32, "side for {side}");
            assert_eq!(token, "0xbase", "token for {side}");
        }
    }

    #[test]
    fn an_unknown_side_is_rejected() {
        let (config, market) = fixture();
        let err = resolve_side_and_token(&config, &market, "sideways")
            .expect_err("an unknown side must not resolve");
        assert!(err.to_string().contains("sideways"), "got: {err}");
    }

    /// A market naming a chain the config doesn't carry must error rather than
    /// fall through to some other chain's token.
    #[test]
    fn a_missing_chain_is_an_error() {
        let (_, market) = fixture();
        let config = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![chain("base-net", "BASE", "0xbase")],
                markets: vec![market.clone()],
            }),
        };
        let err = resolve_side_and_token(&config, &market, "buy")
            .expect_err("a missing quote chain must not resolve");
        assert!(err.to_string().contains("quote-net"), "got: {err}");
    }

    /// Likewise a chain that exists but doesn't list the market's token.
    #[test]
    fn a_missing_token_is_an_error() {
        let (_, market) = fixture();
        let config = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![
                    chain("base-net", "BASE", "0xbase"),
                    chain("quote-net", "OTHER", "0xother"),
                ],
                markets: vec![market.clone()],
            }),
        };
        let err = resolve_side_and_token(&config, &market, "buy")
            .expect_err("a missing token must not resolve");
        assert!(err.to_string().contains("QUOTE"), "got: {err}");
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
        assert!(response.transaction_hashes.is_empty());
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
            transaction_hashes: Vec::new(),
            current_orderbook: Vec::new(),
        };
        let result = classify_cancel_result(Ok(tonic::Response::new(response)));
        assert!(result.expect("ok passes through").order_canceled);
    }
}
