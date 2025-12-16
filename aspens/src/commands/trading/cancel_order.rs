pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use alloy::primitives::Signature;
use alloy::signers::{local::PrivateKeySigner, Signer};
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{CancelOrderRequest, CancelOrderResponse, OrderToCancel, Side};
use eyre::Result;
use prost::Message;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::grpc::create_channel;

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

/// Sign the cancel order request
async fn sign_cancel_request(msg_bytes: &[u8], privkey: &str) -> Result<Signature> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let signature = signer.sign_message(msg_bytes).await?;
    Ok(signature)
}

/// Cancel an order by its ID
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier
/// * `side` - Order side (1 for BID, 2 for ASK)
/// * `token_address` - The token address
/// * `order_id` - The internal order ID to cancel
/// * `privkey` - The private key of the user's wallet
pub async fn call_cancel_order(
    url: String,
    market_id: String,
    side: i32,
    token_address: String,
    order_id: u64,
    privkey: String,
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

    // Sign the cancel request
    let signature = sign_cancel_request(&buffer, &privkey).await?;

    // Create the request
    let request = CancelOrderRequest {
        order: Some(order_to_cancel),
        signature_hash: signature.as_bytes().to_vec(),
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

/// Cancel an order using configuration from the server
///
/// This is the recommended way to cancel orders. It uses the configuration
/// to look up the market and derive the token address.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side ("buy" or "sell")
/// * `order_id` - The internal order ID to cancel
/// * `privkey` - The private key of the user's wallet
/// * `config` - The configuration response from the server
pub async fn call_cancel_order_from_config(
    url: String,
    market_id: String,
    side: String,
    order_id: u64,
    privkey: String,
    config: GetConfigResponse,
) -> Result<CancelOrderResponse> {
    // Look up market info
    let market = config.get_market_by_id(&market_id).ok_or_else(|| {
        let available_markets = config
            .config
            .as_ref()
            .map(|c| {
                c.markets
                    .iter()
                    .map(|m| format!("{} ({})", m.name, m.market_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        eyre::eyre!(
            "Market '{}' not found in configuration. Available markets: {}",
            market_id,
            available_markets
        )
    })?;

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

    call_cancel_order(url, market_id, side_value, token_address, order_id, privkey).await
}
