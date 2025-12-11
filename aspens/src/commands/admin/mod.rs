//! Admin commands module for authenticated configuration operations
//!
//! This module provides admin commands that require JWT authentication.
//! All commands (except `get_version`) require a valid JWT token obtained
//! from the authentication service.

use alloy_sol_types::sol;

// MidribFactory contract for deploying trading instances
sol!(
    #[derive(Debug)]
    #[allow(missing_docs)]
    #[sol(rpc)]
    MidribFactory,
    "artifacts/MidribFactory.json"
);

pub mod config_pb {
    pub use crate::commands::config::config_pb::*;
}

use config_pb::config_service_client::ConfigServiceClient;
use config_pb::{
    DeleteChainRequest, DeleteChainResponse, DeleteMarketRequest, DeleteMarketResponse,
    DeleteTokenRequest, DeleteTokenResponse, DeleteTradeContractRequest,
    DeleteTradeContractResponse, DeployContractRequest, DeployContractResponse, Empty,
    GetDeployCalldataRequest, GetDeployCalldataResponse, SetChainRequest, SetChainResponse,
    SetMarketRequest, SetMarketResponse, SetTokenRequest, SetTokenResponse,
    SetTradeContractRequest, SetTradeContractResponse, UpdateAdminRequest, UpdateAdminResponse,
    VersionInfo,
};
use eyre::Result;
use tonic::metadata::MetadataValue;
use tonic::Request;

use crate::grpc::create_channel;

/// Create an authenticated gRPC request with JWT bearer token
fn authenticated_request<T>(jwt: &str, payload: T) -> Request<T> {
    let mut request = Request::new(payload);
    let bearer = format!("Bearer {}", jwt);
    if let Ok(value) = bearer.parse::<MetadataValue<_>>() {
        request.metadata_mut().insert("authorization", value);
    }
    request
}

// ============================================================================
// Admin Management Operations
// ============================================================================

/// Update the admin address (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `admin_address` - New admin Ethereum address
pub async fn update_admin(
    url: String,
    jwt: String,
    admin_address: String,
) -> Result<UpdateAdminResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, UpdateAdminRequest { admin_address });
    let response = client.update_admin(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Contract Operations
// ============================================================================

/// Get deploy calldata from the server for deploying a trading instance
///
/// This retrieves the pre-encoded calldata for creating a trading instance,
/// along with the factory address and chain ID needed for signing the transaction.
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `fee_bps` - Fee in basis points (e.g., 100 = 1%)
pub async fn get_deploy_calldata(
    url: String,
    jwt: String,
    chain_network: String,
    fee_bps: u32,
) -> Result<GetDeployCalldataResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        GetDeployCalldataRequest {
            chain_network,
            fee_bps,
        },
    );
    let response = client.get_deploy_calldata(request).await?;

    Ok(response.into_inner())
}

/// Deploy a trade contract on a chain (requires auth)
///
/// This function is called after broadcasting the transaction locally.
/// It waits for the transaction to be confirmed and extracts the deployed contract address.
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `tx_hash` - Transaction hash (0x-prefixed hex) of the already-broadcast createInstance call
pub async fn deploy_contract(
    url: String,
    jwt: String,
    chain_network: String,
    tx_hash: String,
) -> Result<DeployContractResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        DeployContractRequest {
            chain_network,
            tx_hash,
        },
    );
    let response = client.deploy_contract(request).await?;

    Ok(response.into_inner())
}

/// Parameters for building a createInstance transaction using server-provided calldata
#[derive(Debug, Clone)]
pub struct CreateInstanceParams {
    /// The factory contract address on the target chain (from GetDeployCalldata response)
    pub factory_address: String,
    /// Pre-encoded calldata for createInstance (from GetDeployCalldata response)
    pub calldata: Vec<u8>,
    /// The RPC URL for the target chain
    pub rpc_url: String,
    /// The chain ID (from GetDeployCalldata response)
    pub chain_id: u64,
    /// The private key for signing (hex string without 0x prefix)
    pub privkey: String,
}

