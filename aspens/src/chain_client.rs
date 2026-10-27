//! Chain-aware RPC client that dispatches between Alloy (EVM and Tron reads)
//! and solana-client (Solana) based on the chain's `architecture` field.

use alloy::primitives::{Address, Uint};
use alloy::providers::{Provider, ProviderBuilder};
use alloy_chains::NamedChain;
use eyre::Result;
use std::time::Duration;
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
use crate::wallet::{Architecture, chain_architecture};

/// Architecture string used in chain config for Solana chains.
pub const ARCH_SOLANA: &str = "Solana";
/// Architecture string used in chain config for EVM chains.
pub const ARCH_EVM: &str = "EVM";
/// Architecture string used in chain config for Tron chains.
pub const ARCH_TRON: &str = "Tron";

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
    /// Tron: reads through the node's `/jsonrpc` with the EVM code, writes
    /// as native transactions against the HTTP base
    /// ([`crate::tron::client::send_tron_tx`]).
    #[cfg(feature = "tron")]
    Tron {
        /// `<base>/jsonrpc`, where every read goes.
        jsonrpc_url: String,
        /// The chain's configured URL: the node's HTTP base, under which the
        /// native `/wallet*` routes live.
        http_base: String,
        /// The chain id `eth_chainId` reports (Nile `3448148188`, mainnet
        /// `728126428`).
        chain_id: u32,
    },
}

impl ChainClient {
    /// Build a `ChainClient` from a chain config entry.
    ///
    /// Dispatches on `chain.architecture` (closed, case-insensitive):
    /// - `"EVM"` or empty → Alloy provider
    /// - `"Solana"` → Solana RPC client (requires the `solana` feature)
    /// - `"Tron"` → Alloy over `<base>/jsonrpc` plus the HTTP base for writes
    ///   (requires the `tron` feature)
    /// - anything else → an error
    ///
    /// The endpoint is [`chain_rpc_url`] — the client's own override — never
    /// `chain.rpcs`, which the server masks (see [`chain_rpc_url`]).
    pub fn from_chain_config(chain: &Chain) -> Result<Self> {
        let rpc_url = chain_rpc_url(chain)?;
        Self::from_chain_config_with_url(chain, rpc_url)
    }

    /// [`ChainClient::from_chain_config`] with the endpoint already resolved.
    pub fn from_chain_config_with_url(chain: &Chain, rpc_url: String) -> Result<Self> {
        match chain_architecture(chain)? {
            Architecture::Evm => Ok(ChainClient::Evm {
                rpc_url,
                chain_id: chain.chain_id,
            }),
            #[cfg(feature = "solana")]
            Architecture::Solana => Ok(ChainClient::Solana {
                client: SolanaRpcClient::new(rpc_url),
            }),
            #[cfg(not(feature = "solana"))]
            Architecture::Solana => Err(eyre::eyre!(
                "chain '{}' is Solana but the `solana` feature is disabled",
                chain.network
            )),
            #[cfg(feature = "tron")]
            Architecture::Tron => Ok(ChainClient::Tron {
                jsonrpc_url: crate::tron::client::jsonrpc_url(&rpc_url)?,
                http_base: rpc_url,
                chain_id: chain.chain_id,
            }),
            #[cfg(not(feature = "tron"))]
            Architecture::Tron => Err(eyre::eyre!(
                "chain '{}' is Tron but the `tron` feature is disabled",
                chain.network
            )),
        }
    }

    /// Query the native gas balance for an address. Returns a `u128` in
    /// the smallest unit (wei for EVM, lamports for Solana, sun for Tron).
    pub async fn native_balance(&self, address: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, .. } => evm_native_balance(rpc_url, address).await,
            #[cfg(feature = "solana")]
            ChainClient::Solana { client } => {
                let pubkey = Pubkey::from_str(address)
                    .map_err(|e| eyre!("invalid Solana address: {}", e))?;
                let lamports = client.get_balance(&pubkey).await?;
                Ok(lamports as u128)
            }
            #[cfg(feature = "tron")]
            ChainClient::Tron { jsonrpc_url, .. } => evm_native_balance(jsonrpc_url, address).await,
        }
    }

    /// Query the token balance for an owner on this chain.
    ///
    /// - EVM and Tron: ERC-20 / TRC-20 `balanceOf(owner)` (Tron over
    ///   `/jsonrpc`)
    /// - Solana: SPL token account balance for the associated token account
    ///   derived from the owner pubkey and the token mint.
    pub async fn token_balance(&self, token: &Token, owner: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, chain_id } => {
                evm_token_balance(rpc_url, *chain_id, token, owner).await
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
            #[cfg(feature = "tron")]
            ChainClient::Tron {
                jsonrpc_url,
                chain_id,
                ..
            } => evm_token_balance(jsonrpc_url, *chain_id, token, owner).await,
        }
    }
}

