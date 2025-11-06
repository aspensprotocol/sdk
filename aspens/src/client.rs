use eyre::{Context, Result};
use std::collections::HashMap;
use url::Url;

/// Main client for interacting with Aspens trading platform
pub struct AspensClient {
    /// URL of the Arborter server
    pub(crate) url: Url,
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
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Get the environment name
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// Get an environment variable value
    pub fn get_env(&self, key: &str) -> Option<&String> {
        self.env_vars.get(key)
    }
}

/// Builder for AspensClient
#[derive(Default)]
pub struct AspensClientBuilder {
    url: Option<Url>,
    environment: Option<String>,
    env_file_path: Option<String>,
}

impl AspensClientBuilder {
    /// Set the Arborter server URL
    pub fn with_url(mut self, url: impl Into<String>) -> Result<Self> {
        let url_str = url.into();
        self.url = Some(Url::parse(&url_str).context("Invalid URL")?);
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

        let url = self
            .url
            .or_else(|| {
                env_vars
                    .get("ARBORTER_URL")
                    .and_then(|u| Url::parse(u).ok())
            })
            .unwrap_or_else(|| Url::parse("http://localhost:50051").unwrap());

        Ok(AspensClient {
            url,
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
                let value = value.trim().to_string();
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
        assert_eq!(client.url().as_str(), "http://example.com:8080/");
    }

    #[test]
    fn test_builder_with_environment() {
        let client = AspensClient::builder()
            .with_environment("testnet")
            .build()
            .unwrap();
        assert_eq!(client.environment(), "testnet");
    }
}
