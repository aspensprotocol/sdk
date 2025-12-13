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

/// Convert a human-readable amount to pair decimals format
///
/// # Arguments
/// * `amount` - The amount as a string (can be integer or decimal like "1.5")
/// * `pair_decimals` - The number of decimal places for the pair
///
/// # Returns
/// The amount in pair decimals format as a string (clean numeric, no leading zeros)
fn to_pair_decimals(amount: &str, pair_decimals: i32) -> eyre::Result<String> {
    // Handle empty or whitespace input
    let amount = amount.trim();
    if amount.is_empty() {
        return Err(eyre::eyre!("Amount cannot be empty"));
    }

    // Parse the amount as a decimal number
    let parts: Vec<&str> = amount.split('.').collect();

    if parts.len() > 2 {
        return Err(eyre::eyre!(
            "Invalid amount format '{}': multiple decimal points",
            amount
        ));
    }

    let integer_part = parts[0];
    let decimal_part = if parts.len() == 2 { parts[1] } else { "" };

    // Calculate how many zeros to add
    let decimal_places = decimal_part.len() as i32;
    let zeros_to_add = pair_decimals - decimal_places;

    let raw_result = if zeros_to_add < 0 {
        // More decimal places than pair_decimals allows - truncate
        let truncated_decimal = &decimal_part[..pair_decimals as usize];
        format!("{}{}", integer_part, truncated_decimal)
    } else {
        // Add zeros to reach pair_decimals
        let zeros = "0".repeat(zeros_to_add as usize);
        format!("{}{}{}", integer_part, decimal_part, zeros)
    };

    // Parse as u128 to remove leading zeros and validate it's a number
    let value: u128 = raw_result
        .parse()
        .map_err(|_| eyre::eyre!("Invalid amount '{}': could not parse as number", amount))?;

    Ok(value.to_string())
}