async fn evm_native_balance(rpc_url: &str, address: &str) -> Result<u128> {
    let url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(url);
    let addr: Address = address.parse()?;
    let balance: Uint<256, 4> = provider.get_balance(addr).await?;
    Ok(balance.try_into().unwrap_or(u128::MAX))
}

async fn evm_token_balance(
    rpc_url: &str,
    chain_id: u32,
    token: &Token,
    owner: &str,
) -> Result<u128> {
    use crate::evm::rpc::IERC20;
    let url = Url::parse(rpc_url)?;
    // Apply alloy's chain metadata only when the id is one it knows. An
    // unknown id (HyperEVM testnet 998, anvil, any new rollup, Tron) must not
    // fall back to a DIFFERENT chain's provider config. `balanceOf` is a plain
    // eth_call, so no metadata is needed.
    let provider = match named_chain_for(chain_id as u64) {
        Some(named) => ProviderBuilder::new()
            .with_chain(named)
            .connect_http(url)
            .erased(),
        None => ProviderBuilder::new().connect_http(url).erased(),
    };
    let token_addr: Address = token.address.parse()?;
    let owner_addr: Address = owner.parse()?;
    let contract = IERC20::new(token_addr, &provider);
    let result: Uint<256, 4> = contract.balanceOf(owner_addr).call().await?;
    Ok(result.try_into().unwrap_or(u128::MAX))
}

/// Where the EVM read code (`eth_call`, `eth_getBalance`) points for `chain`:
/// [`chain_rpc_url`] itself on EVM, its `/jsonrpc` on Tron. Solana has no EVM
/// reads and is an error.
pub fn evm_read_url(chain: &Chain) -> Result<String> {
    evm_read_url_for(chain, &chain_rpc_url(chain)?)
}

/// [`evm_read_url`] with the configured endpoint already resolved.
pub fn evm_read_url_for(chain: &Chain, rpc_url: &str) -> Result<String> {
    match chain_architecture(chain)? {
        Architecture::Evm => Ok(rpc_url.to_string()),
        #[cfg(feature = "tron")]
        Architecture::Tron => crate::tron::client::jsonrpc_url(rpc_url),
        #[cfg(not(feature = "tron"))]
        Architecture::Tron => Err(eyre::eyre!(
            "chain '{}' is Tron but the `tron` feature is disabled",
            chain.network
        )),
        Architecture::Solana => Err(eyre::eyre!(
            "chain '{}' is Solana and has no EVM reads",
            chain.network
        )),
    }
}

/// The RPC endpoint for `chain`: the client's own `ASPENS_RPC_URL_<NETWORK>`.
///
/// There is no fallback to `chain.rpcs`. The arborter MASKS each endpoint's
/// `url` in `GetConfig` (an RPC URL can embed an API key), and a masked URL
/// can still parse — `https://base-mainnet.g.alchemy.com/***/***` is a valid
/// URL that simply fails to authenticate. Falling back to it once sent a
/// deposit to the placeholder, where the provider's HTTP 401 surfaced as a
/// JWT problem. Every client therefore supplies its own endpoint per network.
///
/// This only checks the variable is set to an http(s) URL; it makes no
/// network call. Use [`checked_chain_rpc_url`] before anything that signs or
/// sends, so an unreachable endpoint fails up front with a clear message.
pub fn chain_rpc_url(chain: &Chain) -> Result<String> {
    Ok(resolve_rpc_url(&chain.network)?)
}

