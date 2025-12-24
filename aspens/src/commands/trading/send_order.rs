pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use alloy::primitives::{Address, Signature, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::{local::PrivateKeySigner, Signer};
use alloy_chains::NamedChain;
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderRequest, SendOrderResponse, TransactionHash};
use eyre::Result;
use prost::Message;
use url::Url;

use super::MidribV2;
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
            "SendOrderResponse {{\n  order_id: {},\n  order_in_book: {},\n  order: {},\n  trades: [{}],\n  transaction_hashes: [{}]\n}}",
            self.order_id,
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
async fn call_send_order(
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

/// Query deposited (available) balance for a token on a specific chain
async fn query_deposited_balance(
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    user_address: Address,
    chain_id: u32,
) -> Result<U256> {
    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;

    // Try to get NamedChain, fallback to a default
    let named_chain = NamedChain::try_from(chain_id as u64).unwrap_or(NamedChain::BaseSepolia);

    let provider = ProviderBuilder::new()
        .with_chain(named_chain)
        .connect_http(rpc_url);
    let contract = MidribV2::new(contract_addr, &provider);
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

/// Convert a human-readable amount (e.g., "1.001") to pair decimals format
///
/// # Arguments
/// * `amount` - Human-readable amount string (e.g., "1.001", "100", "0.5")
/// * `decimals` - Number of decimal places for the pair
///
/// # Returns
/// * The amount as an integer string in pair decimals format
fn convert_to_pair_decimals(amount: &str, decimals: u32) -> Result<String> {
    // Parse the amount as a decimal number
    let amount = amount.trim();

    // Split on decimal point
    let parts: Vec<&str> = amount.split('.').collect();

    let (integer_part, fractional_part) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(eyre::eyre!("Invalid amount format: {}", amount)),
    };

    // Parse integer part
    let integer: u128 = if integer_part.is_empty() {
        0
    } else {
        integer_part.parse().map_err(|_| eyre::eyre!("Invalid integer part: {}", integer_part))?
    };

    // Handle fractional part - pad or truncate to match decimals
    let fractional_str = if fractional_part.len() >= decimals as usize {
        // Truncate to decimals places
        &fractional_part[..decimals as usize]
    } else {
        // Will need to pad with zeros
        fractional_part
    };

    let fractional: u128 = if fractional_str.is_empty() {
        0
    } else {
        fractional_str.parse().map_err(|_| eyre::eyre!("Invalid fractional part: {}", fractional_str))?
    };

    // Calculate the multiplier for padding
    let padding_zeros = decimals as usize - fractional_str.len().min(decimals as usize);
    let fractional_padded = fractional * 10_u128.pow(padding_zeros as u32);

    // Combine: integer * 10^decimals + fractional
    let multiplier = 10_u128.pow(decimals);
    let result = integer
        .checked_mul(multiplier)
        .and_then(|v| v.checked_add(fractional_padded))
        .ok_or_else(|| eyre::eyre!("Amount overflow: {}", amount))?;

    Ok(result.to_string())
}

/// Look up a market by ID from the configuration
///
/// Returns the market info or an error listing available markets.
pub fn lookup_market<'a>(
    config: &'a GetConfigResponse,
    market_id: &str,
) -> Result<&'a crate::commands::config::config_pb::Market> {
    config.get_market_by_id(market_id).ok_or_else(|| {
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
    })
}

/// Derive the account address from a private key
pub fn derive_address(privkey: &str) -> Result<(Address, String)> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let address = signer.address();
    let checksum = address.to_checksum(None);
    Ok((address, checksum))
}

/// Send an order using configuration from the server
///
/// This is the recommended way to send orders. It:
/// - Looks up the market to get pair decimals
/// - Converts human-readable amounts (e.g., "1.5") to raw format
/// - Derives account address from private key
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier
/// * `side` - Order side (1 for BUY, 2 for SELL)
/// * `quantity` - The quantity to trade (human-readable, e.g., "1.5")
/// * `price` - Optional limit price (human-readable, e.g., "100.50")
/// * `privkey` - The private key of the user's wallet
/// * `config` - The configuration response from the server
pub async fn send_order(
    url: String,
    market_id: String,
    side: i32,
    quantity: String,
    price: Option<String>,
    privkey: String,
    config: GetConfigResponse,
) -> Result<SendOrderResponse> {
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

    // Derive address
    let (user_address, account_address) = derive_address(&privkey)?;

    tracing::info!(
        "Sending order: market={}, side={}, quantity={} (raw: {}), price={:?} (raw: {:?}), account={}",
        market.name,
        if side == 1 { "BUY" } else { "SELL" },
        quantity,
        quantity_raw,
        price,
        price_raw,
        account_address
    );

    // Send
    let result = call_send_order(
        url,
        side,
        quantity_raw.clone(),
        price_raw.clone(),
        market_id,
        account_address.clone(),
        account_address,
        privkey,
    )
    .await;

    // Enhance balance errors with actual balance info
    if let Err(ref e) = result {
        let err_str = e.to_string().to_lowercase();
        if err_str.contains("insufficient") || err_str.contains("balance") {
            if let Some(enhanced) = enhance_balance_error(
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

    let deposited_balance = query_deposited_balance(
        &chain.rpc_url,
        &token.address,
        &trade_contract.address,
        user_address,
        chain.chain_id,
    )
    .await
    .ok()?;

    let deposited_formatted = format_balance_for_display(deposited_balance, token_decimals);

    let required_str = if side == 1 {
        // BUY: need quantity * price
        if let Some(p) = price_raw {
            let qty: u128 = quantity_raw.parse().unwrap_or(0);
            let prc: u128 = p.parse().unwrap_or(0);
            let pair_dec_factor = 10_u128.pow(pair_decimals);
            let required = qty * prc / pair_dec_factor;
            format_balance_for_display(U256::from(required), token_decimals)
        } else {
            "unknown (market order)".to_string()
        }
    } else {
        // SELL: need quantity
        let qty: u128 = quantity_raw.parse().unwrap_or(0);
        format_balance_for_display(U256::from(qty), token_decimals)
    };

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
        assert_eq!(convert_to_pair_decimals("1", 18).unwrap(), "1000000000000000000");
        assert_eq!(convert_to_pair_decimals("0.1", 18).unwrap(), "100000000000000000");
    }

    #[test]
    fn test_convert_to_pair_decimals_whitespace() {
        assert_eq!(convert_to_pair_decimals("  1.5  ", 6).unwrap(), "1500000");
    }
}
