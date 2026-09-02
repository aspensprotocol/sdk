use eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use url::Url;

use crate::commands::config::config_pb::GetConfigResponse;

/// Transport for trading actions. Config discovery always uses gRPC; actions
/// (orders/cancels/withdraws/reads) route through the selected transport.
#[cfg(feature = "fce")]
#[derive(Clone)]
pub enum Transport {
    /// Direct arborter gRPC (the default).
    Grpc,
    /// The Flare Confidential Extension proxy (`POST /direct` + poll).
    Fce(crate::fce::FceClient),
}

#[cfg(feature = "fce")]
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Grpc => write!(f, "Transport::Grpc"),
            Transport::Fce(_) => write!(f, "Transport::Fce"),
        }
    }
}

/// Main client for interacting with Aspens trading platform
pub struct AspensClient {
    /// URL of the Aspens Market Stack
    pub(crate) stack_url: Url,
    /// Environment variables loaded from .env file
    pub(crate) env_vars: HashMap<String, String>,
    /// Cached configuration from the server
    pub(crate) config: Arc<RwLock<Option<GetConfigResponse>>>,
    /// Trading-action transport (gRPC unless an FCE proxy is configured).
    #[cfg(feature = "fce")]
    pub(crate) transport: Transport,
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

    /// Get an environment variable value
    pub fn get_env(&self, key: &str) -> Option<&String> {
        self.env_vars.get(key)
    }

    /// Fetch configuration from the server and cache it
    pub async fn fetch_config(&self) -> Result<()> {
        // With an FCE transport, discover config over `/direct` too. Otherwise a
        // client that routes every ACTION through the proxy would still need
        // arborter gRPC reachability just to learn market decimals — which is
        // exactly the dependency the FCE transport exists to remove.
        #[cfg(feature = "fce")]
        if self.uses_fce() {
            let config = crate::commands::trading::fce_actions::get_config(self).await?;
            let mut guard = self
                .config
                .write()
                .expect("AspensClient config lock poisoned");
            *guard = Some(config);
            return Ok(());
        }
        let config = crate::commands::config::get_config(self.stack_url.to_string()).await?;
        let mut guard = self
            .config
            .write()
            .expect("AspensClient config lock poisoned");
        *guard = Some(config);
        Ok(())
    }

    /// Get the cached configuration, fetching it if necessary
    pub async fn get_config(&self) -> Result<GetConfigResponse> {
        // Check if we have a cached config
        {
            let guard = self
                .config
                .read()
                .expect("AspensClient config lock poisoned");
            if let Some(config) = guard.as_ref() {
                return Ok(config.clone());
            }
        }

        // No cached config, fetch it
        self.fetch_config().await?;

        // Return the newly fetched config
        let guard = self
            .config
            .read()
            .expect("AspensClient config lock poisoned");
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| eyre::eyre!("Failed to fetch configuration"))
    }

    /// The configured trading-action transport.
    #[cfg(feature = "fce")]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// True when trading actions route through the FCE proxy.
    #[cfg(feature = "fce")]
    pub fn uses_fce(&self) -> bool {
        matches!(self.transport, Transport::Fce(_))
    }

    /// The FCE proxy client, when an FCE transport is configured.
    #[cfg(feature = "fce")]
    pub fn fce(&self) -> Option<&crate::fce::FceClient> {
        match &self.transport {
            Transport::Fce(c) => Some(c),
            Transport::Grpc => None,
        }
    }
}

/// Builder for AspensClient
#[derive(Default)]
pub struct AspensClientBuilder {
    stack_url: Option<Url>,
    env_file_path: Option<String>,
    /// (proxy_url, api_key) for the FCE transport, if set explicitly.
    #[cfg(feature = "fce")]
    fce: Option<(String, Option<String>)>,
}

impl AspensClientBuilder {
    /// Set the Aspens Market Stack Url
    pub fn with_url(mut self, url: impl Into<String>) -> Result<Self> {
        let url_str = url.into();
        self.stack_url = Some(Url::parse(&url_str).context("Invalid URL")?);
        Ok(self)
    }

    /// Set custom environment file path (defaults to .env)
    pub fn with_env_file(mut self, path: impl Into<String>) -> Self {
        self.env_file_path = Some(path.into());
        self
    }

    /// Route trading actions through the FCE proxy at `proxy_url`, authenticating
    /// with `api_key` (the proxy's `DIRECT_API_KEY`, sent as `X-API-Key`).
    /// Config discovery still uses gRPC against the stack URL.
    #[cfg(feature = "fce")]
    pub fn with_fce(mut self, proxy_url: impl Into<String>, api_key: Option<String>) -> Self {
        self.fce = Some((proxy_url.into(), api_key));
        self
    }