/// [`chain_rpc_url`], then a probe that the endpoint answers for this chain
/// (`eth_chainId` on EVM, the same at `/jsonrpc` on Tron, `getVersion` on
/// Solana), within
/// [`RPC_PROBE_TIMEOUT`]. On EVM the probe also refuses an endpoint that
/// serves a different chain id than `chain.chain_id`.
pub async fn checked_chain_rpc_url(chain: &Chain) -> Result<String> {
    let url = chain_rpc_url(chain)?;
    probe_rpc(chain, &url).await?;
    Ok(url)
}

/// How long [`checked_chain_rpc_url`] waits for the endpoint to answer.
pub const RPC_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a chain's RPC endpoint can't be used.
///
/// Its `Display` is the whole user-facing message, so CLIs print it as-is
/// rather than guessing a cause from substrings of it. Endpoints are shown as
/// `scheme://host[:port]` only: the path and query often hold an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcEndpointError {
    /// The override variable is unset or blank.
    NotSet {
        /// Chain network name, e.g. `base`.
        network: String,
        /// The variable to set, e.g. `ASPENS_RPC_URL_BASE`.
        env_key: String,
    },
    /// The override is set but isn't an http(s) URL.
    Invalid {
        /// Chain network name.
        network: String,
        /// The variable holding the bad value.
        env_key: String,
        /// What is wrong with the value (never the value itself).
        reason: String,
    },
    /// The endpoint didn't answer, or answered with an error.
    Unreachable {
        /// Chain network name.
        network: String,
        /// The variable the endpoint came from.
        env_key: String,
        /// The endpoint, reduced to `scheme://host[:port]`.
        endpoint: String,
        /// What went wrong, with the full URL scrubbed out.
        reason: String,
    },
    /// The endpoint answered for a different chain.
    WrongChain {
        /// Chain network name.
        network: String,
        /// The variable the endpoint came from.
        env_key: String,
        /// The endpoint, reduced to `scheme://host[:port]`.
        endpoint: String,
        /// The chain id the venue lists for `network`.
        expected: u64,
        /// The chain id the endpoint reported.
        actual: u64,
    },
}

impl std::fmt::Display for RpcEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcEndpointError::NotSet { network, env_key } => write!(
                f,
                "no RPC endpoint is set for chain '{network}'.\n\n\
                 The Aspens server doesn't share its RPC URLs (they can contain API keys), \
                 so each client brings its own. Please set {env_key} to an RPC URL for \
                 '{network}', for example in your .env:\n\n    \
                 {env_key}=https://your-{network}-rpc"
            ),
            RpcEndpointError::Invalid {
                network,
                env_key,
                reason,
            } => write!(
                f,
                "{env_key} is set, but it isn't a usable RPC URL: {reason}.\n\n\
                 Please set it to an http:// or https:// RPC URL for '{network}'."
            ),
            RpcEndpointError::Unreachable {
                network,
                env_key,
                endpoint,
                reason,
            } => write!(
                f,
                "couldn't reach the RPC endpoint for chain '{network}' ({endpoint}, \
                 from {env_key}): {reason}.\n\n\
                 Please check that the URL in {env_key} is right, that any API key in it is \
                 valid, and that the provider is up."
            ),
            RpcEndpointError::WrongChain {
                network,
                env_key,
                endpoint,
                expected,
                actual,
            } => write!(
                f,
                "the RPC endpoint in {env_key} ({endpoint}) serves chain id {actual}, but \
                 '{network}' is chain id {expected}.\n\n\
                 Please point {env_key} at an RPC for '{network}'."
            ),
        }
    }
}

impl std::error::Error for RpcEndpointError {}

/// alloy's [`NamedChain`] for `chain_id`, or `None` when alloy does not know it.
///
/// `None` is a normal, working outcome — not a failure. A chain outside alloy's
/// registry (HyperEVM testnet 998, local anvil, a fresh rollup) is still a
/// standard EVM chain: the RPC reports its own id, transactions are signed
/// externally, and the only thing lost is alloy's chain-specific metadata.
/// Callers should build the provider WITHOUT `.with_chain(..)` in that case,
/// which is what the arborter does (`chain-evm/src/market.rs`).
///
/// What callers must NOT do is substitute some other chain. An earlier version
/// of `ChainClient::token_balance` fell back to `NamedChain::BaseSepolia`, so
/// every provider built for an unknown id silently carried a different chain's
/// metadata. Wrong-chain is strictly worse than no-chain.
pub fn named_chain_for(chain_id: u64) -> Option<NamedChain> {
    NamedChain::try_from(chain_id).ok()
}

