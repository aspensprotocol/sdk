//! Stack configuration commands.
//!
//! Wraps the `ConfigService` gRPC surface and adds convenience helpers
//! for loading `GetConfigResponse` from files, looking up chains /
//! tokens / markets, and fetching signer public keys + attestation
//! reports.

/// Generated protobuf bindings for the `arborter_config.v1` service.
#[allow(missing_docs)]
pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.v1.rs");
}

use config_pb::{Chain, GetConfigRequest, GetConfigResponse, Market, Token};
use eyre::{Result, bail};
use std::fs;
use std::path::Path;
use tracing::info;

use crate::grpc::create_channel;

/// Raw config fetch from the trading server — NO local RPC overrides applied.
/// Used by the `download_*` helpers, which should snapshot exactly what the
/// server returned (masked `rpcs`), not bake in a client's local override.
async fn fetch_config(url: String) -> Result<GetConfigResponse> {
    use config_pb::config_service_client::ConfigServiceClient;

    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);
    let request = tonic::Request::new(GetConfigRequest {});
    let response = client.get_config(request).await?;

    Ok(response.into_inner())
}

/// Fetch configuration from the trading server, with local RPC overrides
/// applied. The server masks each endpoint's `url` in its response (it can
/// embed an API key), so a client supplies its own endpoint via
/// `ASPENS_RPC_URL_<NETWORK>` — see [`crate::chain_client::resolve_rpc_url`].
pub async fn get_config(url: String) -> Result<GetConfigResponse> {
    let mut config = fetch_config(url).await?;
    config.apply_rpc_overrides();
    Ok(config)
}

/// Download configuration from server and save to file
pub async fn download_config(url: String, path: String) -> Result<()> {
    // Raw fetch: snapshot the server's config (masked rpc_url) verbatim, rather
    // than bake the caller's local RPC override into the saved file.
    let config = fetch_config(url).await?;

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
    /// Load a `GetConfigResponse` from a `.json` or `.toml` file on disk.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;

        // Determine file type based on extension
        let mut config: Self = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::from_str(&contents)?,
            Some("toml") => toml::from_str(&contents)?,
            Some(ext) => bail!("Unsupported file extension: {}", ext),
            None => bail!("No file extension found"),
        };

        // A file may carry masked endpoint urls (e.g. a download snapshot);
        // apply the same local override resolution used for a live fetch.
        config.apply_rpc_overrides();
        Ok(config)
    }

    /// Rewrite each chain's primary (first enabled) RPC endpoint url to the
    /// client's local override (`ASPENS_RPC_URL_<NETWORK>`) when set;
    /// otherwise keep the server value (an unmasked URL stays usable). The
    /// arborter masks each endpoint's `url` in its response (it can embed an
    /// API key), so this is where a client supplies its own endpoint. A chain
    /// left masked (no override) is logged at WARN — on-chain operations for
    /// it will fail until the env var is set.
    ///
    /// Only the first enabled endpoint is overridden: the SDK talks to the
    /// chain directly for its own wallet/deposit/withdraw/balance calls and
    /// only needs one workable URL (multi-endpoint failover is the
    /// arborter's own internal concern, not implemented client-side here).
    fn apply_rpc_overrides(&mut self) {
        let Some(config) = self.config.as_mut() else {
            return;
        };
        for chain in &mut config.chains {
            match crate::chain_client::chain_rpc_url(chain) {
                Ok(url) => {
                    if let Some(primary) = chain.rpcs.iter_mut().find(|e| e.enabled) {
                        primary.url = url;
                    }
                }
                Err(_) => tracing::warn!(
                    network = %chain.network,
                    env = %crate::chain_client::rpc_override_env_key(&chain.network),
                    "chain has no usable RPC endpoint (masked/unset, or no enabled endpoint) \
                     and no local RPC override is set; on-chain operations for this chain will \
                     fail until you set this env var"
                ),
            }
        }
    }

    /// Look up a chain by its `network` name (e.g. `"base-sepolia"`).
    pub fn get_chain(&self, network: &str) -> Option<&Chain> {
        self.config
            .as_ref()?
            .chains
            .iter()
            .find(|chain| chain.network == network)
    }

    /// Look up a token on `network` by its `symbol` (e.g. `"USDC"`).
    pub fn get_token(&self, network: &str, symbol: &str) -> Option<&Token> {
        self.get_chain(network)
            .and_then(|chain| chain.tokens.get(symbol))
    }

    /// Look up a market by its display `name`.
    pub fn get_market(&self, name: &str) -> Option<&Market> {
        self.config
            .as_ref()?
            .markets
            .iter()
            .find(|market| market.name == name)
    }

    /// Look up a market by the `(network, symbol)` pair of its base and quote tokens.
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

    /// Look up a chain by its EIP-155 / Solana cluster `chain_id`.
    pub fn get_chain_by_id(&self, chain_id: u32) -> Option<&Chain> {
        self.config
            .as_ref()?
            .chains
            .iter()
            .find(|chain| chain.chain_id == chain_id)
    }

    /// Look up a market by its canonical `market_id`
    /// (`chain_id::token_address::chain_id::token_address`).
    pub fn get_market_by_id(&self, market_id: &str) -> Option<&Market> {
        self.config
            .as_ref()?
            .markets
            .iter()
            .find(|market| market.market_id == market_id)
    }
}

