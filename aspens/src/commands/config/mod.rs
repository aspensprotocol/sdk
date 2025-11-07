pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.v1.rs");
}

use config_pb::{Chain, GetConfigRequest, GetConfigResponse, Market, Token};
use eyre::{bail, Result};
use std::fs;
use std::path::Path;
use tracing::info;

/// Fetch configuration from the trading server
pub async fn get_config(url: String) -> Result<GetConfigResponse> {
    use config_pb::config_service_client::ConfigServiceClient;

    let mut client = ConfigServiceClient::connect(url).await?;
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

    pub fn get_chain_by_id(&self, chain_id: i32) -> Option<&Chain> {
        self.config
            .as_ref()?
            .chains
            .iter()
            .find(|chain| chain.chain_id.eq(&chain_id))
    }
}

pub async fn call_get_config(url: String) -> Result<GetConfigResponse> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

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