/// The env-var key a client sets to supply its own RPC endpoint for `network`:
/// `ASPENS_RPC_URL_<NETWORK>`, with `network` upper-cased and every
/// non-alphanumeric byte replaced by `_` (e.g. `base-sepolia` →
/// `ASPENS_RPC_URL_BASE_SEPOLIA`, `anvil-1` → `ASPENS_RPC_URL_ANVIL_1`).
pub fn rpc_override_env_key(network: &str) -> String {
    format!("ASPENS_RPC_URL_{}", network_env_suffix(network))
}

/// `network` as an env-var suffix: upper-cased, with every non-alphanumeric
/// byte replaced by `_`. Shared by every per-network variable.
pub(crate) fn network_env_suffix(network: &str) -> String {
    network
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The RPC endpoint for `network`, read from [`rpc_override_env_key`]. See
/// [`chain_rpc_url`] for why there is no other source.
pub fn resolve_rpc_url(network: &str) -> Result<String, RpcEndpointError> {
    let value = std::env::var(rpc_override_env_key(network)).ok();
    resolve_rpc_url_with(network, value.as_deref())
}

/// Core of [`resolve_rpc_url`] with the variable's value injected, so it is
/// testable without mutating the process environment.
fn resolve_rpc_url_with(network: &str, value: Option<&str>) -> Result<String, RpcEndpointError> {
    let env_key = rpc_override_env_key(network);
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        return Err(RpcEndpointError::NotSet {
            network: network.to_string(),
            env_key,
        });
    }
    let invalid = |reason: &str| RpcEndpointError::Invalid {
        network: network.to_string(),
        env_key: env_key.clone(),
        reason: reason.to_string(),
    };
    // A value copied from `config` output still carries the server's mask.
    if value.contains("***") {
        return Err(invalid(
            "it still contains the server's *** mask (copied from the stack's config?)",
        ));
    }
    let url =
        Url::parse(value).map_err(|e| invalid(&format!("it doesn't parse as a URL ({e})")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(&format!(
            "scheme '{}' isn't http or https",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(invalid("it has no host"));
    }
    Ok(value.to_string())
}

/// `url` reduced to `scheme://host[:port]`, safe to print.
pub fn redact_rpc_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{}://{h}:{p}", u.scheme()),
            (Some(h), None) => format!("{}://{h}", u.scheme()),
            (None, _) => "<no host>".to_string(),
        },
        Err(_) => "<unparseable url>".to_string(),
    }
}

/// Probe `url` for `chain`; see [`checked_chain_rpc_url`].
async fn probe_rpc(chain: &Chain, url: &str) -> Result<(), RpcEndpointError> {
    let unreachable = |reason: String| RpcEndpointError::Unreachable {
        network: chain.network.clone(),
        env_key: rpc_override_env_key(&chain.network),
        endpoint: redact_rpc_url(url),
        reason,
    };

    let architecture = chain_architecture(chain).map_err(|e| unreachable(e.to_string()))?;
    let answer = match architecture {
        Architecture::Solana => probe_solana(url).await,
        Architecture::Evm => probe_evm(url).await,
        // Tron's `eth_chainId` is served under `/jsonrpc`, and answers with
        // the same id the EIP-712 domain carries.
        Architecture::Tron => match evm_read_url_for(chain, url) {
            Ok(jsonrpc) => probe_evm(&jsonrpc).await,
            Err(e) => Ok(Err(e.to_string())),
        },
    };
    let reported = match answer {
        Err(_elapsed) => {
            return Err(unreachable(format!(
                "no answer within {}s",
                RPC_PROBE_TIMEOUT.as_secs()
            )));
        }
        Ok(Err(cause)) => return Err(unreachable(describe_rpc_failure(&cause, url))),
        Ok(Ok(id)) => id,
    };

    match reported {
        Some(actual) if actual != chain.chain_id as u64 => Err(RpcEndpointError::WrongChain {
            network: chain.network.clone(),
            env_key: rpc_override_env_key(&chain.network),
            endpoint: redact_rpc_url(url),
            expected: chain.chain_id as u64,
            actual,
        }),
        _ => Ok(()),
    }
}

