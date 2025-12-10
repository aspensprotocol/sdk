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
    SetChainRequest, SetChainResponse, SetMarketRequest, SetMarketResponse, SetTokenRequest,
    SetTokenResponse, SetTradeContractRequest, SetTradeContractResponse, UpdateAdminRequest,
    UpdateAdminResponse, VersionInfo,
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

/// Deploy a trade contract on a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `signed_tx` - RLP-encoded signed transaction bytes
pub async fn deploy_contract(
    url: String,
    jwt: String,
    chain_network: String,
    signed_tx: Vec<u8>,
) -> Result<DeployContractResponse> {
    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        DeployContractRequest {
            chain_network,
            signed_tx,
        },
    );
    let response = client.deploy_contract(request).await?;

    Ok(response.into_inner())
}

/// Parameters for building a createInstance transaction
#[derive(Debug, Clone)]
pub struct CreateInstanceParams {
    /// The factory contract address on the target chain
    pub factory_address: String,
    /// The trading instance signer address
    pub instance_signer_address: String,
    /// Fee percentage (uint16: 0-65535)
    pub fees: u16,
    /// The RPC URL for the target chain
    pub rpc_url: String,
    /// The chain ID
    pub chain_id: u64,
    /// The private key for signing (hex string without 0x prefix)
    pub privkey: String,
}

/// Build and sign a createInstance transaction for deploying a trading instance
///
/// This creates a signed transaction that can be sent to the backend for broadcasting.
/// The transaction calls `createInstance(address _tradingInstanceSigner, uint16 _fees)`
/// on the MidribFactory contract.
///
/// # Arguments
/// * `params` - Parameters for building the transaction
///
/// # Returns
/// The RLP-encoded signed transaction bytes
pub async fn build_create_instance_tx(params: CreateInstanceParams) -> Result<Vec<u8>> {
    use alloy::network::EthereumWallet;
    use alloy::primitives::Address;
    use alloy::providers::ProviderBuilder;
    use alloy::signers::local::PrivateKeySigner;
    use url::Url;

    // Parse addresses
    let factory_addr: Address = params.factory_address.parse()?;
    let signer_addr: Address = params.instance_signer_address.parse()?;

    // Set up the signer
    let signer: PrivateKeySigner = params.privkey.parse()?;
    let wallet = EthereumWallet::new(signer.clone());

    // Set up the provider
    let rpc_url = Url::parse(&params.rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain_id(params.chain_id)
        .wallet(wallet)
        .connect_http(rpc_url);

    // Create the contract instance
    let factory = MidribFactory::new(factory_addr, &provider);

    // Build the createInstance call
    let tx_builder = factory.createInstance(signer_addr, params.fees);

    // Build and sign the transaction without broadcasting
    // build_raw_transaction takes a signer and returns the RLP-encoded signed tx bytes
    let signed_tx_bytes = tx_builder.build_raw_transaction(signer).await?;

    Ok(signed_tx_bytes)
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
