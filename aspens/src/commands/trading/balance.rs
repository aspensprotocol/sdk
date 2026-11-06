use alloy::primitives::{Address, Uint};
use alloy::providers::{Provider, ProviderBuilder};
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};
use eyre::Result;
use std::collections::HashMap;
use tracing::{info, warn};
use url::Url;

#[cfg(test)]
use crate::chain_client::ARCH_SOLANA;
use crate::chain_client::ChainClient;
use crate::chain_client::named_chain_for;
use crate::commands::config::config_pb::{Chain, Configuration, GetConfigResponse};
use crate::evm::rpc::{IERC20, MidribV3};
#[cfg(test)]
use crate::wallet::CurveType;
use crate::wallet::{Architecture, Wallet};

/// Represents a unique token across all chains
#[derive(Debug, Clone)]
struct TokenInfo {
    symbol: String,
}

/// One balance reading: a raw integer in the unit's base units, or the reason
/// there is none.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum BalanceValue {
    /// Base units (the token's own decimals; the native unit's for gas).
    Raw(u128),
    /// No supplied wallet matches the chain's curve.
    NoWallet,
    /// The read failed or has no value; the text is the SDK's own reason. It
    /// is not redacted: a consumer that shows it to others must do so itself.
    Error(String),
}

/// Error texts the table has always shown as they are.
const DISPLAYED_VERBATIM: &[&str] = &[
    "error",
    "not deployed",
    "bad mint",
    "bad address",
    "no rpc",
    "missing token",
    "bad architecture",
    "solana feature disabled",
];

const EXCEEDS_U128: &str = "exceeds u128: ";

/// A 256-bit chain balance as a reading; one that does not fit `u128` is an
/// `Error` carrying its digits, never a panic or a wrap.
fn u256_balance(v: Uint<256, 4>) -> BalanceValue {
    match u128::try_from(v) {
        Ok(n) => BalanceValue::Raw(n),
        Err(_) => BalanceValue::Error(format!("{EXCEEDS_U128}{v}")),
    }
}

impl BalanceValue {
    /// Classify a reading the read helpers produce as text: a decimal integer
    /// is a balance, anything else ("error", "not deployed", ...) is the reason.
    fn from_wire(s: String) -> Self {
        match s.parse::<u128>() {
            Ok(v) => BalanceValue::Raw(v),
            Err(_) => BalanceValue::Error(s),
        }
    }

    /// The text the balances table shows for this reading. A failed read shows
    /// as "error" (the reason is in the data and the log); the fixed
    /// not-available texts in [`DISPLAYED_VERBATIM`] show as they are.
    fn display(&self, decimals: u32) -> String {
        match self {
            BalanceValue::Raw(v) => format_balance_with_decimals(&v.to_string(), decimals),
            BalanceValue::NoWallet => "no wallet".to_string(),
            BalanceValue::Error(e) if DISPLAYED_VERBATIM.contains(&e.as_str()) => e.clone(),
            // A balance too big for u128 used to print as its digits.
            BalanceValue::Error(e) if e.starts_with(EXCEEDS_U128) => {
                e[EXCEEDS_U128.len()..].to_string()
            }
            BalanceValue::Error(_) => "error".to_string(),
        }
    }
}

/// Balance information for a token on a specific chain
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChainBalanceView {
    pub network: String,
    /// Decimals of THIS chain's token: the raw values below are in its units,
    /// and one symbol can have different decimals on different chains.
    pub decimals: u32,
    pub wallet: BalanceValue,
    pub available: BalanceValue,
    pub locked: BalanceValue,
}

impl ChainBalanceView {
    fn all(network: String, decimals: u32, v: BalanceValue) -> Self {
        Self {
            network,
            decimals,
            wallet: v.clone(),
            available: v.clone(),
            locked: v,
        }
    }

    fn all_err(network: String, decimals: u32, msg: &str) -> Self {
        Self::all(network, decimals, BalanceValue::Error(msg.to_string()))
    }
}

/// Native gas token balance for a chain
#[derive(Debug, Clone, serde::Serialize)]
pub struct NativeBalanceView {
    pub network: String,
    /// Decimals of the native unit the balance is in. `None` when the chain's
    /// architecture is unknown: the unit is then unknown too, and `balance`
    /// says so.
    pub decimals: Option<u32>,
    pub balance: BalanceValue,
}