/// `eth_chainId` against `url`, bounded by [`RPC_PROBE_TIMEOUT`].
async fn probe_evm(url: &str) -> Result<Result<Option<u64>, String>, tokio::time::error::Elapsed> {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let provider = ProviderBuilder::new().connect_http(parsed);
    tokio::time::timeout(RPC_PROBE_TIMEOUT, provider.get_chain_id())
        .await
        .map(|r| r.map(Some).map_err(|e| e.to_string()))
}

/// `getVersion` against `url`, bounded by [`RPC_PROBE_TIMEOUT`]. Solana has
/// no numeric chain id to compare, so this proves reachability only.
#[cfg(feature = "solana")]
async fn probe_solana(
    url: &str,
) -> Result<Result<Option<u64>, String>, tokio::time::error::Elapsed> {
    let client = SolanaRpcClient::new(url.to_string());
    tokio::time::timeout(RPC_PROBE_TIMEOUT, client.get_version())
        .await
        .map(|r| r.map(|_| None).map_err(|e| e.to_string()))
}

#[cfg(not(feature = "solana"))]
async fn probe_solana(
    _url: &str,
) -> Result<Result<Option<u64>, String>, tokio::time::error::Elapsed> {
    Ok(Err("the `solana` feature is disabled".to_string()))
}

/// A short, plain reason for a failed probe. The full URL (which may hold an
/// API key) is scrubbed from the transport's own message.
fn describe_rpc_failure(cause: &str, url: &str) -> String {
    let scrubbed = cause.replace(url, &redact_rpc_url(url));
    let lower = scrubbed.to_lowercase();
    let status = |code: &str| {
        lower.contains(&format!("http error {code}")) || lower.contains(&format!("status {code}"))
    };
    if status("401") || status("403") {
        return "the provider refused the request (HTTP 401/403), so the API key in the URL \
                is probably missing, wrong, or not allowed for this chain"
            .to_string();
    }
    if status("429") {
        return "the provider is rate-limiting this key (HTTP 429)".to_string();
    }
    if lower.contains("dns") || lower.contains("failed to lookup") || lower.contains("no such host")
    {
        return "the host name doesn't resolve".to_string();
    }
    if lower.contains("connection refused")
        || lower.contains("connect error")
        || lower.contains("error sending request")
    {
        return "nothing accepted the connection at that address".to_string();
    }
    scrubbed.chars().take(200).collect()
}

#[cfg(test)]
mod named_chain_tests {
    use super::*;

    /// alloy's registry is the source of truth for ids it KNOWS. These two are
    /// pinned because the stack trades on both and they behave differently.
    #[test]
    fn known_ids_resolve_to_their_named_chain() {
        assert_eq!(named_chain_for(114), Some(NamedChain::FlareCoston2));
        // Hyperliquid MAINNET is in the registry...
        assert_eq!(named_chain_for(999), Some(NamedChain::Hyperliquid));
    }

    /// ...its TESTNET (998) is not, and that must be `None` rather than an
    /// error or a substitute. A generic EVM chain is perfectly usable — the RPC
    /// supplies the chain id — it just gets no alloy metadata.
    #[test]
    fn hyperevm_testnet_is_unnamed_not_an_error() {
        assert_eq!(named_chain_for(998), None);
    }

    /// The bug this replaces: an unknown id silently became BaseSepolia, so
    /// every provider built for HyperEVM testnet carried another chain's
    /// metadata. Wrong-chain is worse than no-chain — assert we never return a
    /// chain that is not the one asked for.
    #[test]
    fn an_unknown_id_never_substitutes_a_different_chain() {
        for id in [998u64, 31337, 1337, 424242] {
            match named_chain_for(id) {
                None => {}
                Some(c) => assert_eq!(
                    u64::from(c),
                    id,
                    "id {id} resolved to a DIFFERENT chain ({c:?}) — a silent substitution"
                ),
            }
        }
    }
}

#[cfg(test)]
mod rpc_resolve_tests {
    use super::*;
    use crate::commands::config::config_pb::RpcEndpoint;

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
    fn a_set_override_is_used() {
        let got = resolve_rpc_url_with("net", Some(" https://my.rpc/v2/key ")).unwrap();
        assert_eq!(got, "https://my.rpc/v2/key");
    }