/// Check if amount appears to already be in pair decimals format
///
/// Heuristic: if the amount is a large integer (no decimal point) and
/// significantly larger than would be reasonable for human input,
/// it's probably already in pair decimals format.
fn appears_to_be_pair_decimals(amount: &str, pair_decimals: i32) -> bool {
    // If it has a decimal point, it's human-readable
    if amount.contains('.') {
        return false;
    }

    // Parse as integer to check magnitude
    if let Ok(value) = amount.parse::<u128>() {
        // If value >= 10^(pair_decimals-2), it's likely already converted
        // e.g., for pair_decimals=4, if value >= 100, it might be pair decimals
        // This handles edge cases where user enters "1" meaning 1 unit
        let threshold = 10u128.pow((pair_decimals.max(0) as u32).saturating_sub(2));
        value >= threshold
    } else {
        // Can't parse as integer, treat as human-readable
        false
    }
}

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
/// Amounts can be provided in either format:
/// - Human-readable: "1.5", "100", "0.001" (will be converted using pair_decimals)
/// - Pair decimals: Already scaled values like "15000" for 1.5 with pair_decimals=4
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side (1 for BUY, 2 for SELL)
/// * `quantity` - The quantity to trade (human-readable or pair decimals)
/// * `price` - Optional limit price (human-readable or pair decimals)
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

    let pair_decimals = market.pair_decimals;

    // Convert quantity to pair decimals if needed
    let converted_quantity = if appears_to_be_pair_decimals(&quantity, pair_decimals) {
        tracing::debug!(
            "Quantity '{}' appears to already be in pair decimals format",
            quantity
        );
        quantity.clone()
    } else {
        let converted = to_pair_decimals(&quantity, pair_decimals)?;
        tracing::info!(
            "Converting quantity {} -> {} (pair_decimals={})",
            quantity,
            converted,
            pair_decimals
        );
        converted
    };

    // Convert price to pair decimals if provided
    let converted_price = match price {
        Some(p) => {
            if appears_to_be_pair_decimals(&p, pair_decimals) {
                tracing::debug!(
                    "Price '{}' appears to already be in pair decimals format",
                    p
                );
                Some(p)
            } else {
                let converted = to_pair_decimals(&p, pair_decimals)?;
                tracing::info!(
                    "Converting price {} -> {} (pair_decimals={})",
                    p,
                    converted,
                    pair_decimals
                );
                Some(converted)
            }
        }
        None => None,
    };

    // Derive public key (account address) from private key
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let account_address = signer.address().to_checksum(None);

    tracing::info!(
        "Sending order: market={}, side={}, quantity={}, price={:?}, account={}",
        market.name,
        if side == 1 { "BUY" } else { "SELL" },
        converted_quantity,
        converted_price,
        account_address
    );

    // Call the low-level send_order function
    call_send_order(
        url,
        side,
        converted_quantity,
        converted_price,
        market_id,
        account_address.clone(),
        account_address,
        privkey,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_pair_decimals_integer() {
        // Test integer conversion with pair_decimals=4
        assert_eq!(to_pair_decimals("1", 4).unwrap(), "10000");
        assert_eq!(to_pair_decimals("10", 4).unwrap(), "100000");
        assert_eq!(to_pair_decimals("100", 4).unwrap(), "1000000");
    }

    #[test]
    fn test_to_pair_decimals_decimal() {
        // Test decimal conversion with pair_decimals=4
        assert_eq!(to_pair_decimals("1.5", 4).unwrap(), "15000");
        assert_eq!(to_pair_decimals("0.5", 4).unwrap(), "5000"); // Leading zero removed
        assert_eq!(to_pair_decimals("1.25", 4).unwrap(), "12500");
        assert_eq!(to_pair_decimals("0.0001", 4).unwrap(), "1");
    }

    #[test]
    fn test_to_pair_decimals_truncation() {
        // Test truncation when too many decimal places
        assert_eq!(to_pair_decimals("1.12345", 4).unwrap(), "11234");
        assert_eq!(to_pair_decimals("0.99999", 4).unwrap(), "9999");
    }

    #[test]
    fn test_to_pair_decimals_different_precisions() {
        // Test with different pair_decimals values
        assert_eq!(to_pair_decimals("1", 6).unwrap(), "1000000");
        assert_eq!(to_pair_decimals("1.5", 6).unwrap(), "1500000");
        assert_eq!(to_pair_decimals("1", 8).unwrap(), "100000000");
        assert_eq!(to_pair_decimals("1.5", 8).unwrap(), "150000000");
        assert_eq!(to_pair_decimals("1", 18).unwrap(), "1000000000000000000");
    }

    #[test]
    fn test_to_pair_decimals_invalid() {
        // Test invalid input
        assert!(to_pair_decimals("1.2.3", 4).is_err());
    }

    #[test]
    fn test_appears_to_be_pair_decimals() {
        // Test heuristic for detecting already-converted values
        // With pair_decimals=4, threshold is 10^2 = 100

        // Clearly human-readable (has decimal point)
        assert!(!appears_to_be_pair_decimals("1.5", 4));
        assert!(!appears_to_be_pair_decimals("100.0", 4));

        // Small integers should be treated as human-readable
        assert!(!appears_to_be_pair_decimals("1", 4));
        assert!(!appears_to_be_pair_decimals("10", 4));
        assert!(!appears_to_be_pair_decimals("99", 4));

        // Large integers could be pair decimals
        assert!(appears_to_be_pair_decimals("100", 4));
        assert!(appears_to_be_pair_decimals("10000", 4));
        assert!(appears_to_be_pair_decimals("1500000", 4));
    }

    #[test]
    fn test_appears_to_be_pair_decimals_different_precisions() {
        // With pair_decimals=6, threshold is 10^4 = 10000
        assert!(!appears_to_be_pair_decimals("1", 6));
        assert!(!appears_to_be_pair_decimals("1000", 6));
        assert!(!appears_to_be_pair_decimals("9999", 6));
        assert!(appears_to_be_pair_decimals("10000", 6));
        assert!(appears_to_be_pair_decimals("1000000", 6));

        // With pair_decimals=18, threshold is 10^16
        assert!(!appears_to_be_pair_decimals("1", 18));
        assert!(!appears_to_be_pair_decimals("1000000000000000", 18)); // 10^15
        assert!(appears_to_be_pair_decimals("10000000000000000", 18)); // 10^16
    }
}