/// Aggregated balance for a single token across all chains
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenBalances {
    pub symbol: String,
    pub chains: Vec<ChainBalanceView>,
}

/// Everything `fetch_balances` read: per-token rows per chain, and native gas.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Balances {
    pub tokens: Vec<TokenBalances>,
    pub native: Vec<NativeBalanceView>,
}

/// Decimals of a chain's native gas unit as `native_balance` reports it: wei
/// (18) on EVM, sun (6) on Tron. Solana keeps the table's long-standing 18.
/// An unknown architecture is an `Err`, never a guessed unit.
fn native_decimals(chain: &Chain) -> Result<u32> {
    Ok(match crate::wallet::chain_architecture(chain)? {
        Architecture::Tron => 6,
        Architecture::Evm | Architecture::Solana => 18,
    })
}

/// Extract all unique tokens from configuration chains
fn extract_all_tokens_from_config(config: &Configuration) -> HashMap<String, TokenInfo> {
    let mut tokens: HashMap<String, TokenInfo> = HashMap::new();

    for chain in &config.chains {
        for symbol in chain.tokens.keys() {
            tokens.entry(symbol.clone()).or_insert_with(|| TokenInfo {
                symbol: symbol.clone(),
            });
        }
    }

    tokens
}

/// Format balance with decimals for human-readable display
fn format_balance_with_decimals(balance_str: &str, decimals: u32) -> String {
    if balance_str == "error" || balance_str == "not deployed" {
        return balance_str.to_string();
    }

    match balance_str.parse::<u128>() {
        Ok(balance) => {
            let divisor = 10_u128.pow(decimals);
            let integer_part = balance / divisor;
            let fractional_part = balance % divisor;

            // Format with proper decimal places
            format!(
                "{}.{:0width$}",
                integer_part,
                fractional_part,
                width = decimals as usize
            )
        }
        Err(_) => balance_str.to_string(),
    }
}

/// Display all token balances in a single table grouped by chain
fn display_all_token_balances(balances: &Balances) -> String {
    let all_token_balances = &balances.tokens;
    let native_balances = &balances.native;
    if all_token_balances.is_empty() {
        return String::new();
    }

    // Collect all unique chain networks across all tokens and sort them
    let mut all_chains: Vec<String> = Vec::new();
    for token_balance in all_token_balances {
        for chain_balance in &token_balance.chains {
            if !all_chains.contains(&chain_balance.network) {
                all_chains.push(chain_balance.network.clone());
            }
        }
    }
    all_chains.sort();

    let mut output = String::new();
    output.push('\n');

    output
        .push_str("═══════════════════════════════════════════════════════════════════════════\n");
    output.push_str("                                 BALANCES\n");
    output.push_str(
        "═══════════════════════════════════════════════════════════════════════════\n\n",
    );

    let mut table = Table::new();
    table.load_style(UTF8_BORDERS_ONLY);

    // Set header: Token, Wallet, Deposited, Locked
    table.set_header(vec!["Token", "Wallet", "Deposited", "Locked"]);

    // Group by chain - add rows for each chain section
    for (chain_idx, chain) in all_chains.iter().enumerate() {
        // Add chain header row (spans conceptually as a section header)
        if chain_idx > 0 {
            // Add separator row between chains
            table.add_row(vec!["", "", "", ""]);
        }

        // Add chain name as a section header
        table.add_row(vec![
            format!("── {} ──", chain),
            String::new(),
            String::new(),
            String::new(),
        ]);

        // Add native gas balance for this chain
        if let Some(native) = native_balances.iter().find(|nb| nb.network == *chain) {
            let gas_balance = match native.decimals {
                Some(decimals) => native.balance.display(decimals),
                None => native.balance.display(0),
            };
            table.add_row(vec![
                "GAS".to_string(),
                gas_balance,
                String::new(),
                String::new(),
            ]);
        }

        // Add tokens for this chain
        for token_balance in all_token_balances {
            // Find if this token exists on this chain
            if let Some(chain_balance) = token_balance.chains.iter().find(|cb| cb.network == *chain)
            {
                let decimals = chain_balance.decimals;
                let symbol = &token_balance.symbol;

                let wallet = chain_balance.wallet.display(decimals);
                let deposited = chain_balance.available.display(decimals);
                let locked = chain_balance.locked.display(decimals);

                table.add_row(vec![symbol.clone(), wallet, deposited, locked]);
            }
        }
    }

    output.push_str(&table.to_string());
    output.push('\n');

    output
}