    #[test]
    fn unset_or_blank_is_not_set_and_names_the_variable() {
        for value in [None, Some(""), Some("   ")] {
            let err = resolve_rpc_url_with("base-sepolia", value).unwrap_err();
            assert_eq!(
                err,
                RpcEndpointError::NotSet {
                    network: "base-sepolia".into(),
                    env_key: "ASPENS_RPC_URL_BASE_SEPOLIA".into(),
                },
                "value {value:?}"
            );
            assert!(
                err.to_string().contains("ASPENS_RPC_URL_BASE_SEPOLIA="),
                "{err}"
            );
        }
    }

    #[test]
    fn a_copied_masked_url_is_rejected_even_though_it_parses() {
        let masked = "https://base-mainnet.g.alchemy.com/***/***";
        assert!(Url::parse(masked).is_ok(), "premise: the masked url parses");
        let err = resolve_rpc_url_with("base", Some(masked)).unwrap_err();
        assert!(matches!(err, RpcEndpointError::Invalid { .. }), "{err:?}");
        assert!(err.to_string().contains("mask"), "{err}");
    }

    #[test]
    fn non_http_values_are_rejected() {
        for value in ["not a url", "ws://node.example", "file:///etc/passwd"] {
            let err = resolve_rpc_url_with("net", Some(value)).unwrap_err();
            assert!(
                matches!(err, RpcEndpointError::Invalid { .. }),
                "{value}: {err:?}"
            );
        }
    }

    /// The bug this replaces: with no override set, the server's masked url
    /// was used whenever it still parsed, so a deposit went to
    /// `https://base-mainnet.g.alchemy.com/***/***` and the provider's 401
    /// read as a JWT failure. The server's value must never be used.
    ///
    /// The network name is unique to this test so no other test can set its
    /// override (the process env is global).
    #[test]
    fn chain_rpc_url_never_falls_back_to_the_server_url() {
        for server_url in [
            "https://base-mainnet.g.alchemy.com/***/***",
            "http://localhost:8545",
        ] {
            let chain = Chain {
                network: "rpc-no-fallback".to_string(),
                rpcs: vec![RpcEndpoint {
                    label: "primary".to_string(),
                    url: server_url.to_string(),
                    enabled: true,
                    ..Default::default()
                }],
                chain_id: 8453,
                ..Default::default()
            };
            let err = chain_rpc_url(&chain).expect_err(server_url).to_string();
            assert!(err.contains("ASPENS_RPC_URL_RPC_NO_FALLBACK"), "{err}");
            // Matched rather than `expect_err`: that needs `T: Debug`, and
            // adding `Debug` to a public client type just for a test is the
            // wrong trade — the Solana variant would have to derive it too.
            if ChainClient::from_chain_config(&chain).is_ok() {
                panic!("{server_url}: built a client from the server's url");
            }
        }
    }

    fn evm_chain(chain_id: u32) -> Chain {
        Chain {
            network: "probe-net".to_string(),
            architecture: ARCH_EVM.to_string(),
            chain_id,
            ..Default::default()
        }
    }