/// Build and sign a createInstance transaction for deploying a trading instance
///
/// This creates a signed transaction using pre-encoded calldata from the server.
/// The calldata is obtained from the GetDeployCalldata RPC call.
///
/// # Arguments
/// * `params` - Parameters for building the transaction (includes server-provided calldata)
///
/// # Returns
/// The RLP-encoded signed transaction bytes
pub async fn build_create_instance_tx(params: CreateInstanceParams) -> Result<Vec<u8>> {
    use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy::network::{EthereumWallet, TransactionBuilder, TxSigner};
    use alloy::primitives::{Address, Bytes, TxKind, U256};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::types::TransactionRequest;
    use alloy::signers::local::PrivateKeySigner;
    use url::Url;

    // Parse addresses
    let factory_addr: Address = params.factory_address.parse()?;

    // Set up the signer
    let signer: PrivateKeySigner = params.privkey.parse()?;
    let wallet = EthereumWallet::new(signer.clone());
    let from_address = signer.address();

    // Set up the provider
    let rpc_url = Url::parse(&params.rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain_id(params.chain_id)
        .wallet(wallet)
        .connect_http(rpc_url);

    // Get the nonce for the signing address
    let nonce = provider.get_transaction_count(from_address).await?;

    // The calldata is ABI-encoded createInstance(address, uint16)
    let calldata_bytes = Bytes::from(params.calldata.clone());

    // Build a transaction request for gas estimation
    let tx_request = TransactionRequest::default()
        .with_from(from_address)
        .with_to(factory_addr)
        .with_input(calldata_bytes.clone());

    // Estimate gas
    let gas_estimate = provider.estimate_gas(tx_request).await?;

    // Get current gas prices
    let fee_estimate = provider.estimate_eip1559_fees().await?;

    // Build the EIP-1559 transaction
    let mut tx = TxEip1559 {
        chain_id: params.chain_id,
        nonce,
        gas_limit: gas_estimate + (gas_estimate / 10), // Add 10% buffer
        max_fee_per_gas: fee_estimate.max_fee_per_gas,
        max_priority_fee_per_gas: fee_estimate.max_priority_fee_per_gas,
        to: TxKind::Call(factory_addr),
        value: U256::ZERO,
        access_list: Default::default(),
        input: calldata_bytes,
    };

    // Sign the transaction
    let signature = signer.sign_transaction(&mut tx).await?;
    let signed_tx = TxEnvelope::Eip1559(tx.into_signed(signature));

    // Encode the signed transaction to RLP bytes
    use alloy::eips::eip2718::Encodable2718;
    let mut encoded = Vec::new();
    signed_tx.encode_2718(&mut encoded);

    Ok(encoded)
}

/// Broadcast a signed transaction and return the transaction hash
///
/// # Arguments
/// * `rpc_url` - The RPC URL for the target chain
/// * `signed_tx` - RLP-encoded signed transaction bytes
///
/// # Returns
/// The transaction hash as a 0x-prefixed hex string
pub async fn broadcast_transaction(rpc_url: String, signed_tx: Vec<u8>) -> Result<String> {
    use alloy::providers::{Provider, ProviderBuilder};
    use url::Url;

    let rpc_url_parsed = Url::parse(&rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url_parsed);

    // Broadcast the signed transaction
    let pending_tx = provider
        .send_raw_transaction(&signed_tx)
        .await
        .map_err(|e| eyre::eyre!("Failed to broadcast transaction: {}", e))?;

    let tx_hash = pending_tx.tx_hash();

    // Return the tx hash as 0x-prefixed hex string
    Ok(format!("{:?}", tx_hash))
}

/// Set a trade contract on a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `address` - Contract address
/// * `chain_id` - Chain ID to associate with
pub async fn set_trade_contract(
    url: String,
    jwt: String,
    address: String,
    chain_id: u32,
) -> Result<SetTradeContractResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, SetTradeContractRequest { address, chain_id });
    let response = client.set_trade_contract(request).await?;

    Ok(response.into_inner())
}