/// Query a token balance on a specific chain using a curve-aware client.
///
/// Used by `balance_from_config_with_wallets`. For Solana chains, locked/deposited
/// balances come from the trade program; for Solana it queries the on-chain
/// `UserBalance` PDA via the Midrib program.
#[cfg(feature = "solana")]
async fn solana_user_balance(
    chain: &Chain,
    token: &crate::commands::config::config_pb::Token,
    owner_address: &str,
) -> (String, String) {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    let (program_id, instance) = match crate::solana::client::resolve_program_and_instance(chain) {
        Ok(v) => v,
        Err(_) => return ("not deployed".to_string(), "not deployed".to_string()),
    };
    let user = match Pubkey::from_str(owner_address) {
        Ok(p) => p,
        Err(_) => return ("bad address".to_string(), "bad address".to_string()),
    };
    let mint = match Pubkey::from_str(&token.address) {
        Ok(p) => p,
        Err(_) => return ("bad mint".to_string(), "bad mint".to_string()),
    };
    let rpc = match crate::chain_client::chain_rpc_url(chain) {
        Ok(u) => u,
        Err(_) => return ("no rpc".to_string(), "no rpc".to_string()),
    };
    match crate::solana::client::fetch_user_balance(&rpc, &instance, &user, &mint, &program_id)
        .await
    {
        Ok((deposited, locked)) => {
            let available = deposited.saturating_sub(locked);
            (available.to_string(), locked.to_string())
        }
        Err(e) => {
            warn!(
                "Solana UserBalance fetch failed on {}: {}",
                chain.network, e
            );
            (e.to_string(), e.to_string())
        }
    }
}

#[cfg(not(feature = "solana"))]
async fn solana_user_balance(
    _chain: &Chain,
    _token: &crate::commands::config::config_pb::Token,
    _owner_address: &str,
) -> (String, String) {
    (
        "solana feature disabled".to_string(),
        "solana feature disabled".to_string(),
    )
}

