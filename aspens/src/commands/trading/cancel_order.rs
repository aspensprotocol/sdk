/// Generated protobuf bindings for the `arborter.v1` trading service.
#[allow(missing_docs)]
pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{CancelOrderRequest, CancelOrderResponse, OrderToCancel, Side};
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
    order_id: u64,
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
        order_id,
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
    let response = client.cancel_order(request).await?;

    // Get the response data
    let response_data = response.into_inner();

    tracing::info!("Cancel response received: {}", response_data);

    Ok(response_data)
}

/// Cancel an order using configuration from the server with a curve-agnostic wallet.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side ("buy" or "sell")
/// * `order_id` - The internal order ID to cancel
/// * `wallet` - The user's wallet (EVM or Solana)
/// * `config` - The configuration response from the server
pub async fn call_cancel_order_from_config_with_wallet(
    url: String,
    market_id: String,
    side: String,
    order_id: u64,
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<CancelOrderResponse> {
    // Look up market info
    let market = super::send_order::lookup_market(&config, &market_id)?;

    // Convert side string to Side enum value
    let (side_value, token_address) = match side.to_lowercase().as_str() {
        "buy" | "bid" => {
            // For buy orders, the token is on the quote chain
            let quote_chain = config
                .get_chain(&market.quote_chain_network)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Quote chain '{}' not found in configuration",
                        market.quote_chain_network
                    )
                })?;
            let token = quote_chain
                .tokens
                .get(&market.quote_chain_token_symbol)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Token '{}' not found on chain '{}'",
                        market.quote_chain_token_symbol,
                        market.quote_chain_network
                    )
                })?;
            (Side::Bid as i32, token.address.clone())
        }
        "sell" | "ask" => {
            // For sell orders, the token is on the base chain
            let base_chain = config
                .get_chain(&market.base_chain_network)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Base chain '{}' not found in configuration",
                        market.base_chain_network
                    )
                })?;
            let token = base_chain
                .tokens
                .get(&market.base_chain_token_symbol)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Token '{}' not found on chain '{}'",
                        market.base_chain_token_symbol,
                        market.base_chain_network
                    )
                })?;
            (Side::Ask as i32, token.address.clone())
        }
        _ => {
            return Err(eyre::eyre!(
                "Invalid side '{}'. Must be 'buy' or 'sell'",
                side
            ));
        }
    };

    tracing::info!(
        "Canceling order: market={}, side={}, order_id={}, token_address={}",
        market.name,
        side,
        order_id,
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
