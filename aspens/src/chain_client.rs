//! Chain-aware RPC client that dispatches between Alloy (EVM) and
//! solana-client (Solana) based on the chain's `architecture` field.

use alloy::primitives::{Address, Uint};
use alloy::providers::{Provider, ProviderBuilder};
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

#[cfg(feature = "solana")]
use eyre::eyre;
#[cfg(feature = "solana")]
use solana_client::nonblocking::rpc_client::RpcClient as SolanaRpcClient;
#[cfg(feature = "solana")]
use solana_sdk::pubkey::Pubkey;
#[cfg(feature = "solana")]
use std::str::FromStr;

use crate::commands::config::config_pb::{Chain, Token};

/// Architecture string used in chain config for Solana chains.
pub const ARCH_SOLANA: &str = "Solana";
/// Architecture string used in chain config for EVM chains.
pub const ARCH_EVM: &str = "EVM";

/// A curve-aware RPC client.
pub enum ChainClient {
    /// EVM provider (Alloy).
    Evm {
        /// HTTP(S) JSON-RPC endpoint for the EVM chain.
        rpc_url: String,
        /// EIP-155 chain id used to construct the Alloy provider.
        chain_id: u32,
    },
    /// Solana RPC client.
    #[cfg(feature = "solana")]
    Solana {
        /// Async Solana JSON-RPC client.
        client: SolanaRpcClient,
    },
}

impl ChainClient {
    /// Build a `ChainClient` from a chain config entry.
    ///
    /// Dispatches on `chain.architecture`:
    /// - `"EVM"` (or empty/anything else for backward compat) → Alloy provider
    /// - `"Solana"` → Solana RPC client (requires the `solana` feature)
    pub fn from_chain_config(chain: &Chain) -> Result<Self> {
        if chain.architecture.eq_ignore_ascii_case(ARCH_SOLANA) {
            #[cfg(feature = "solana")]
            {
                return Ok(ChainClient::Solana {
                    client: SolanaRpcClient::new(chain.rpc_url.clone()),
                });
            }
            #[cfg(not(feature = "solana"))]
            {
                return Err(eyre::eyre!(
                    "chain '{}' is Solana but the `solana` feature is disabled",
                    chain.network
                ));
            }
        }
        Ok(ChainClient::Evm {
            rpc_url: chain.rpc_url.clone(),
            chain_id: chain.chain_id,
        })
    }

    /// Query the native gas balance for an address. Returns a `u128` in
    /// the smallest unit (wei for EVM, lamports for Solana).
    pub async fn native_balance(&self, address: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, .. } => {
                let url = Url::parse(rpc_url)?;
                let provider = ProviderBuilder::new().connect_http(url);
                let addr: Address = address.parse()?;
                let balance: Uint<256, 4> = provider.get_balance(addr).await?;
                Ok(balance.try_into().unwrap_or(u128::MAX))
            }
            #[cfg(feature = "solana")]
            ChainClient::Solana { client } => {
                let pubkey = Pubkey::from_str(address)
                    .map_err(|e| eyre!("invalid Solana address: {}", e))?;
                let lamports = client.get_balance(&pubkey).await?;
                Ok(lamports as u128)
            }
        }
    }

    /// Query the token balance for an owner on this chain.
    ///
    /// - EVM: ERC-20 `balanceOf(owner)`
    /// - Solana: SPL token account balance for the associated token account
    ///   derived from the owner pubkey and the token mint.
    pub async fn token_balance(&self, token: &Token, owner: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, chain_id } => {
                use crate::evm::rpc::IERC20;
                let url = Url::parse(rpc_url)?;
                let named_chain =
                    NamedChain::try_from(*chain_id as u64).unwrap_or(NamedChain::BaseSepolia);
                let provider = ProviderBuilder::new()
                    .with_chain(named_chain)
                    .connect_http(url);
                let token_addr: Address = token.address.parse()?;
                let owner_addr: Address = owner.parse()?;
                let contract = IERC20::new(token_addr, &provider);
                let result: Uint<256, 4> = contract.balanceOf(owner_addr).call().await?;
                Ok(result.try_into().unwrap_or(u128::MAX))
            }
            #[cfg(feature = "solana")]
            ChainClient::Solana { client } => {
                let owner_pubkey = Pubkey::from_str(owner)
                    .map_err(|e| eyre!("invalid Solana owner address: {}", e))?;
                let mint_pubkey = Pubkey::from_str(&token.address)
                    .map_err(|e| eyre!("invalid Solana mint address: {}", e))?;
                let ata =
                    crate::solana::derive_associated_token_account(&owner_pubkey, &mint_pubkey);

                match client.get_token_account_balance(&ata).await {
                    Ok(b) => b
                        .amount
                        .parse::<u128>()
                        .map_err(|e| eyre!("invalid Solana token balance: {}", e)),
                    Err(_) => Ok(0),
                }
            }
        }
    }
}