async fn query_token_balance_via_client(
    chain: &Chain,
    token_symbol: &str,
    owner_address: &str,
) -> ChainBalanceView {
    let chain_network = chain.network.clone();
    let token_decimals = chain.tokens.get(token_symbol).map_or(0, |t| t.decimals);
    // Resolved once: the arborter masks rpc_url, so without a client-side
    // override every read below would fail identically and unhelpfully.
    let rpc = match crate::chain_client::chain_rpc_url(chain) {
        Ok(u) => u,
        Err(_) => {
            return ChainBalanceView::all_err(chain_network, token_decimals, "no rpc");
        }
    };
    let token = match chain.tokens.get(token_symbol) {
        Some(t) => t,
        None => {
            return ChainBalanceView::all_err(chain_network, token_decimals, "missing token");
        }
    };

    let client = match ChainClient::from_chain_config(chain) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build client for {}: {}", chain_network, e);
            return ChainBalanceView::all_err(chain_network, token_decimals, &e.to_string());
        }
    };

    // The NATIVE sentinel is a mapping key, not a contract — `balanceOf` on it
    // reverts, which printed `error` for the native token row (C2FLR, HYPE)
    // while the GAS row above showed that very balance correctly. Read the gas
    // balance for it instead; both are 18-dec so the shared formatter agrees.
    let is_native_sentinel = token
        .address
        .eq_ignore_ascii_case(crate::evm::NATIVE_TOKEN_SENTINEL);
    let wallet_result = if is_native_sentinel {
        client.native_balance(owner_address).await
    } else {
        client.token_balance(token, owner_address).await
    };
    let wallet_balance = wallet_result.map_or_else(
        |e| {
            warn!("Failed to get wallet balance on {}: {}", chain_network, e);
            BalanceValue::Error(e.to_string())
        },
        BalanceValue::Raw,
    );

    // Locked/deposited balances come from the trade contract: the Midrib
    // program's UserBalance PDA on Solana, MidribV3 on EVM and on Tron (read
    // over its `/jsonrpc`).
    let architecture = match crate::wallet::chain_architecture(chain) {
        Ok(a) => a,
        Err(e) => {
            warn!("{e}");
            return ChainBalanceView {
                network: chain_network,
                decimals: token_decimals,
                wallet: wallet_balance,
                available: BalanceValue::Error("bad architecture".to_string()),
                locked: BalanceValue::Error("bad architecture".to_string()),
            };
        }
    };
    let (available_balance, locked_balance) = if architecture == Architecture::Solana {
        let (available, locked) = solana_user_balance(chain, token, owner_address).await;
        (
            BalanceValue::from_wire(available),
            BalanceValue::from_wire(locked),
        )
    } else {
        let rpc = match crate::chain_client::evm_read_url_for(chain, &rpc) {
            Ok(u) => u,
            Err(e) => {
                warn!("No EVM read endpoint for {}: {}", chain_network, e);
                return ChainBalanceView {
                    network: chain_network,
                    decimals: token_decimals,
                    wallet: wallet_balance,
                    available: BalanceValue::Error(e.to_string()),
                    locked: BalanceValue::Error(e.to_string()),
                };
            }
        };
        let contract_address = chain
            .trade_contract
            .as_ref()
            .map(|tc| tc.address.clone())
            .unwrap_or_default();

        if contract_address.is_empty() {
            (
                BalanceValue::Error("not deployed".to_string()),
                BalanceValue::Error("not deployed".to_string()),
            )
        } else {
            // Reuse existing EVM helpers — they need a privkey to derive the address,
            // but we already have the address. Use *_for_address variants.
            let owner: Address = match owner_address.parse() {
                Ok(a) => a,
                Err(_) => {
                    return ChainBalanceView {
                        network: chain_network,
                        decimals: token_decimals,
                        wallet: wallet_balance,
                        available: BalanceValue::Error("bad address".to_string()),
                        locked: BalanceValue::Error("bad address".to_string()),
                    };
                }
            };
            let available = call_get_balance_for_address(
                chain.chain_id as u64,
                &rpc,
                &token.address,
                &contract_address,
                owner,
            )
            .await
            .map_or_else(
                |e| {
                    warn!(
                        "Failed to get available balance on {}: {}",
                        chain_network, e
                    );
                    BalanceValue::Error(e.to_string())
                },
                u256_balance,
            );
            let locked =
                call_get_locked_balance_for_address(&rpc, &token.address, &contract_address, owner)
                    .await
                    .map_or_else(
                        |e| {
                            warn!("Failed to get locked balance on {}: {}", chain_network, e);
                            BalanceValue::Error(e.to_string())
                        },
                        u256_balance,
                    );
            (available, locked)
        }
    };

    ChainBalanceView {
        network: chain_network,
        decimals: token_decimals,
        wallet: wallet_balance,
        available: available_balance,
        locked: locked_balance,
    }
}

/// Pick the first wallet matching `chain`'s curve. Returns `None` when no
/// caller-supplied wallet matches — the chain's rows are reported as
/// "no wallet" rather than erroring out.
fn select_wallet_for_chain<'a>(chain: &Chain, wallets: &'a [&'a Wallet]) -> Option<&'a Wallet> {
    // A chain whose architecture is unknown has no wallet to read with; say
    // why rather than let it look like a missing key.
    let wanted = match crate::wallet::chain_curve(chain) {
        Ok(c) => c,
        Err(e) => {
            warn!("{e}");
            return None;
        }
    };
    wallets.iter().copied().find(|w| w.curve() == wanted)
}

/// Curve-aware, multi-wallet config-driven balance read, as data.
///
/// Each chain in the config gets matched to a wallet with the same curve
/// (EVM chains → `Wallet::Evm`, Solana chains → `Wallet::Solana`). A chain
/// with no matching wallet is reported as [`BalanceValue::NoWallet`] rather
/// than failing the whole call; a failed read is [`BalanceValue::Error`].
pub async fn fetch_balances(config: GetConfigResponse, wallets: &[&Wallet]) -> Result<Balances> {
    let configuration = config
        .config
        .ok_or_else(|| eyre::eyre!("No configuration found in response"))?;

    let tokens = extract_all_tokens_from_config(&configuration);

    if !tokens.is_empty() {
        info!("Found {} unique token(s) across all chains", tokens.len());
    }

    let mut all_token_balances: Vec<TokenBalances> = Vec::new();

    for (symbol, token_info) in tokens {
        let mut chains = Vec::new();

        for chain in &configuration.chains {
            if !chain.tokens.contains_key(&symbol) {
                continue;
            }
            let cb = match select_wallet_for_chain(chain, wallets) {
                Some(w) => query_token_balance_via_client(chain, &symbol, &w.address()).await,
                None => ChainBalanceView::all(
                    chain.network.clone(),
                    chain.tokens.get(&symbol).map_or(0, |t| t.decimals),
                    BalanceValue::NoWallet,
                ),
            };
            chains.push(cb);
        }

        all_token_balances.push(TokenBalances {
            symbol: token_info.symbol,
            chains,
        });
    }

    all_token_balances.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    // Native gas balances per chain
    let mut native_balances: Vec<NativeBalanceView> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for chain in &configuration.chains {
        if seen.contains_key(&chain.network) {
            continue;
        }
        seen.insert(chain.network.clone(), ());

        let decimals = match native_decimals(chain) {
            Ok(d) => Some(d),
            Err(e) => {
                warn!("{e}");
                native_balances.push(NativeBalanceView {
                    network: chain.network.clone(),
                    balance: BalanceValue::Error("bad architecture".to_string()),
                    decimals: None,
                });
                continue;
            }
        };

        let owner_address = match select_wallet_for_chain(chain, wallets) {
            Some(w) => w.address(),
            None => {
                native_balances.push(NativeBalanceView {
                    network: chain.network.clone(),
                    decimals,
                    balance: BalanceValue::NoWallet,
                });
                continue;
            }
        };

        let client = match ChainClient::from_chain_config(chain) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to build client for {}: {}", chain.network, e);
                native_balances.push(NativeBalanceView {
                    network: chain.network.clone(),
                    decimals,
                    balance: BalanceValue::Error(e.to_string()),
                });
                continue;
            }
        };
        let balance = client.native_balance(&owner_address).await.map_or_else(
            |e| {
                warn!("Failed to get native balance on {}: {}", chain.network, e);
                BalanceValue::Error(e.to_string())
            },
            BalanceValue::Raw,
        );
        native_balances.push(NativeBalanceView {
            network: chain.network.clone(),
            balance,
            decimals,
        });
    }

    Ok(Balances {
        tokens: all_token_balances,
        native: native_balances,
    })
}

