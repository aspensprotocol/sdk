pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.v1.rs");
}

use config_pb::{Chain, GetConfigRequest, GetConfigResponse, Market, Token};
use eyre::{bail, Result};
use std::fs;
use std::path::Path;
use tracing::info;

use crate::grpc::create_channel;

/// Fetch configuration from the trading server
pub async fn get_config(url: String) -> Result<GetConfigResponse> {
    use config_pb::config_service_client::ConfigServiceClient;

    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);
    let request = tonic::Request::new(GetConfigRequest {});
    let response = client.get_config(request).await?;

    Ok(response.into_inner())
}

/// Download configuration from server and save to file
pub async fn download_config(url: String, path: String) -> Result<()> {
    let config = get_config(url).await?;

    // Determine format based on file extension
    let contents = match Path::new(&path).extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::to_string_pretty(&config)?,
        Some("toml") => toml::to_string_pretty(&config)?,
        Some(ext) => bail!("Unsupported file extension: {}. Use .json or .toml", ext),
        None => bail!("No file extension found. Use .json or .toml"),
    };

    fs::write(&path, contents)?;
    info!("Configuration saved to: {}", path);

    Ok(())
}

impl GetConfigResponse {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;

        // Determine file type based on extension
        let config = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::from_str(&contents)?,
            Some("toml") => toml::from_str(&contents)?,
            Some(ext) => bail!("Unsupported file extension: {}", ext),
            None => bail!("No file extension found"),
        };

        Ok(config)
    }

    pub fn get_chain(&self, network: &str) -> Option<&Chain> {
        self.config
            .as_ref()?
            .chains
            .iter()
            .find(|chain| chain.network == network)
    }

    pub fn get_token(&self, network: &str, symbol: &str) -> Option<&Token> {
        self.get_chain(network)
            .and_then(|chain| chain.tokens.get(symbol))
    }

    pub fn get_market(&self, name: &str) -> Option<&Market> {
        self.config
            .as_ref()?
            .markets
            .iter()
            .find(|market| market.name == name)
    }

    pub fn get_market_by_tokens(
        &self,
        base_network: &str,
        base_symbol: &str,
        quote_network: &str,
        quote_symbol: &str,
    ) -> Option<&Market> {
        self.config.as_ref()?.markets.iter().find(|market| {
            market.base_chain_network == base_network
                && market.base_chain_token_symbol == base_symbol
                && market.quote_chain_network == quote_network
                && market.quote_chain_token_symbol == quote_symbol
        })
    }

    pub fn get_chain_by_id(&self, chain_id: u32) -> Option<&Chain> {
        self.config
            .as_ref()?
            .chains
            .iter()
            .find(|chain| chain.chain_id == chain_id)
    }

    pub fn get_market_by_id(&self, market_id: &str) -> Option<&Market> {
        self.config
            .as_ref()?
            .markets
            .iter()
            .find(|market| market.market_id == market_id)
    }
}

pub async fn call_get_config(url: String) -> Result<GetConfigResponse> {
    // Create a channel to connect to the gRPC server
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = config_pb::config_service_client::ConfigServiceClient::new(channel);

    // Create a request object
    let request = tonic::Request::new(GetConfigRequest {});

    // Call the get_config endpoint
    let response = client.get_config(request).await?;
    Ok(response.into_inner())
}

pub async fn download_config_to_file<P: AsRef<Path>>(url: String, path: P) -> Result<()> {
    info!("Downloading configuration to {}", path.as_ref().display());

    let config = call_get_config(url).await?;

    // Create parent directories if they don't exist
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)?;
    }

    // Write config to file
    let json = serde_json::to_string_pretty(&config)?;
    fs::write(path, json)?;

    info!("Configuration downloaded successfully");
    Ok(())
}

// Re-export types for external use
pub use config_pb::{ChainPublicKey, GetSignerPublicKeyResponse};

