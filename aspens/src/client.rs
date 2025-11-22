use eyre::{Context, Result};
use std::collections::HashMap;
use url::Url;

/// Main client for interacting with Aspens trading platform
pub struct AspensClient {
    /// URL of the Arborter server
    pub(crate) stack_url: Url,
    /// Environment configuration name (e.g., "anvil", "testnet")
    pub(crate) environment: String,
    /// Environment variables loaded from config
    pub(crate) env_vars: HashMap<String, String>,
}

impl AspensClient {
    /// Create a new builder for AspensClient
    pub fn builder() -> AspensClientBuilder {
        AspensClientBuilder::default()
    }

    /// Get the Arborter server URL
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

    /// Normalize chain identifier to environment variable prefix
    ///
    /// Maps chain identifiers (like "base", "BaseSepolia", "84532") to
    /// environment variable prefixes ("BASE", "QUOTE")
    pub fn normalize_chain_identifier(&self, chain: &str) -> Result<String> {
        let chain_lower = chain.to_lowercase();

        // Direct matches
        if chain_lower == "base" || chain_lower.contains("base") {
            return Ok("BASE".to_string());
        }
        if chain_lower == "quote"
            || chain_lower.contains("optimism")
            || chain_lower.contains("sepolia")
            || chain_lower == "ethereum"
        {
            return Ok("QUOTE".to_string());
        }

        // Try to match by chain ID from RPC URLs
        if let Some(base_rpc) = self.get_env("BASE_CHAIN_RPC_URL") {
            if base_rpc.contains(&chain_lower) {
                return Ok("BASE".to_string());
            }
        }
        if let Some(quote_rpc) = self.get_env("QUOTE_CHAIN_RPC_URL") {
            if quote_rpc.contains(&chain_lower) {
                return Ok("QUOTE".to_string());
            }
        }

        // Default to BASE if no match
        eyre::bail!("Unable to determine chain type for '{}'. Expected 'base' or 'quote', or a known chain name like 'BaseSepolia' or 'OptimismSepolia'", chain)
    }

    /// Resolve token address for a given chain and token symbol
    ///
    /// Looks up token addresses using the pattern: {CHAIN}_CHAIN_{TOKEN}_TOKEN_ADDRESS
    /// For example: BASE_CHAIN_USDC_TOKEN_ADDRESS or QUOTE_CHAIN_WETH_TOKEN_ADDRESS
    pub fn resolve_token_address(&self, chain: &str, token: &str) -> Result<String> {
        let token_upper = token.to_uppercase();
        let chain_normalized = self.normalize_chain_identifier(chain)?;

        // Try pattern: {CHAIN}_CHAIN_{TOKEN}_TOKEN_ADDRESS
        let key = format!("{}_CHAIN_{}_TOKEN_ADDRESS", chain_normalized, token_upper);

        self.get_env(&key).cloned().ok_or_else(|| {
            eyre::eyre!(
                "Token address not found for {} on {}. Expected environment variable: {}",
                token,
                chain,
                key
            )
        })
    }

    /// Get RPC URL for a given chain
    pub fn get_chain_rpc_url(&self, chain: &str) -> Result<String> {
        let chain_normalized = self.normalize_chain_identifier(chain)?;
        let key = format!("{}_CHAIN_RPC_URL", chain_normalized);

        self.get_env(&key).cloned().ok_or_else(|| {
            eyre::eyre!(
                "RPC URL not found for chain {}. Expected environment variable: {}",
                chain,
                key
            )
        })
    }

    /// Get contract address for a given chain
    pub fn get_chain_contract_address(&self, chain: &str) -> Result<String> {
        let chain_normalized = self.normalize_chain_identifier(chain)?;
        let key = format!("{}_CHAIN_CONTRACT_ADDRESS", chain_normalized);

        self.get_env(&key).cloned().ok_or_else(|| {
            eyre::eyre!(
                "Contract address not found for chain {}. Expected environment variable: {}",
                chain,
                key
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
    /// Set the Arborter server URL
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
