use eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use url::Url;

use crate::commands::config::config_pb::{Chain, GetConfigResponse, Token};

/// Main client for interacting with Aspens trading platform
pub struct AspensClient {
    /// URL of the Aspens Market Stack 
    pub(crate) stack_url: Url,
    /// Environment configuration name (e.g., "anvil", "testnet")
    pub(crate) environment: String,
    /// Environment variables loaded from config
    pub(crate) env_vars: HashMap<String, String>,
    /// Cached configuration from the server
    pub(crate) config: Arc<RwLock<Option<GetConfigResponse>>>,
}

impl AspensClient {
    /// Create a new builder for AspensClient
    pub fn builder() -> AspensClientBuilder {
        AspensClientBuilder::default()
    }

    /// Get the Aspens Market Stack URL
    pub fn stack_url(&self) -> &Url {
        &self.stack_url
    }

    /// Get the environment name
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// Get an environment variable value
    pub fn get_env(&self, key: &str) -> Option<&String> {
        self.env_vars.get(key)
    }

    /// Fetch configuration from the server and cache it
    pub async fn fetch_config(&self) -> Result<()> {
        let config = crate::commands::config::call_get_config(self.stack_url.to_string()).await?;
        let mut guard = self.config.write().unwrap();
        *guard = Some(config);
        Ok(())
    }

    /// Get the cached configuration, fetching it if necessary
    pub async fn get_config(&self) -> Result<GetConfigResponse> {
        // Check if we have a cached config
        {
            let guard = self.config.read().unwrap();
            if let Some(config) = guard.as_ref() {
                return Ok(config.clone());
            }
        }

        // No cached config, fetch it
        self.fetch_config().await?;

        // Return the newly fetched config
        let guard = self.config.read().unwrap();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| eyre::eyre!("Failed to fetch configuration"))
    }

    /// Get chain information by network name
    pub async fn get_chain_info(&self, network: &str) -> Result<Chain> {
        let config = self.get_config().await?;
        config.get_chain(network).cloned().ok_or_else(|| {
            eyre::eyre!(
                "Chain '{}' not found in configuration. Available chains: {}",
                network,
                config
                    .config
                    .as_ref()
                    .map(|c| c
                        .chains
                        .iter()
                        .map(|ch| ch.network.as_str())
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            )
        })
    }

    /// Get token information by network and symbol
    pub async fn get_token_info(&self, network: &str, symbol: &str) -> Result<Token> {
        let config = self.get_config().await?;
        config.get_token(network, symbol).cloned().ok_or_else(|| {
            let available_tokens = config
                .get_chain(network)
                .map(|chain| {
                    chain
                        .tokens
                        .keys()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "none".to_string());

            eyre::eyre!(
                "Token '{}' not found on chain '{}'. Available tokens: {}",
                symbol,
                network,
                available_tokens
            )
        })
    }

    /// Get trade contract address for a given network
    pub async fn get_trade_contract_address(&self, network: &str) -> Result<String> {
        let chain = self.get_chain_info(network).await?;
        chain
            .trade_contract
            .as_ref()
            .map(|tc| tc.address.clone())
            .ok_or_else(|| {
                eyre::eyre!(
                    "Trade contract not found for chain '{}'. Please ensure the contract is deployed.",
                    network
                )
            })
    }
}

/// Builder for AspensClient
#[derive(Default)]
pub struct AspensClientBuilder {
    stack_url: Option<Url>,
    environment: Option<String>,
    env_file_path: Option<String>,
}

impl AspensClientBuilder {
    /// Set the Aspens Market Stack Url
    pub fn with_url(mut self, url: impl Into<String>) -> Result<Self> {
        let url_str = url.into();
        self.stack_url = Some(Url::parse(&url_str).context("Invalid URL")?);
        Ok(self)
    }

    /// Set the environment name (e.g., "anvil", "testnet")
    pub fn with_environment(mut self, env: impl Into<String>) -> Self {
        self.environment = Some(env.into());
        self
    }

    /// Set custom environment file path
    pub fn with_env_file(mut self, path: impl Into<String>) -> Self {
        self.env_file_path = Some(path.into());
        self
    }

    /// Build the AspensClient
    pub fn build(self) -> Result<AspensClient> {
        let environment = self
            .environment
            .or_else(|| std::env::var("ASPENS_ENV").ok())
            .unwrap_or_else(|| "anvil".to_string());

        // Load environment file
        let env_file = self
            .env_file_path
            .unwrap_or_else(|| format!(".env.{}.local", environment));

        let env_vars = load_env_file(&env_file)?;

        let stack_url = self
            .stack_url
            .or_else(|| {
                env_vars
                    .get("ARBORTER_URL")
                    .and_then(|u| Url::parse(u).ok())
            })
            .unwrap_or_else(|| Url::parse("http://0.0.0.0:50051").unwrap());

        Ok(AspensClient {
            stack_url,
            environment,
            env_vars,
            config: Arc::new(RwLock::new(None)),
        })
    }
}

/// Load environment variables from a .env file
fn load_env_file(path: &str) -> Result<HashMap<String, String>> {
    use std::fs;
    use std::io::{BufRead, BufReader};

    let mut env_vars = HashMap::new();

    // Try to load the file, but don't fail if it doesn't exist
    if let Ok(file) = fs::File::open(path) {
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse KEY=VALUE
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let mut value = value.trim().to_string();

                // Strip surrounding quotes if present
                if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    value = value[1..value.len() - 1].to_string();
                }

                env_vars.insert(key.clone(), value.clone());
                // Also set in process environment for backwards compatibility
                std::env::set_var(&key, &value);
            }
        }
    }

    Ok(env_vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_builder_defaults() {
        let client = AspensClient::builder().build().unwrap();
        assert_eq!(client.environment(), "anvil");
    }

    #[test]
    fn test_builder_with_url() {
        let client = AspensClient::builder()
            .with_url("http://example.com:8080")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(client.stack_url().as_str(), "http://example.com:8080/");
    }

    #[test]
    fn test_builder_with_environment() {
        let client = AspensClient::builder()
            .with_environment("testnet")
            .build()
            .unwrap();
        assert_eq!(client.environment(), "testnet");
    }

    #[test]
    fn test_env_file_quote_stripping() {
        // Create a temporary .env file with quoted values
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "DOUBLE_QUOTED=\"value1\"").unwrap();
        writeln!(file, "SINGLE_QUOTED='value2'").unwrap();
        writeln!(file, "UNQUOTED=value3").unwrap();
        writeln!(file, "# Comment line").unwrap();
        writeln!(file, "EMPTY_VALUE=\"\"").unwrap();
        file.flush().unwrap();

        let env_vars = load_env_file(file.path().to_str().unwrap()).unwrap();

        assert_eq!(env_vars.get("DOUBLE_QUOTED"), Some(&"value1".to_string()));
        assert_eq!(env_vars.get("SINGLE_QUOTED"), Some(&"value2".to_string()));
        assert_eq!(env_vars.get("UNQUOTED"), Some(&"value3".to_string()));
        assert_eq!(env_vars.get("EMPTY_VALUE"), Some(&"".to_string()));
    }
}