/// Get signer public key(s) from the trading server
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `chain_id` - Optional chain ID to filter by. If None, returns all chains.
pub async fn get_signer_public_key(
    url: String,
    chain_id: Option<u32>,
) -> Result<GetSignerPublicKeyResponse> {
    use config_pb::config_service_client::ConfigServiceClient;
    use config_pb::GetSignerPublicKeyRequest;

    let channel = create_channel(&url).await?;

    let mut client = ConfigServiceClient::new(channel);
    let request = tonic::Request::new(GetSignerPublicKeyRequest { chain_id });
    let response = client.get_signer_public_key(request).await?;

    Ok(response.into_inner())
}

/// Information about a signer including their public key and gas balance
#[derive(Debug, Clone)]
pub struct SignerInfo {
    /// The chain ID
    pub chain_id: u32,
    /// The chain network name (e.g., "base-sepolia")
    pub chain_network: String,
    /// The signer's public key (address)
    pub public_key: String,
    /// The native gas balance in wei, or None if unable to fetch
    pub gas_balance: Option<u128>,
}

impl SignerInfo {
    /// Format the gas balance as a human-readable string with 18 decimals (standard for native tokens)
    pub fn formatted_gas_balance(&self) -> String {
        match self.gas_balance {
            Some(balance) => {
                let balance_f64 = balance as f64 / 1e18;
                format!("{:.6}", balance_f64)
            }
            None => "error".to_string(),
        }
    }
}

/// Get native token balance for an address on a chain via RPC
async fn get_native_balance(rpc_url: &str, address: &str) -> Result<u128> {
    use alloy::primitives::Address;
    use alloy::providers::{Provider, ProviderBuilder};
    use url::Url;

    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let address: Address = address.parse()?;
    let balance = provider.get_balance(address).await?;

    Ok(balance.to::<u128>())
}

/// Get signer public key(s) with their native gas balances
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `chain_id` - Optional chain ID to filter by. If None, returns all chains.
///
/// # Returns
/// A vector of SignerInfo containing public key and gas balance for each chain
pub async fn get_signer_public_key_with_balances(
    url: String,
    chain_id: Option<u32>,
) -> Result<Vec<SignerInfo>> {
    // Get signer public keys
    let signer_response = get_signer_public_key(url.clone(), chain_id).await?;

    // Get config to find RPC URLs for each chain
    let config_response = get_config(url).await?;
    let config = config_response
        .config
        .ok_or_else(|| eyre::eyre!("No configuration found"))?;

    // Build a map of chain_id -> rpc_url
    let chain_rpc_map: std::collections::HashMap<u32, String> = config
        .chains
        .iter()
        .map(|chain| (chain.chain_id, chain.rpc_url.clone()))
        .collect();

    // Fetch balances for each signer
    let mut signer_infos = Vec::new();

    for (chain_id, key_info) in signer_response.chain_keys {
        let gas_balance = if let Some(rpc_url) = chain_rpc_map.get(&chain_id) {
            match get_native_balance(rpc_url, &key_info.public_key).await {
                Ok(balance) => Some(balance),
                Err(e) => {
                    tracing::warn!("Failed to get gas balance for chain {}: {}", chain_id, e);
                    None
                }
            }
        } else {
            tracing::warn!("No RPC URL found for chain {}", chain_id);
            None
        };

        signer_infos.push(SignerInfo {
            chain_id,
            chain_network: key_info.chain_network,
            public_key: key_info.public_key,
            gas_balance,
        });
    }

    // Sort by chain_id for consistent output
    signer_infos.sort_by_key(|info| info.chain_id);

    Ok(signer_infos)
}

// Re-export attestation types for external use
pub use crate::attestation::v1::{AttestationReport, GetAttestationRequest, GetAttestationResponse};

/// Get TEE attestation from the signer
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `report_data` - Optional user-provided data to bind to the attestation report (max 64 bytes)
pub async fn get_attestation(
    url: String,
    report_data: Option<Vec<u8>>,
) -> Result<GetAttestationResponse> {
    use config_pb::config_service_client::ConfigServiceClient;

    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = tonic::Request::new(GetAttestationRequest { report_data });
    let response = client.get_attestation(request).await?;

    Ok(response.into_inner())
}