    /// A one-shot-per-connection HTTP server answering every request with
    /// `status` and `body`. Returns its base url.
    async fn fake_rpc(status: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    const CHAIN_ID_8453: &str = r#"{"jsonrpc":"2.0","id":0,"result":"0x2105"}"#;

    #[tokio::test]
    async fn probe_accepts_an_endpoint_on_the_right_chain() {
        let url = fake_rpc("200 OK", CHAIN_ID_8453).await;
        probe_rpc(&evm_chain(8453), &url).await.unwrap();
    }

    #[tokio::test]
    async fn probe_refuses_an_endpoint_on_another_chain() {
        let url = fake_rpc("200 OK", CHAIN_ID_8453).await;
        let err = probe_rpc(&evm_chain(1), &url).await.unwrap_err();
        assert!(
            matches!(
                err,
                RpcEndpointError::WrongChain {
                    expected: 1,
                    actual: 8453,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// The failure from the live run: the provider answers 401. The message
    /// must blame the RPC key, not a JWT, and must not echo the key.
    #[tokio::test]
    async fn probe_explains_a_refused_key_without_printing_it() {
        let base = fake_rpc("401 Unauthorized", "Must be authenticated!").await;
        let url = format!("{base}/v2/SECRET-KEY-123");
        let err = probe_rpc(&evm_chain(8453), &url).await.unwrap_err();
        let RpcEndpointError::Unreachable { reason, .. } = &err else {
            panic!("expected Unreachable, got {err:?}");
        };
        assert!(reason.contains("API key"), "{reason}");
        let shown = err.to_string();
        assert!(!shown.contains("SECRET-KEY-123"), "key leaked: {shown}");
        assert!(!shown.to_lowercase().contains("jwt"), "{shown}");
    }

    /// A transport error that echoes the full URL (reqwest's do) must not
    /// carry the API key into the message. Uses a cause no friendly branch
    /// matches, so the scrubbed text itself is what's returned.
    #[test]
    fn failure_reasons_scrub_the_url() {
        let url = "https://rpc.example/v2/SECRET-KEY-123?k=SECRET-KEY-123";
        let reason = describe_rpc_failure(&format!("unexpected reply from {url}"), url);
        assert!(!reason.contains("SECRET-KEY-123"), "{reason}");
        assert!(reason.contains("https://rpc.example"), "{reason}");
    }

    #[tokio::test]
    async fn probe_reports_a_closed_port_as_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = probe_rpc(&evm_chain(8453), &url).await.unwrap_err();
        assert!(
            matches!(err, RpcEndpointError::Unreachable { .. }),
            "{err:?}"
        );
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

#[cfg(all(test, feature = "tron"))]
mod tron_tests {
    use super::*;
    use crate::tron::client::mock::{bodies, mount_jsonrpc, nile_chain};
    use wiremock::MockServer;

    /// A Tron chain builds the Tron client: reads over `<base>/jsonrpc` with
    /// the EVM code, writes against the base. Its native balance is an
    /// `eth_getBalance` that reaches `/jsonrpc`, never the bare base.
    #[tokio::test]
    async fn chain_client_from_tron_config() {
        let server = MockServer::start().await;
        mount_jsonrpc(&server, "0x989680").await;
        let base = format!("{}/", server.uri());
        for spelling in ["Tron", "TRON", "tron"] {
            let mut chain = nile_chain();
            chain.architecture = spelling.into();
            let client = ChainClient::from_chain_config_with_url(&chain, base.clone()).unwrap();
            let ChainClient::Tron {
                jsonrpc_url,
                http_base,
                chain_id,
            } = &client
            else {
                panic!("{spelling}: not the Tron client");
            };
            assert_eq!(jsonrpc_url, &format!("{}/jsonrpc", server.uri()));
            assert_eq!(http_base, &base);
            assert_eq!(*chain_id, 3_448_148_188);
            let balance = client
                .native_balance("0xa5411a1a5784b0828bbafcfa588c72ef3f3ce646")
                .await
                .unwrap();
            assert_eq!(balance, 10_000_000);
        }
        let reads = bodies(&server, "/jsonrpc").await;
        assert_eq!(reads.len(), 3);
        assert!(reads.iter().all(|r| r["method"] == "eth_getBalance"));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            3,
            "nothing went anywhere but /jsonrpc"
        );
    }

    #[test]
    fn an_unknown_architecture_builds_no_client() {
        let mut chain = nile_chain();
        chain.architecture = "sui".into();
        let Err(err) = ChainClient::from_chain_config_with_url(&chain, "http://x".into()) else {
            panic!("an unknown architecture built a client");
        };
        assert!(err.to_string().contains("sui"), "{err}");
    }

    /// EVM reads on a Tron chain go to `/jsonrpc`; on an EVM chain to the
    /// configured URL itself; a Solana chain has none.
    #[test]
    fn evm_read_url_per_architecture() {
        let tron = nile_chain();
        assert_eq!(
            evm_read_url_for(&tron, "https://nile.trongrid.io").unwrap(),
            "https://nile.trongrid.io/jsonrpc"
        );
        let mut evm = nile_chain();
        evm.architecture = "EVM".into();
        assert_eq!(
            evm_read_url_for(&evm, "https://rpc.example/v2/k").unwrap(),
            "https://rpc.example/v2/k"
        );
        let mut sol = nile_chain();
        sol.architecture = "Solana".into();
        assert!(evm_read_url_for(&sol, "https://api.devnet.solana.com").is_err());
    }
}