/// Delete a trade contract from a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_id` - Chain ID to remove contract from
pub async fn delete_trade_contract(
    url: String,
    jwt: String,
    chain_id: u32,
) -> Result<DeleteTradeContractResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeleteTradeContractRequest { chain_id });
    let response = client.delete_trade_contract(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Chain Operations
// ============================================================================

/// Set a chain in the configuration (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain` - Chain configuration
pub async fn set_chain(url: String, jwt: String, chain: Chain) -> Result<SetChainResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, SetChainRequest { chain: Some(chain) });
    let response = client.set_chain(request).await?;

    Ok(response.into_inner())
}

/// Delete a chain from the configuration (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name to delete (e.g., "base-sepolia")
pub async fn delete_chain(
    url: String,
    jwt: String,
    chain_network: String,
) -> Result<DeleteChainResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeleteChainRequest { chain_network });
    let response = client.delete_chain(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Token Operations
// ============================================================================

/// Set a token on a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `token` - Token configuration
pub async fn set_token(
    url: String,
    jwt: String,
    chain_network: String,
    token: Token,
) -> Result<SetTokenResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        SetTokenRequest {
            chain_network,
            token: Some(token),
        },
    );
    let response = client.set_token(request).await?;

    Ok(response.into_inner())
}

/// Delete a token from a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `token_symbol` - Token symbol to delete (e.g., "USDC")
pub async fn delete_token(
    url: String,
    jwt: String,
    chain_network: String,
    token_symbol: String,
) -> Result<DeleteTokenResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        DeleteTokenRequest {
            chain_network,
            token_symbol,
        },
    );
    let response = client.delete_token(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Market Operations
// ============================================================================

/// Parameters for setting a market
#[derive(Debug, Clone)]
pub struct SetMarketParams {
    pub base_chain_network: String,
    pub quote_chain_network: String,
    pub base_chain_token_symbol: String,
    pub quote_chain_token_symbol: String,
    pub base_chain_token_address: String,
    pub quote_chain_token_address: String,
    pub base_chain_token_decimals: i32,
    pub quote_chain_token_decimals: i32,
    pub pair_decimals: i32,
}

/// Set a market (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `params` - Market parameters
pub async fn set_market(
    url: String,
    jwt: String,
    params: SetMarketParams,
) -> Result<SetMarketResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        SetMarketRequest {
            base_chain_network: params.base_chain_network,
            quote_chain_network: params.quote_chain_network,
            base_chain_token_symbol: params.base_chain_token_symbol,
            quote_chain_token_symbol: params.quote_chain_token_symbol,
            base_chain_token_address: params.base_chain_token_address,
            quote_chain_token_address: params.quote_chain_token_address,
            base_chain_token_decimals: params.base_chain_token_decimals,
            quote_chain_token_decimals: params.quote_chain_token_decimals,
            pair_decimals: params.pair_decimals,
        },
    );
    let response = client.set_market(request).await?;

    Ok(response.into_inner())
}

/// Delete a market (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `market_id` - Market ID to delete
pub async fn delete_market(
    url: String,
    jwt: String,
    market_id: String,
) -> Result<DeleteMarketResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeleteMarketRequest { market_id });
    let response = client.delete_market(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Read-Only Operations (no auth required)
// ============================================================================

/// Get server version information (no auth required)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
pub async fn get_version(url: String) -> Result<VersionInfo> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = Request::new(Empty {});
    let response = client.get_version(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Re-exports for convenience
// ============================================================================

// Re-export types needed by CLI
pub use config_pb::Chain;
pub use config_pb::Token;
pub use config_pb::TradeContract;
