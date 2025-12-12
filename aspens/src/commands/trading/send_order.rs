pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use alloy::primitives::Signature;
use alloy::signers::{local::PrivateKeySigner, Signer};
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderRequest, SendOrderResponse, TransactionHash};
use eyre::Result;
use prost::Message;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::grpc::create_channel;

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Order {{\n  side: {},\n  quantity: {},\n  price: {},\n  market_id: {},\n  base_account_address: {},\n  quote_account_address: {},\n  execution_type: {},\n  matching_order_ids: {:?}\n}}",
            self.side,
            self.quantity,
            self.price.clone().map_or("None".to_string(), |p| p.to_string()),
            self.market_id,
            self.base_account_address,
            self.quote_account_address,
            self.execution_type,
            self.matching_order_ids
        )
    }
}

/// Transaction hash information for blockchain transactions
///
/// This struct contains information about transaction hashes that are generated
/// when orders are processed on the blockchain. Each transaction hash includes
/// a type (e.g., "deposit", "settlement", "withdrawal") and the actual hash value.
impl fmt::Display for TransactionHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TransactionHash {{ hash_type: {}, hash_value: {} }}",
            self.hash_type, self.hash_value
        )
    }
}

impl TransactionHash {
    /// Format transaction hash for CLI display
    ///
    /// Returns a user-friendly string representation of the transaction hash
    /// in the format "type: hash_value"
    pub fn format_for_cli(&self) -> String {
        format!("[{}] {}", self.hash_type.to_uppercase(), self.hash_value)
    }

    /// Get block explorer URL hints based on common chains
    ///
    /// Returns a suggested block explorer base URL for common chains
    pub fn get_explorer_hint(&self) -> Option<String> {
        // This is a simple implementation - could be enhanced with actual chain detection
        Some(
            "Paste this hash into your chain's block explorer (e.g., Etherscan, Basescan)"
                .to_string(),
        )
    }
}

impl SendOrderResponse {
    /// Get formatted transaction hashes for CLI display
    ///
    /// Returns a vector of formatted transaction hash strings that can be
    /// easily displayed in the CLI or REPL interface
    pub fn get_formatted_transaction_hashes(&self) -> Vec<String> {
        self.transaction_hashes
            .iter()
            .map(|th| th.format_for_cli())
            .collect()
    }
}

impl fmt::Display for SendOrderResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SendOrderResponse {{\n  order_in_book: {},\n  order: {},\n  trades: [{}],\n  transaction_hashes: [{}]\n}}",
            self.order_in_book,
            self.order
                .as_ref()
                .map_or("None".to_string(), |o| format!("{}", o)),
            self.trades
                .iter()
                .map(|t| format!("{:?}", t))
                .collect::<Vec<_>>()
                .join(", "),
            self.transaction_hashes
                .iter()
                .map(|th| format!("{}: {}", th.hash_type, th.hash_value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn call_send_order(
    url: String,
    side: i32,
    quantity: String,
    price: Option<String>,
    market_id: String,
    base_account_address: String,
    quote_account_address: String,
    privkey: String,
) -> Result<SendOrderResponse> {
    // Create a channel to connect to the gRPC server (with TLS support for HTTPS)
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Create the order for sending with original pair decimal values
    let order_for_sending = Order {
        side,
        quantity: quantity.clone(), // Original pair decimal values
        price: price.clone(),       // Original pair decimal values
        market_id: market_id.clone(),
        base_account_address: base_account_address.clone(),
        quote_account_address: quote_account_address.clone(),
        execution_type: 0,
        matching_order_ids: vec![],
    };

    // Serialize the order to a byte vector for signing
    let mut buffer = Vec::new();
    order_for_sending.encode(&mut buffer)?;

    // Sign the order using the same values that will be sent
    let signature = sign_transaction(&buffer, &privkey).await?;

    // Create the request with the original order and signature
    let request = SendOrderRequest {
        order: Some(order_for_sending),
        signature_hash: signature.as_bytes().to_vec(),
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

async fn sign_transaction(msg_bytes: &[u8], privkey: &str) -> Result<Signature> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let signature = signer.sign_message(msg_bytes).await?;
    Ok(signature)
}

/// Send an order using configuration from the server
///
/// This is the recommended way to send orders. It uses the configuration
/// fetched from the server to look up the market and derive account addresses.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side (1 for BUY, 2 for SELL)
/// * `quantity` - The quantity to trade (in pair decimals)
/// * `price` - Optional limit price (in pair decimals)
/// * `privkey` - The private key of the user's wallet
/// * `config` - The configuration response from the server
pub async fn call_send_order_from_config(
    url: String,
    market_id: String,
    side: i32,
    quantity: String,
    price: Option<String>,
    privkey: String,
    config: GetConfigResponse,
) -> Result<SendOrderResponse> {
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

    // Derive public key (account address) from private key
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let account_address = signer.address().to_checksum(None);

    tracing::info!(
        "Sending order: market={}, side={}, quantity={}, price={:?}, account={}",
        market.name,
        if side == 1 { "BUY" } else { "SELL" },
        quantity,
        price,
        account_address
    );

    // Call the low-level send_order function
    call_send_order(
        url,
        side,
        quantity,
        price,
        market_id,
        account_address.clone(),
        account_address,
        privkey,
    )
    .await
}
