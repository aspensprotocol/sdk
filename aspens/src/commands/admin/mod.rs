//! Admin commands module for authenticated configuration operations
//!
//! This module provides admin commands that require JWT authentication.
//! All commands (except `get_version`) require a valid JWT token obtained
//! from the authentication service.

pub mod config_pb {
    pub use crate::commands::config::config_pb::*;
}

use config_pb::config_service_client::ConfigServiceClient;
use config_pb::{
    AddChainRequest, AddChainResponse, AddMarketRequest, AddMarketResponse, AddTokenRequest,
    AddTokenResponse, AddTradeContractRequest, AddTradeContractResponse, DeleteChainRequest,
    DeleteChainResponse, DeleteMarketRequest, DeleteMarketResponse, DeleteTokenRequest,
    DeleteTokenResponse, DeleteTradeContractRequest, DeleteTradeContractResponse,
    DeployContractRequest, DeployContractResponse, Empty, UpdateManagerRequest,
    UpdateManagerResponse, VersionInfo,
};
use eyre::Result;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Request;

/// Create an authenticated gRPC request with JWT bearer token
fn authenticated_request<T>(jwt: &str, payload: T) -> Request<T> {
    let mut request = Request::new(payload);
    let bearer = format!("Bearer {}", jwt);
    if let Ok(value) = bearer.parse::<MetadataValue<_>>() {
        request.metadata_mut().insert("authorization", value);
    }
    request
}

/// Create a gRPC channel from URL
async fn create_channel(url: String) -> Result<Channel> {
    let channel = Channel::from_shared(url)?.connect().await?;
    Ok(channel)
}

// ============================================================================
// Manager Operations
// ============================================================================

/// Update the manager address (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `manager_address` - New manager Ethereum address
pub async fn update_manager(
    url: String,
    jwt: String,
    manager_address: String,
) -> Result<UpdateManagerResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, UpdateManagerRequest { manager_address });
    let response = client.update_manager(request).await?;

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
pub async fn deploy_contract(
    url: String,
    jwt: String,
    chain_network: String,
) -> Result<DeployContractResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeployContractRequest { chain_network });
    let response = client.deploy_contract(request).await?;

    Ok(response.into_inner())
}

/// Add a trade contract to a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `address` - Contract address
/// * `chain_id` - Chain ID to associate with
pub async fn add_trade_contract(
    url: String,
    jwt: String,
    address: String,
    chain_id: i32,
) -> Result<AddTradeContractResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, AddTradeContractRequest { address, chain_id });
    let response = client.add_trade_contract(request).await?;

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
    chain_id: i32,
) -> Result<DeleteTradeContractResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeleteTradeContractRequest { chain_id });
    let response = client.delete_trade_contract(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Chain Operations
// ============================================================================

/// Add a new chain to the configuration (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain` - Chain configuration
pub async fn add_chain(url: String, jwt: String, chain: Chain) -> Result<AddChainResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, AddChainRequest { chain: Some(chain) });
    let response = client.add_chain(request).await?;

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
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(&jwt, DeleteChainRequest { chain_network });
    let response = client.delete_chain(request).await?;

    Ok(response.into_inner())
}

// ============================================================================
// Token Operations
// ============================================================================

/// Add a token to a chain (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `chain_network` - Network name (e.g., "base-sepolia")
/// * `token` - Token configuration
pub async fn add_token(
    url: String,
    jwt: String,
    chain_network: String,
    token: Token,
) -> Result<AddTokenResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        AddTokenRequest {
            chain_network,
            token: Some(token),
        },
    );
    let response = client.add_token(request).await?;

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
    let channel = create_channel(url).await?;
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

/// Parameters for adding a new market
#[derive(Debug, Clone)]
pub struct AddMarketParams {
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

/// Add a new market (requires auth)
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `jwt` - Valid JWT token
/// * `params` - Market parameters
pub async fn add_market(
    url: String,
    jwt: String,
    params: AddMarketParams,
) -> Result<AddMarketResponse> {
    let channel = create_channel(url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = authenticated_request(
        &jwt,
        AddMarketRequest {
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
    let response = client.add_market(request).await?;

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
    let channel = create_channel(url).await?;
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
    let channel = create_channel(url).await?;
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