/// Download the stack configuration from `url` and write it to `path` as JSON.
///
/// Creates parent directories if they do not exist. Unlike
/// [`download_config`], this always serializes as JSON regardless of the
/// file extension.
pub async fn download_config_to_file<P: AsRef<Path>>(url: String, path: P) -> Result<()> {
    info!("Downloading configuration to {}", path.as_ref().display());

    // Raw fetch (see download_config): keep the saved snapshot's rpc_url masked.
    let config = fetch_config(url).await?;

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
/// * `chain_network` - Optional chain name to filter by. If None, returns all chains.
pub async fn get_signer_public_key(
    url: String,
    chain_network: Option<String>,
) -> Result<GetSignerPublicKeyResponse> {
    use config_pb::GetSignerPublicKeyRequest;
    use config_pb::config_service_client::ConfigServiceClient;

    let channel = create_channel(&url).await?;

    let mut client = ConfigServiceClient::new(channel);
    let request = tonic::Request::new(GetSignerPublicKeyRequest { chain_network });
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
/// * `chain_network` - Optional chain to filter by. If None, returns all chains.
///
/// # Returns
/// A vector of SignerInfo containing public key and gas balance for each chain
pub async fn get_signer_public_key_with_balances(
    url: String,
    chain_network: Option<String>,
) -> Result<Vec<SignerInfo>> {
    // Get signer public keys
    let signer_response = get_signer_public_key(url.clone(), chain_network).await?;

    // Get config to find RPC URLs for each chain
    let config_response = get_config(url).await?;
    let config = config_response
        .config
        .ok_or_else(|| eyre::eyre!("No configuration found"))?;

    // Build a map of chain_network -> rpc_url. `get_config` above already
    // applied local RPC overrides, so this just reads the resolved primary
    // endpoint back out (falling back to empty on failure, same as before).
    let chain_rpc_map: std::collections::HashMap<String, String> = config
        .chains
        .iter()
        .map(|chain| {
            (
                chain.network.clone(),
                crate::chain_client::chain_rpc_url(chain).unwrap_or_default(),
            )
        })
        .collect();

    // Fetch balances for each signer
    let mut signer_infos = Vec::new();

    for (chain_network_key, key_info) in signer_response.chain_keys {
        let gas_balance = if let Some(rpc_url) = chain_rpc_map.get(&chain_network_key) {
            match get_native_balance(rpc_url, &key_info.public_key).await {
                Ok(balance) => Some(balance),
                Err(e) => {
                    tracing::warn!(
                        "Failed to get gas balance for chain {}: {}",
                        chain_network_key,
                        e
                    );
                    None
                }
            }
        } else {
            tracing::warn!("No RPC URL found for chain {}", chain_network_key);
            None
        };

        signer_infos.push(SignerInfo {
            chain_id: key_info.chain_id,
            chain_network: chain_network_key,
            public_key: key_info.public_key,
            gas_balance,
        });
    }

    // Sort by chain_network for consistent output
    signer_infos.sort_by_key(|info| info.chain_network.clone());

    Ok(signer_infos)
}

// Re-export attestation types for external use
pub use crate::attestation::v1::{
    AttestationReport, GetAttestationRequest, GetAttestationResponse,
};

/// Get TEE attestation from the signer
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `nonce` - Optional caller-chosen freshness nonce; the signer binds `SHA256(nonce)`
///   into the quote's REPORTDATA (any length; 32 random bytes is conventional)
pub async fn get_attestation(
    url: String,
    nonce: Option<Vec<u8>>,
) -> Result<GetAttestationResponse> {
    use config_pb::config_service_client::ConfigServiceClient;

    let channel = create_channel(&url).await?;
    let mut client = ConfigServiceClient::new(channel);

    let request = tonic::Request::new(GetAttestationRequest { nonce });
    let response = client.get_attestation(request).await?;

    Ok(response.into_inner())
}

/// Format attestation report for display
pub fn format_attestation_report(report: &AttestationReport) -> String {
    use sha2::{Digest, Sha256};
    let mut output = String::new();
    output.push_str("TEE Attestation Report:\n");
    if report.raw_quote.is_empty() {
        output.push_str("  TD Quote:      (none -- signer is not attesting)\n");
    } else {
        output.push_str(&format!(
            "  TD Quote:      {} bytes (sha256 {})\n",
            report.raw_quote.len(),
            hex::encode(Sha256::digest(&report.raw_quote))
        ));
    }
    if report.cert_chain.is_empty() {
        output.push_str("  Cert chain:    (embedded in the quote's certification data)\n");
    } else {
        output.push_str(&format!(
            "  Cert chain:    {} bytes\n",
            report.cert_chain.len()
        ));
    }
    if report.image_digest.is_empty() {
        output.push_str("  Image digest:  (none self-reported)\n");
    } else {
        output.push_str(&format!(
            "  Image digest:  {}\n",
            String::from_utf8_lossy(&report.image_digest).trim_end()
        ));
    }
    output.push_str(
        "\nThe TD Quote is the only attestation artifact; nothing above is verified.\n\
         Verify it fail-closed -- DCAP chain to Intel, pinned measurements, and the\n\
         REPORTDATA nonce binding -- with `verify-attestation` (aspens::tdx_verify).\n",
    );
    output
}

/// JSON view of an [`AttestationReport`]: the raw quote (hex, for piping into
/// an offline verifier), its length + sha256, and the self-reported fields.
pub fn attestation_report_json(report: &AttestationReport) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    serde_json::json!({
        "raw_quote_len": report.raw_quote.len(),
        "raw_quote_sha256": hex::encode(Sha256::digest(&report.raw_quote)),
        "raw_quote": hex::encode(&report.raw_quote),
        "cert_chain_len": report.cert_chain.len(),
        "image_digest": String::from_utf8_lossy(&report.image_digest),
    })
}

// No unit tests here: the three that existed were `#[ignore]`d against
// `../example/config.{json,toml}`, fixtures deleted from the repo in
// 718534e (2025-05-30), so they could not run even with `--ignored`.
