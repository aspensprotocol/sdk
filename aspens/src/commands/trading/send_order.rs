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

/// Send an order using configuration from the server
///
/// This is the recommended way to send orders. It uses the configuration
/// fetched from the server to look up the market and derive account addresses.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `market_id` - The market identifier from config
/// * `side` - Order side (1 for BUY, 2 for SELL)
/// * `quantity` - The quantity to trade (in pair decimals format)
/// * `price` - Optional limit price (in pair decimals format)
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

    // Derive public key (account address) from private key
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let user_address = signer.address();
    let account_address = user_address.to_checksum(None);

    tracing::info!(
        "Sending order: market={}, side={}, quantity={}, price={:?}, account={}",
        market.name,
        if side == 1 { "BUY" } else { "SELL" },
        quantity,
        price,
        account_address
    );

    // Call the low-level send_order function
    let result = call_send_order(
        url,
        side,
        quantity.clone(),
        price.clone(),
        market_id,
        account_address.clone(),
        account_address,
        privkey,
    )
    .await;

    // If we get an insufficient balance error, query and display actual balances
    if let Err(ref e) = result {
        let err_str = e.to_string().to_lowercase();
        if err_str.contains("insufficient") || err_str.contains("balance") {
            // Determine which chain/token to check based on side
            // BUY: need quote token, SELL: need base token
            let (check_chain_network, check_token_symbol, check_token_decimals) = if side == 1 {
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

            // Try to query the actual deposited balance for more helpful error
            if let Some(chain) = config.get_chain(check_chain_network) {
                if let Some(trade_contract) = &chain.trade_contract {
                    if let Some(token) = chain.tokens.get(check_token_symbol) {
                        if let Ok(deposited_balance) = query_deposited_balance(
                            &chain.rpc_url,
                            &token.address,
                            &trade_contract.address,
                            user_address,
                            chain.chain_id,
                        )
                        .await
                        {
                            let deposited_formatted =
                                format_balance_for_display(deposited_balance, check_token_decimals);

                            // Calculate required amount
                            let required_str = if side == 1 {
                                // BUY: need quantity * price
                                if let Some(ref p) = price {
                                    let qty: u128 = quantity.parse().unwrap_or(0);
                                    let prc: u128 = p.parse().unwrap_or(0);
                                    let pair_dec_factor = 10_u128.pow(pair_decimals as u32);
                                    let required = qty * prc / pair_dec_factor;
                                    format_balance_for_display(
                                        U256::from(required),
                                        check_token_decimals,
                                    )
                                } else {
                                    "unknown (market order)".to_string()
                                }
                            } else {
                                // SELL: need quantity
                                let qty: u128 = quantity.parse().unwrap_or(0);
                                format_balance_for_display(U256::from(qty), check_token_decimals)
                            };

                            return Err(eyre::eyre!(
                                "Insufficient deposited balance on {}.\n\
                                 Token: {}\n\
                                 Required: {} {}\n\
                                 Available: {} {}\n\n\
                                 Deposit more {} on {} before placing this order.",
                                check_chain_network,
                                check_token_symbol,
                                required_str,
                                check_token_symbol,
                                deposited_formatted,
                                check_token_symbol,
                                check_token_symbol,
                                check_chain_network
                            ));
                        }
                    }
                }
            }
        }
    }

    result
}
