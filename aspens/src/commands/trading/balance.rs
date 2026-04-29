use alloy::primitives::{Address, Uint};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use eyre::Result;
use std::collections::HashMap;
use tracing::{info, warn};
use url::Url;

use super::{MidribV2, IERC20};
use crate::chain_client::{ChainClient, ARCH_SOLANA};
use crate::commands::config::config_pb::{Chain, Configuration, GetConfigResponse};
use crate::wallet::{load_trader_wallet, CurveType, Wallet};

/// Represents a unique token across all chains
#[derive(Debug, Clone)]
struct TokenInfo {
    symbol: String,
    decimals: u32,
}

/// Balance information for a token on a specific chain
#[derive(Debug)]
struct ChainBalance {
    chain_network: String,
    wallet_balance: String,
    available_balance: String,
    locked_balance: String,
}

/// Native gas token balance for a chain
#[derive(Debug)]
struct NativeBalance {
    chain_network: String,
    balance: String,
}

/// Aggregated balance for a single token across all chains
#[derive(Debug)]
struct TokenBalance {
    token_info: TokenInfo,
    chain_balances: Vec<ChainBalance>,
}

/// Extract all unique tokens from configuration chains
fn extract_all_tokens_from_config(config: &Configuration) -> HashMap<String, TokenInfo> {
    let mut tokens: HashMap<String, TokenInfo> = HashMap::new();

    for chain in &config.chains {
        for (symbol, token) in &chain.tokens {
            tokens.entry(symbol.clone()).or_insert_with(|| TokenInfo {
                symbol: symbol.clone(),
                decimals: token.decimals,
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
fn display_all_token_balances(
    all_token_balances: &[TokenBalance],
    native_balances: &[NativeBalance],
) -> String {
    if all_token_balances.is_empty() {
        return String::new();
    }

    // Collect all unique chain networks across all tokens and sort them
    let mut all_chains: Vec<String> = Vec::new();
    for token_balance in all_token_balances {
        for chain_balance in &token_balance.chain_balances {
            if !all_chains.contains(&chain_balance.chain_network) {
                all_chains.push(chain_balance.chain_network.clone());
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
    table.load_preset(UTF8_BORDERS_ONLY);

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
        if let Some(native) = native_balances.iter().find(|nb| nb.chain_network == *chain) {
            let gas_balance = format_balance_with_decimals(&native.balance, 18);
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
            if let Some(chain_balance) = token_balance
                .chain_balances
                .iter()
                .find(|cb| cb.chain_network == *chain)
            {
                let decimals = token_balance.token_info.decimals;
                let symbol = &token_balance.token_info.symbol;

                let wallet = format_balance_with_decimals(&chain_balance.wallet_balance, decimals);
                let deposited =
                    format_balance_with_decimals(&chain_balance.available_balance, decimals);
                let locked = format_balance_with_decimals(&chain_balance.locked_balance, decimals);

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
/// Used by `balance_from_config_with_wallet`. For Solana chains, locked/deposited
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
    match crate::solana::client::fetch_user_balance(
        &chain.rpc_url,
        &instance,
        &user,
        &mint,
        &program_id,
    )
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
            ("error".to_string(), "error".to_string())
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
) -> ChainBalance {
    let chain_network = chain.network.clone();
    let token = match chain.tokens.get(token_symbol) {
        Some(t) => t,
        None => {
            return ChainBalance {
                chain_network,
                wallet_balance: "missing token".to_string(),
                available_balance: "missing token".to_string(),
                locked_balance: "missing token".to_string(),
            };
        }
    };

    let client = match ChainClient::from_chain_config(chain) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build client for {}: {}", chain_network, e);
            return ChainBalance {
                chain_network,
                wallet_balance: "error".to_string(),
                available_balance: "error".to_string(),
                locked_balance: "error".to_string(),
            };
        }
    };

    let wallet_balance = client
        .token_balance(token, owner_address)
        .await
        .map_or_else(
            |e| {
                warn!("Failed to get wallet balance on {}: {}", chain_network, e);
                "error".to_string()
            },
            |v| v.to_string(),
        );

    // Locked/deposited balances come from the trade contract.
    // Solana side is not yet wired up; EVM uses MidribV2.
    let (available_balance, locked_balance) =
        if chain.architecture.eq_ignore_ascii_case(ARCH_SOLANA) {
            solana_user_balance(chain, token, owner_address).await
        } else {
            let contract_address = chain
                .trade_contract
                .as_ref()
                .map(|tc| tc.address.clone())
                .unwrap_or_default();

            if contract_address.is_empty() {
                ("not deployed".to_string(), "not deployed".to_string())
            } else {
                let named_chain =
                    NamedChain::try_from(chain.chain_id as u64).unwrap_or(NamedChain::BaseSepolia);
                // Reuse existing EVM helpers — they need a privkey to derive the address,
                // but we already have the address. Use *_for_address variants.
                let owner: Address = match owner_address.parse() {
                    Ok(a) => a,
                    Err(_) => {
                        return ChainBalance {
                            chain_network,
                            wallet_balance,
                            available_balance: "bad address".to_string(),
                            locked_balance: "bad address".to_string(),
                        };
                    }
                };
                let available = call_get_balance_for_address(
                    named_chain,
                    &chain.rpc_url,
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
                        "error".to_string()
                    },
                    |v| v.to_string(),
                );
                let locked = call_get_locked_balance_for_address(
                    &chain.rpc_url,
                    &token.address,
                    &contract_address,
                    owner,
                )
                .await
                .map_or_else(
                    |e| {
                        warn!("Failed to get locked balance on {}: {}", chain_network, e);
                        "error".to_string()
                    },
                    |v| v.to_string(),
                );
                (available, locked)
            }
        };

    ChainBalance {
        chain_network,
        wallet_balance,
        available_balance,
        locked_balance,
    }
}

/// Curve-agnostic config-driven balance function.
///
/// Dispatches per-chain based on `chain.architecture`:
/// - EVM chains query via Alloy + MidribV2
/// - Solana chains query via solana-client (SOL + SPL); deposited/locked
///   balances are scaffolded as "not deployed" until the on-chain program lands
pub async fn balance_from_config_with_wallet(
    config: GetConfigResponse,
    wallet: &Wallet,
) -> Result<()> {
    balance_from_config_with_wallets(config, &[wallet]).await
}

/// Pick the first wallet matching `chain`'s curve. Returns `None` when no
/// caller-supplied wallet matches — the chain's rows are reported as
/// "no wallet" rather than erroring out.
fn select_wallet_for_chain<'a>(chain: &Chain, wallets: &'a [&'a Wallet]) -> Option<&'a Wallet> {
    let wanted = crate::wallet::chain_curve(chain);
    wallets.iter().copied().find(|w| w.curve() == wanted)
}

/// Curve-aware, multi-wallet config-driven balance function.
///
/// Each chain in the config gets matched to a wallet with the same curve
/// (EVM chains → `Wallet::Evm`, Solana chains → `Wallet::Solana`). A chain
/// with no matching wallet is reported as "no wallet" rather than failing
/// the whole call.
pub async fn balance_from_config_with_wallets(
    config: GetConfigResponse,
    wallets: &[&Wallet],
) -> Result<()> {
    let configuration = config
        .config
        .ok_or_else(|| eyre::eyre!("No configuration found in response"))?;

    let tokens = extract_all_tokens_from_config(&configuration);

    if tokens.is_empty() {
        info!("No tokens found in configuration");
        return Ok(());
    }

    info!("Found {} unique token(s) across all chains", tokens.len());

    let mut all_token_balances: Vec<TokenBalance> = Vec::new();

    for (symbol, token_info) in tokens {
        let mut chain_balances = Vec::new();

        for chain in &configuration.chains {
            if !chain.tokens.contains_key(&symbol) {
                continue;
            }
            let cb = match select_wallet_for_chain(chain, wallets) {
                Some(w) => query_token_balance_via_client(chain, &symbol, &w.address()).await,
                None => ChainBalance {
                    chain_network: chain.network.clone(),
                    wallet_balance: "no wallet".to_string(),
                    available_balance: "no wallet".to_string(),
                    locked_balance: "no wallet".to_string(),
                },
            };
            chain_balances.push(cb);
        }

        all_token_balances.push(TokenBalance {
            token_info: token_info.clone(),
            chain_balances,
        });
    }

    all_token_balances.sort_by(|a, b| a.token_info.symbol.cmp(&b.token_info.symbol));

    // Native gas balances per chain
    let mut native_balances: Vec<NativeBalance> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for chain in &configuration.chains {
        if seen.contains_key(&chain.network) {
            continue;
        }
        seen.insert(chain.network.clone(), ());

        let owner_address = match select_wallet_for_chain(chain, wallets) {
            Some(w) => w.address(),
            None => {
                native_balances.push(NativeBalance {
                    chain_network: chain.network.clone(),
                    balance: "no wallet".to_string(),
                });
                continue;
            }
        };

        let client = match ChainClient::from_chain_config(chain) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to build client for {}: {}", chain.network, e);
                native_balances.push(NativeBalance {
                    chain_network: chain.network.clone(),
                    balance: "error".to_string(),
                });
                continue;
            }
        };
        let balance = client.native_balance(&owner_address).await.map_or_else(
            |e| {
                warn!("Failed to get native balance on {}: {}", chain.network, e);
                "error".to_string()
            },
            |v| v.to_string(),
        );
        native_balances.push(NativeBalance {
            chain_network: chain.network.clone(),
            balance,
        });
    }

    let output = display_all_token_balances(&all_token_balances, &native_balances);
    info!("{}", output);

    Ok(())
}

/// New config-driven balance function
/// Legacy EVM-privkey entry point. Kept for existing CLI/REPL call sites.
///
/// Builds an EVM wallet from `privkey` and opportunistically also loads a
/// Solana trader wallet from `TRADER_PRIVKEY_SOLANA` (when present and the
/// `solana` feature is on), then delegates to
/// [`balance_from_config_with_wallets`]. Chains whose architecture has no
/// matching wallet are reported as "no wallet" in the table.
pub async fn balance_from_config(config: GetConfigResponse, privkey: String) -> Result<()> {
    let evm = Wallet::from_evm_hex(&privkey)?;
    let solana = load_trader_wallet(CurveType::Ed25519).ok();
    let mut wallets: Vec<&Wallet> = vec![&evm];
    if let Some(ref s) = solana {
        wallets.push(s);
    }
    balance_from_config_with_wallets(config, &wallets).await
}

pub async fn balance(
    base_chain_rpc_url: String,
    base_chain_usdc_token_address: String,
    quote_chain_rpc_url: String,
    quote_chain_usdc_token_address: String,
    base_chain_contract_address: String,
    quote_chain_contract_address: String,
    privkey: String,
) -> Result<()> {
    let base_wallet_balance = call_get_erc20_balance(
        NamedChain::BaseGoerli,
        &base_chain_rpc_url,
        &base_chain_usdc_token_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let base_available_balance = call_get_balance(
        NamedChain::BaseGoerli,
        &base_chain_rpc_url,
        &base_chain_usdc_token_address,
        &base_chain_contract_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let base_locked_balance = call_get_locked_balance(
        &base_chain_rpc_url,
        &base_chain_usdc_token_address,
        &base_chain_contract_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let quote_wallet_balance = call_get_erc20_balance(
        NamedChain::BaseSepolia,
        &quote_chain_rpc_url,
        &quote_chain_usdc_token_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let quote_available_balance = call_get_balance(
        NamedChain::BaseSepolia,
        &quote_chain_rpc_url,
        &quote_chain_usdc_token_address,
        &quote_chain_contract_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let quote_locked_balance = call_get_locked_balance(
        &quote_chain_rpc_url,
        &quote_chain_usdc_token_address,
        &quote_chain_contract_address,
        &privkey,
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());
    let balance_table = balance_table(
        vec!["USDC", "Base Chain", "Quote Chain"],
        &base_wallet_balance,
        &base_available_balance,
        &base_locked_balance,
        &quote_wallet_balance,
        &quote_available_balance,
        &quote_locked_balance,
    );
    info!("\n{}", balance_table);
    Ok(())
}

pub async fn call_get_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    privkey: &str,
) -> Result<Uint<256, 4>> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .connect_http(rpc_url);
    let contract = MidribV2::new(contract_addr, &provider);
    let result = contract
        .tradeBalance(depositer_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

pub async fn call_get_locked_balance(
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    privkey: &str,
) -> Result<Uint<256, 4>> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let contract = MidribV2::new(contract_addr, &provider);
    let result = contract
        .lockedTradeBalance(depositer_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

/// Variant of `call_get_balance` that takes an `Address` directly instead of
/// deriving it from a private key. Used by curve-aware balance queries.
pub async fn call_get_balance_for_address(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    depositer_address: Address,
) -> Result<Uint<256, 4>> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .connect_http(rpc_url);
    let contract = MidribV2::new(contract_addr, &provider);
    let result = contract
        .tradeBalance(depositer_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

/// Variant of `call_get_locked_balance` that takes an `Address` directly.
pub async fn call_get_locked_balance_for_address(
    rpc_url: &str,
    token_address: &str,
    contract_address: &str,
    depositer_address: Address,
) -> Result<Uint<256, 4>> {
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token_address.parse()?;
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let contract = MidribV2::new(contract_addr, &provider);
    let result = contract
        .lockedTradeBalance(depositer_address, token_addr)
        .call()
        .await?;
    Ok(result)
}

pub async fn call_get_erc20_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    privkey: &str,
) -> Result<Uint<256, 4>> {
    let token_addr: Address = token_address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .connect_http(rpc_url);
    let contract = IERC20::new(token_addr, &provider);
    let result = contract.balanceOf(depositer_address).call().await?;
    Ok(result)
}

pub async fn call_get_native_balance(rpc_url: &str, privkey: &str) -> Result<Uint<256, 4>> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let address = signer.address();
    call_get_native_balance_for_address(rpc_url, address).await
}

pub async fn call_get_native_balance_for_address(
    rpc_url: &str,
    address: Address,
) -> Result<Uint<256, 4>> {
    let rpc_url = Url::parse(rpc_url)?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let balance = provider.get_balance(address).await?;
    Ok(balance)
}

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

pub fn balance_table(
    header: Vec<&str>,
    base_wallet_bal: &str,
    base_available_bal: &str,
    base_locked_bal: &str,
    quote_wallet_bal: &str,
    quote_available_bal: &str,
    quote_locked_bal: &str,
) -> Table {
    let mut table = Table::new();

    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_header(header)
        .add_row(vec!["Wallet Balance", base_wallet_bal, quote_wallet_bal])
        .add_row(vec![
            "Available Balance",
            base_available_bal,
            quote_available_bal,
        ])
        .add_row(vec!["Locked Balance", base_locked_bal, quote_locked_bal]);

    table
}