/// Format attestation report for display
pub fn format_attestation_report(report: &AttestationReport) -> String {
    let mut output = String::new();
    output.push_str("TEE Attestation Report:\n");
    output.push_str(&format!("  TEE TCB SVN:      {}\n", report.tee_tcb_svn));
    output.push_str(&format!("  MR SEAM:          {}\n", report.mr_seam));
    output.push_str(&format!("  MR Signer SEAM:   {}\n", report.mr_signer_seam));
    output.push_str(&format!("  SEAM Attributes:  {}\n", report.seam_attributes));
    output.push_str(&format!("  TD Attributes:    {}\n", report.td_attributes));
    output.push_str(&format!("  XFAM:             {}\n", report.xfam));
    output.push_str(&format!("  MR TD:            {}\n", report.mr_td));
    output.push_str(&format!("  MR Config ID:     {}\n", report.mr_config_id));
    output.push_str(&format!("  MR Owner:         {}\n", report.mr_owner));
    output.push_str(&format!("  MR Owner Config:  {}\n", report.mr_owner_config));
    output.push_str(&format!("  RTMR[0]:          {}\n", report.rt_mr0));
    output.push_str(&format!("  RTMR[1]:          {}\n", report.rt_mr1));
    output.push_str(&format!("  RTMR[2]:          {}\n", report.rt_mr2));
    output.push_str(&format!("  RTMR[3]:          {}\n", report.rt_mr3));
    output.push_str(&format!("  Report Data:      {}\n", report.report_data));
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    #[ignore = "requires example config files"]
    fn test_json_config_parsing() {
        let config = GetConfigResponse::from_file("../example/config.json").unwrap();
        verify_config(&config);
    }

    #[test]
    #[ignore = "requires example config files"]
    fn test_toml_config_parsing() {
        let config = GetConfigResponse::from_file("../example/config.toml").unwrap();
        verify_config(&config);
    }

    #[tokio::test]
    #[ignore = "requires example config files and running server"]
    async fn test_download_config_to_file() -> Result<()> {
        let config = GetConfigResponse::from_file("../example/config.toml").unwrap();

        let anvil1 = config.get_chain("anvil-1").unwrap();
        let temp_dir = tempdir()?;
        let config_path = temp_dir.path().join("config.json");

        download_config_to_file(anvil1.rpc_url.clone(), &config_path).await?;

        // Verify file exists and contains valid JSON
        let contents = fs::read_to_string(&config_path)?;
        let _: GetConfigResponse = serde_json::from_str(&contents)?;

        Ok(())
    }

    fn verify_config(config: &GetConfigResponse) {
        // Test chain retrieval
        let anvil1 = config.get_chain("anvil-1").unwrap();
        assert_eq!(anvil1.chain_id, 84531);
        assert_eq!(anvil1.rpc_url, "http://localhost:8545");

        // Test token retrieval
        let usdc = config.get_token("anvil-1", "USDC").unwrap();
        assert_eq!(usdc.symbol, "USDC");
        assert_eq!(usdc.name, "USD Coin");
        assert_eq!(usdc.decimals, 6);

        // Test market retrieval
        let market = config.get_market("A1USDC-A2USDT").unwrap();
        assert_eq!(market.base_chain_network, "anvil-1");
        assert_eq!(market.base_chain_token_symbol, "USDC");
        assert_eq!(market.quote_chain_network, "anvil-2");
        assert_eq!(market.quote_chain_token_symbol, "USDT");

        // Test market lookup by tokens
        let market = config.get_market_by_tokens("anvil-1", "USDC", "anvil-2", "USDT");
        assert!(market.is_some());
        assert_eq!(market.unwrap().name, "Anvil-1 USDC - Anvil-2 USDT");
    }
}