    /// Build the AspensClient
    pub fn build(self) -> Result<AspensClient> {
        // Load environment file (defaults to .env)
        let env_file = self.env_file_path.unwrap_or_else(|| ".env".to_string());

        let env_vars = load_env_file(&env_file)?;

        let stack_url = self
            .stack_url
            .or_else(|| {
                env_vars
                    .get("ASPENS_MARKET_STACK_URL")
                    .and_then(|u| Url::parse(u).ok())
            })
            .ok_or_else(|| {
                eyre::eyre!(
                    "aspens stack URL is not set: use AspensClient::builder().with_url(\"...\"), \
                     or set ASPENS_MARKET_STACK_URL in {} (or in the process environment before build).",
                    env_file
                )
            })?;

        // FCE transport: explicit `.with_fce(...)` wins; otherwise auto-detect
        // from `EXT_PROXY_URL` (+ optional `DIRECT_API_KEY`) in the env file /
        // process env. Absent both, actions use gRPC.
        #[cfg(feature = "fce")]
        let transport = {
            let cfg = self.fce.or_else(|| {
                env_vars
                    .get("EXT_PROXY_URL")
                    .filter(|u| !u.is_empty())
                    .map(|u| (u.clone(), env_vars.get("DIRECT_API_KEY").cloned()))
            });
            match cfg {
                Some((url, key)) => {
                    let mut fce = crate::fce::FceClient::new(url, key)?;
                    // The ext-proxy queues direct actions, so end-to-end latency
                    // is the queue depth, not the arborter. A backlogged proxy
                    // pushes a result minutes out and the 15 × 2s default gives
                    // up long before it lands — the action still executed, which
                    // reads as a silent failure. Let the caller widen the wait.
                    if let Some(attempts) = env_vars
                        .get("FCE_POLL_ATTEMPTS")
                        .and_then(|v| v.parse::<u32>().ok())
                    {
                        let interval = env_vars
                            .get("FCE_POLL_INTERVAL_SECS")
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(2);
                        fce = fce.with_polling(attempts, std::time::Duration::from_secs(interval));
                    }
                    Transport::Fce(fce)
                }
                None => Transport::Grpc,
            }
        };

        Ok(AspensClient {
            stack_url,
            env_vars,
            config: Arc::new(RwLock::new(None)),
            #[cfg(feature = "fce")]
            transport,
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
                unsafe {
                    std::env::set_var(&key, &value);
                }
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
    fn test_builder_with_url() {
        let client = AspensClient::builder()
            .with_url("http://example.com:8080")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(client.stack_url().as_str(), "http://example.com:8080/");
    }

    #[test]
    fn test_builder_requires_stack_url() {
        let file = NamedTempFile::new().unwrap();
        let err = AspensClient::builder()
            .with_env_file(file.path().to_str().unwrap())
            .build()
            .err()
            .expect("builder should fail without ASPENS_MARKET_STACK_URL");
        let msg = err.to_string();
        assert!(
            msg.contains("ASPENS_MARKET_STACK_URL") && msg.contains("stack URL"),
            "unexpected error: {}",
            msg
        );
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

    /// Builds an FCE-transport client from an env file carrying `extra` lines.
    #[cfg(feature = "fce")]
    fn fce_client_from_env(extra: &[&str]) -> AspensClient {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "ASPENS_MARKET_STACK_URL=http://example.com:8080").unwrap();
        writeln!(file, "EXT_PROXY_URL=http://example.com:6674").unwrap();
        for line in extra {
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();
        AspensClient::builder()
            .with_env_file(file.path().to_str().unwrap())
            .build()
            .unwrap()
    }

    #[test]
    #[cfg(feature = "fce")]
    fn fce_poll_schedule_defaults_when_env_absent() {
        let client = fce_client_from_env(&[]);
        let (attempts, interval) = client.fce().expect("FCE transport").polling();
        assert_eq!(attempts, 15);
        assert_eq!(interval, std::time::Duration::from_secs(2));
    }

    #[test]
    #[cfg(feature = "fce")]
    fn fce_poll_schedule_honours_env() {
        let client = fce_client_from_env(&["FCE_POLL_ATTEMPTS=200", "FCE_POLL_INTERVAL_SECS=10"]);
        let (attempts, interval) = client.fce().expect("FCE transport").polling();
        assert_eq!(attempts, 200);
        assert_eq!(interval, std::time::Duration::from_secs(10));
    }

    /// The interval is optional: attempts alone widens the wait, keeping the
    /// 2s default cadence.
    #[test]
    #[cfg(feature = "fce")]
    fn fce_poll_attempts_alone_keeps_default_interval() {
        let client = fce_client_from_env(&["FCE_POLL_ATTEMPTS=50"]);
        let (attempts, interval) = client.fce().expect("FCE transport").polling();
        assert_eq!(attempts, 50);
        assert_eq!(interval, std::time::Duration::from_secs(2));
    }

    /// A garbled value must not silently become a 0-attempt schedule — that
    /// would turn every action into an instant "no result" while it executes.
    #[test]
    #[cfg(feature = "fce")]
    fn fce_poll_schedule_ignores_unparseable_values() {
        let client = fce_client_from_env(&["FCE_POLL_ATTEMPTS=soon", "FCE_POLL_INTERVAL_SECS=x"]);
        let (attempts, interval) = client.fce().expect("FCE transport").polling();
        assert_eq!(attempts, 15);
        assert_eq!(interval, std::time::Duration::from_secs(2));
    }
}