/// [`fetch_balances`], logged as the balances table. Used by the CLI and REPL.
pub async fn balance_from_config_with_wallets(
    config: GetConfigResponse,
    wallets: &[&Wallet],
) -> Result<()> {
    let balances = fetch_balances(config, wallets).await?;

    if balances.tokens.is_empty() {
        info!("No tokens found in configuration");
        return Ok(());
    }

    let output = display_all_token_balances(&balances);
    info!("{}", output);

    Ok(())
}

/// Read the trader's available trade balance from MidribV3's
/// `tradeBalance(owner, token)` accessor.
/// `chain_id` rather than a `NamedChain`: alloy's registry has no variant for
/// every EVM chain the venue can list (HyperEVM testnet 998 is one), and
/// demanding one made venue custody unqueryable there. `tradeBalance` is a
/// plain `eth_call`, so the metadata is optional — see [`named_chain_for`].
pub async fn call_get_balance_for_address(
    chain_id: u64,
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    depositer_address: Address,
) -> Result<Uint<256, 4>> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;
    let provider = match named_chain_for(chain_id) {
        Some(named) => ProviderBuilder::new()
            .with_chain(named)
            .connect_http(rpc_url)
            .erased(),
        None => ProviderBuilder::new().connect_http(rpc_url).erased(),
    };
    let contract = MidribV3::new(contract_addr, &provider);
    let result = contract
        .tradeBalance(depositer_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

/// Variant of `call_get_locked_balance` that takes an `Address` directly.
///
/// MidribV3 has no on-chain locked balance: under the optimistic shadow
/// ledger, order reservations live off-chain in the TEE (the chain only sees
/// deposits, net settlement, and voucher withdrawals). There is nothing on
/// the contract to query, so the EVM locked balance is always zero.
pub async fn call_get_locked_balance_for_address(
    _rpc_url: &str,
    _token_address: &str,
    _contract_address: &str,
    _depositer_address: Address,
) -> Result<Uint<256, 4>> {
    Ok(Uint::<256, 4>::ZERO)
}

/// Read the native gas balance (wei) for an explicit `address`.
pub async fn call_get_native_balance_for_address(
    rpc_url: &str,
    address: Address,
) -> Result<Uint<256, 4>> {
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let balance = provider.get_balance(address).await?;
    Ok(balance)
}

/// Read the ERC-20 balance held by `holder`, without any signer setup.
pub async fn call_get_erc20_balance_for_address(
    rpc_url: &str,
    token_address: &str,
    holder: Address,
) -> Result<Uint<256, 4>> {
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let contract = IERC20::new(token_addr, &provider);
    let result = contract.balanceOf(holder).call().await?;
    Ok(result)
}

/// Format a u256 raw balance as a decimal string with `decimals` digits
/// after the point (e.g. `1500000` with `decimals = 6` → `"1.500000"`).
pub fn format_balance(value: Uint<256, 4>, decimals: u32) -> String {
    let s = value.to_string();
    if decimals == 0 {
        return s;
    }
    let dec = decimals as usize;
    if s.len() <= dec {
        let padded = format!("{:0>width$}", s, width = dec + 1);
        let (int_part, frac_part) = padded.split_at(padded.len() - dec);
        format!("{}.{}", int_part, frac_part)
    } else {
        let (int_part, frac_part) = s.split_at(s.len() - dec);
        format!("{}.{}", int_part, frac_part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- format_balance_with_decimals ------------------------------------

    #[test]
    fn format_balance_with_decimals_passes_sentinels_through() {
        // "error" and "not deployed" are sentinel strings the RPC layer
        // emits when a balance call fails; they must reach the user
        // verbatim, not get mangled by the formatter.
        assert_eq!(format_balance_with_decimals("error", 6), "error");
        assert_eq!(
            format_balance_with_decimals("not deployed", 18),
            "not deployed"
        );
    }

    #[test]
    fn format_balance_with_decimals_renders_typical_amounts() {
        // 1 USDC (6 decimals).
        assert_eq!(format_balance_with_decimals("1000000", 6), "1.000000");
        // 1.5 USDC.
        assert_eq!(format_balance_with_decimals("1500000", 6), "1.500000");
        // Sub-unit amount must keep the zero-padded fractional part.
        assert_eq!(
            format_balance_with_decimals("1", 18),
            "0.000000000000000001"
        );
        // Zero.
        assert_eq!(format_balance_with_decimals("0", 6), "0.000000");
    }

    #[test]
    fn format_balance_with_decimals_falls_back_on_unparseable_input() {
        // Anything that isn't a u128 (negative numbers, hex, garbage)
        // round-trips unchanged rather than panicking.
        assert_eq!(format_balance_with_decimals("0xdeadbeef", 6), "0xdeadbeef");
        assert_eq!(format_balance_with_decimals("-1", 6), "-1");
        assert_eq!(format_balance_with_decimals("garbage", 6), "garbage");
    }

    // -- format_balance (Uint<256, 4> variant) ---------------------------

    #[test]
    fn format_balance_handles_zero_decimals() {
        // decimals=0 → integer-only output, no decimal point.
        assert_eq!(format_balance(Uint::<256, 4>::from(0u64), 0), "0");
        assert_eq!(format_balance(Uint::<256, 4>::from(12345u64), 0), "12345");
    }

    #[test]
    fn format_balance_pads_subunit_values() {
        // Value shorter than `decimals` must left-pad with zeros after
        // the implicit "0." prefix.
        assert_eq!(
            format_balance(Uint::<256, 4>::from(1u64), 18),
            "0.000000000000000001"
        );
        assert_eq!(
            format_balance(Uint::<256, 4>::from(1500000u64), 6),
            "1.500000"
        );
    }

    #[test]
    fn format_balance_splits_above_unit_values() {
        // Value longer than `decimals` splits at len - decimals.
        assert_eq!(
            format_balance(Uint::<256, 4>::from(123456789u64), 6),
            "123.456789"
        );
    }

    // -- select_wallet_for_chain -----------------------------------------

    fn evm_chain() -> Chain {
        Chain {
            architecture: "EVM".to_string(),
            ..Default::default()
        }
    }

    fn solana_chain() -> Chain {
        Chain {
            architecture: ARCH_SOLANA.to_string(),
            ..Default::default()
        }
    }

    fn evm_wallet() -> Wallet {
        // Anvil test key #0.
        Wallet::from_evm_hex("0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
            .unwrap()
    }

    #[cfg(feature = "solana")]
    fn solana_wallet() -> Wallet {
        use solana_keypair::Keypair;
        let kp = Keypair::new();
        Wallet::from_solana_base58(&bs58::encode(kp.to_bytes()).into_string()).unwrap()
    }

    #[test]
    fn select_wallet_for_chain_routes_evm_to_secp256k1() {
        let w = evm_wallet();
        let chain = evm_chain();
        let wallets: [&Wallet; 1] = [&w];
        let picked = select_wallet_for_chain(&chain, &wallets);
        assert!(picked.is_some());
        assert_eq!(picked.unwrap().curve(), CurveType::Secp256k1);
    }

    #[cfg(feature = "solana")]
    #[test]
    fn select_wallet_for_chain_routes_solana_to_ed25519() {
        let w = solana_wallet();
        let chain = solana_chain();
        let wallets: [&Wallet; 1] = [&w];
        let picked = select_wallet_for_chain(&chain, &wallets);
        assert!(picked.is_some());
        assert_eq!(picked.unwrap().curve(), CurveType::Ed25519);
    }

    #[cfg(feature = "solana")]
    #[test]
    fn select_wallet_for_chain_picks_matching_curve_from_mixed_set() {
        // Caller passes both wallets; the helper must pick by curve,
        // not by list order. Regression guard for the multi-wallet
        // cross-chain order flow.
        let evm = evm_wallet();
        let sol = solana_wallet();
        let sol_addr = sol.address();

        let mixed_a: [&Wallet; 2] = [&evm, &sol];
        let pick = select_wallet_for_chain(&solana_chain(), &mixed_a).unwrap();
        assert_eq!(pick.curve(), CurveType::Ed25519);
        assert_eq!(pick.address(), sol_addr);

        let mixed_b: [&Wallet; 2] = [&sol, &evm];
        let pick = select_wallet_for_chain(&evm_chain(), &mixed_b).unwrap();
        assert_eq!(pick.curve(), CurveType::Secp256k1);
    }

    /// A Tron chain reads with the EVM key, and its GAS row is in sun (6
    /// decimals), not wei: 10 TRX must print as 10, not 0.00000000001.
    #[test]
    fn tron_chain_uses_the_evm_wallet_and_sun_decimals() {
        let w = evm_wallet();
        let wallets: [&Wallet; 1] = [&w];
        let tron = Chain {
            architecture: "Tron".to_string(),
            ..Default::default()
        };
        assert!(select_wallet_for_chain(&tron, &wallets).is_some());
        assert_eq!(native_decimals(&tron).unwrap(), 6);
        assert_eq!(native_decimals(&evm_chain()).unwrap(), 18);
        assert_eq!(
            format_balance_with_decimals("10000000", native_decimals(&tron).unwrap()),
            "10.000000"
        );
    }

    /// An unknown architecture has no native unit: its GAS row says "bad
    /// architecture" rather than a balance scaled by a guessed 18 decimals.
    #[test]
    fn unknown_architecture_has_no_native_unit() {
        let sui = Chain {
            network: "sui".to_string(),
            architecture: "sui".to_string(),
            ..Default::default()
        };
        assert!(native_decimals(&sui).is_err());

        let balances = Balances {
            tokens: vec![TokenBalances {
                symbol: "USDC".to_string(),
                chains: vec![ChainBalanceView::all_err(
                    "sui".to_string(),
                    6,
                    "bad architecture",
                )],
            }],
            native: vec![NativeBalanceView {
                network: "sui".to_string(),
                balance: BalanceValue::Error("bad architecture".to_string()),
                decimals: None,
            }],
        };
        let shown = display_all_token_balances(&balances);
        let gas = shown
            .lines()
            .find(|l| l.contains("GAS"))
            .expect("a GAS row");
        assert!(gas.contains("bad architecture"), "{gas}");
    }

    /// An unknown architecture selects no wallet (and is logged), never an
    /// EVM default.
    #[test]
    fn unknown_architecture_selects_no_wallet() {
        let w = evm_wallet();
        let wallets: [&Wallet; 1] = [&w];
        let sui = Chain {
            architecture: "sui".to_string(),
            ..Default::default()
        };
        assert!(select_wallet_for_chain(&sui, &wallets).is_none());
    }

    #[test]
    fn select_wallet_for_chain_returns_none_when_no_curve_matches() {
        // EVM-only caller, Solana chain → no match (reported as
        // "no wallet" downstream rather than erroring out).
        let w = evm_wallet();
        let wallets: [&Wallet; 1] = [&w];
        assert!(select_wallet_for_chain(&solana_chain(), &wallets).is_none());
    }

    #[test]
    fn balance_value_strings_round_trip_existing_display() {
        // Expected text captured from the pre-refactor display function for
        // the same fixture (string-holding structs, same inputs).
        let balances = Balances {
            tokens: vec![TokenBalances {
                symbol: "USDC".into(),
                chains: vec![
                    ChainBalanceView {
                        network: "base".into(),
                        decimals: 6,
                        wallet: BalanceValue::Raw(1_500_000),
                        available: BalanceValue::Raw(2_000_001),
                        locked: BalanceValue::Raw(0),
                    },
                    ChainBalanceView {
                        network: "solana".into(),
                        decimals: 6,
                        wallet: BalanceValue::NoWallet,
                        available: BalanceValue::NoWallet,
                        locked: BalanceValue::NoWallet,
                    },
                ],
            }],
            native: vec![
                NativeBalanceView {
                    network: "base".into(),
                    decimals: Some(18),
                    balance: BalanceValue::Raw(1_234_567_890_123_456_789),
                },
                NativeBalanceView {
                    network: "solana".into(),
                    decimals: Some(18),
                    balance: BalanceValue::NoWallet,
                },
            ],
        };
        let expected = "
═══════════════════════════════════════════════════════════════════════════
                                 BALANCES
═══════════════════════════════════════════════════════════════════════════

┌─────────────────────────────────────────────────────────────┐
│ Token          Wallet                 Deposited   Locked    │
╞═════════════════════════════════════════════════════════════╡
│ ── base ──                                                  │
│ GAS            1.234567890123456789                         │
│ USDC           1.500000               2.000001    0.000000  │
│                                                             │
│ ── solana ──                                                │
│ GAS            no wallet                                    │
│ USDC           no wallet              no wallet   no wallet │
└─────────────────────────────────────────────────────────────┘
";
        assert_eq!(display_all_token_balances(&balances), expected);
    }

    #[test]
    fn balance_value_from_wire_splits_numbers_from_sentinels() {
        assert_eq!(BalanceValue::from_wire("42".into()), BalanceValue::Raw(42));
        assert_eq!(
            BalanceValue::from_wire("not deployed".into()),
            BalanceValue::Error("not deployed".into())
        );
    }

    #[test]
    fn u256_balance_never_panics_above_u128() {
        assert_eq!(
            u256_balance(Uint::<256, 4>::from(7u8)),
            BalanceValue::Raw(7)
        );
        let max = Uint::<256, 4>::from(u128::MAX);
        assert_eq!(u256_balance(max), BalanceValue::Raw(u128::MAX));
        let over = max + Uint::<256, 4>::from(1u8);
        let v = u256_balance(over);
        assert_eq!(v, BalanceValue::Error(format!("exceeds u128: {over}")),);
        // The table prints the digits, as the stringifying code did.
        assert_eq!(v.display(6), over.to_string());
    }

    #[test]
    fn read_failure_reason_is_data_but_displays_as_error() {
        let reason = "error sending request for url (https://rpc.example/key)";
        let failed = BalanceValue::Error(reason.to_string());
        assert_eq!(failed, BalanceValue::Error(reason.to_string()));
        assert_eq!(failed.display(6), "error");
        // Fixed not-available texts still show as they are.
        assert_eq!(
            BalanceValue::Error("not deployed".into()).display(6),
            "not deployed"
        );
        assert_eq!(BalanceValue::Error("no rpc".into()).display(6), "no rpc");
    }

    #[test]
    fn each_chain_row_scales_by_its_own_token_decimals() {
        let row = |network: &str, decimals: u32| ChainBalanceView {
            network: network.into(),
            decimals,
            wallet: BalanceValue::Raw(1_500_000),
            available: BalanceValue::Raw(2_000_000),
            locked: BalanceValue::Raw(3),
        };
        let balances = Balances {
            tokens: vec![TokenBalances {
                symbol: "USDT".into(),
                chains: vec![row("alpha", 6), row("beta", 18)],
            }],
            native: vec![],
        };
        let shown = display_all_token_balances(&balances);
        let line = |chain: &str| {
            let after = shown.split(&format!("── {chain} ──")).nth(1).unwrap();
            after
                .lines()
                .find(|l| l.contains("USDT"))
                .unwrap()
                .to_string()
        };
        let a = line("alpha");
        assert!(
            a.contains("1.500000") && a.contains("2.000000") && a.contains("0.000003"),
            "{a}"
        );
        let b = line("beta");
        assert!(b.contains("0.000000000001500000"), "{b}");
        assert!(b.contains("0.000000000002000000"), "{b}");
    }
}