/// The env-var key a client sets to supply its own RPC endpoint for `network`:
/// `ASPENS_RPC_URL_<NETWORK>`, with `network` upper-cased and every
/// non-alphanumeric byte replaced by `_` (e.g. `base-sepolia` →
/// `ASPENS_RPC_URL_BASE_SEPOLIA`, `anvil-1` → `ASPENS_RPC_URL_ANVIL_1`).
pub fn rpc_override_env_key(network: &str) -> String {
    let suffix: String = network
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("ASPENS_RPC_URL_{suffix}")
}

/// Resolve the RPC endpoint to use for `network`.
///
/// The arborter masks `rpc_url` in its `GetConfig` response (it can embed an API
/// key), so the server value is usually unusable. Resolution order:
/// 1. the per-network override env var ([`rpc_override_env_key`]), if set; else
/// 2. `server_rpc_url`, when it is a real URL (an unmasked server / local
///    fixture); else
/// 3. an actionable error naming the env var to set.
pub fn resolve_rpc_url(network: &str, server_rpc_url: &str) -> Result<String> {
    let override_val = std::env::var(rpc_override_env_key(network)).ok();
    resolve_rpc_url_with(network, override_val.as_deref(), server_rpc_url)
}

/// Core of [`resolve_rpc_url`] with the override value injected, so the
/// precedence logic is testable without mutating the process environment.
fn resolve_rpc_url_with(
    network: &str,
    override_val: Option<&str>,
    server_rpc_url: &str,
) -> Result<String> {
    if let Some(v) = override_val {
        let v = v.trim();
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    let server = server_rpc_url.trim();
    if !server.is_empty() && Url::parse(server).is_ok() {
        return Ok(server.to_string());
    }
    Err(eyre::eyre!(
        "no usable RPC endpoint for chain '{network}': the server masks rpc_url in its config \
         (it can embed an API key). Set {} to your own RPC URL for '{network}'.",
        rpc_override_env_key(network)
    ))
}

#[cfg(test)]
mod rpc_resolve_tests {
    use super::*;

    #[test]
    fn env_key_uppercases_and_sanitizes() {
        assert_eq!(
            rpc_override_env_key("base-sepolia"),
            "ASPENS_RPC_URL_BASE_SEPOLIA"
        );
        assert_eq!(rpc_override_env_key("anvil-1"), "ASPENS_RPC_URL_ANVIL_1");
        assert_eq!(
            rpc_override_env_key("solana-devnet"),
            "ASPENS_RPC_URL_SOLANA_DEVNET"
        );
    }

    #[test]
    fn override_wins_over_masked_server_value() {
        let got = resolve_rpc_url_with("net", Some("https://my.rpc/v2/key"), "********").unwrap();
        assert_eq!(got, "https://my.rpc/v2/key");
    }

    #[test]
    fn blank_override_falls_through_to_usable_server_value() {
        let got = resolve_rpc_url_with("net", Some("   "), "https://server.example").unwrap();
        assert_eq!(got, "https://server.example");
    }

    #[test]
    fn no_override_uses_unmasked_server_value() {
        let got = resolve_rpc_url_with("net", None, "http://localhost:8545").unwrap();
        assert_eq!(got, "http://localhost:8545");
    }

    #[test]
    fn masked_value_without_override_errors_with_env_key() {
        let err = resolve_rpc_url_with("base-sepolia", None, "********")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ASPENS_RPC_URL_BASE_SEPOLIA"),
            "actionable: {err}"
        );
    }

    #[test]
    fn empty_server_without_override_errors() {
        assert!(resolve_rpc_url_with("net", None, "").is_err());
    }
}

#[cfg(all(test, feature = "solana"))]
mod tests {
    use super::*;
    use crate::solana::derive_associated_token_account;

    #[test]
    fn ata_derivation_matches_known_pair() {
        let owner = Pubkey::from_str("11111111111111111111111111111112").unwrap();
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let ata = derive_associated_token_account(&owner, &mint);
        assert_ne!(ata, Pubkey::default());
    }
}
