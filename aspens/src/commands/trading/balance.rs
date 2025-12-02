use alloy::primitives::{Address, Uint};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use eyre::Result;
use std::collections::HashMap;
use tracing::{info, warn};
use url::Url;

use super::{MidribV2, IERC20};
use crate::commands::config::config_pb::{Configuration, GetConfigResponse};

/// Represents a unique token across all chains
#[derive(Debug, Clone)]
struct TokenInfo {
    symbol: String,
    name: String,
    decimals: u32,
    /// Map of chain_id -> (chain_network, token_address, contract_address)
    chain_locations: HashMap<u32, ChainLocation>,
}

#[derive(Debug, Clone)]
struct ChainLocation {
    network: String,
    token_address: String,
    contract_address: String,
    rpc_url: String,
}

/// Balance information for a token on a specific chain
#[derive(Debug)]
struct ChainBalance {
    chain_network: String,
    wallet_balance: String,
    available_balance: String,
    locked_balance: String,
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

    // Iterate through all chains
    for chain in &config.chains {
        let chain_id = chain.chain_id;
        let chain_network = chain.network.clone();
        let contract_address = chain
            .trade_contract
            .as_ref()
            .map(|tc| tc.address.clone())
            .unwrap_or_default();
        let rpc_url = chain.rpc_url.clone();

        // Iterate through all tokens on this chain
        for (symbol, token) in &chain.tokens {
            tokens
                .entry(symbol.clone())
                .or_insert_with(|| TokenInfo {
                    symbol: symbol.clone(),
                    name: token.name.clone(),
                    decimals: token.decimals,
                    chain_locations: HashMap::new(),
                })
                .chain_locations
                .insert(
                    chain_id,
                    ChainLocation {
                        network: chain_network.clone(),
                        token_address: token.address.clone(),
                        contract_address: contract_address.clone(),
                        rpc_url: rpc_url.clone(),
                    },
                );
        }
    }

    tokens
}

/// Query all balance types for a token on a specific chain
async fn query_token_balance_on_chain(
    chain_id: u32,
    location: &ChainLocation,
    privkey: &str,
) -> ChainBalance {
    let chain_network = location.network.clone();

    // Try to parse chain as NamedChain, fallback to a default if it fails
    let named_chain = NamedChain::try_from(chain_id as u64).unwrap_or(NamedChain::BaseSepolia);

    // Query wallet balance
    let wallet_balance = call_get_erc20_balance(
        named_chain,
        &location.rpc_url,
        &location.token_address,
        privkey,
    )
    .await
    .map_or_else(
        |e| {
            warn!("Failed to get wallet balance on {}: {}", chain_network, e);
            "error".to_string()
        },
        |v| v.to_string(),
    );

    // Check if trade contract is deployed before querying contract balances
    let (available_balance, locked_balance) = if location.contract_address.is_empty() {
        warn!(
            "Trade contract not deployed on chain '{}'. Available and locked balances unavailable.",
            chain_network
        );
        ("not deployed".to_string(), "not deployed".to_string())
    } else {
        // Query available balance
        let available = call_get_balance(
            named_chain,
            &location.rpc_url,
            &location.token_address,
            &location.contract_address,
            privkey,
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

        // Query locked balance
        let locked = call_get_locked_balance(
            &location.rpc_url,
            &location.token_address,
            &location.contract_address,
            privkey,
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
    };

    ChainBalance {
        chain_network,
        wallet_balance,
        available_balance,
        locked_balance,
    }
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

/// Display all token balances in a single table with tokens as rows
fn display_all_token_balances(all_token_balances: &[TokenBalance]) -> String {
    if all_token_balances.is_empty() {
        return String::new();
    }

    // Collect all unique chain networks across all tokens
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

    output.push_str("═══════════════════════════════════════════════════════════\n");
    output.push_str("                      BALANCES\n");
    output.push_str("═══════════════════════════════════════════════════════════\n");

    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);

    // Build header with two rows: chain name on first row, balance type on second row
    let mut header: Vec<String> = vec!["Symbol".to_string(), "Token Name".to_string()];
    for chain in &all_chains {
        header.push(format!("{}\nWallet", chain));
        header.push(format!("{}\nAvailable", chain));
        header.push(format!("{}\nLocked", chain));
    }
    table.set_header(header);

    // Add row for each token
    for token_balance in all_token_balances {
        let mut row: Vec<String> = Vec::new();

        // Token symbol
        row.push(token_balance.token_info.symbol.clone());

        // Token name
        row.push(token_balance.token_info.name.clone());

        // For each chain, add Wallet, Available, and Locked columns
        for chain in &all_chains {
            // Find the balance for this chain
            if let Some(chain_balance) = token_balance
                .chain_balances
                .iter()
                .find(|cb| cb.chain_network == *chain)
            {
                let decimals = token_balance.token_info.decimals;
                row.push(format_balance_with_decimals(
                    &chain_balance.wallet_balance,
                    decimals,
                ));
                row.push(format_balance_with_decimals(
                    &chain_balance.available_balance,
                    decimals,
                ));
                row.push(format_balance_with_decimals(
                    &chain_balance.locked_balance,
                    decimals,
                ));
            } else {
                // Token doesn't exist on this chain
                row.push("-".to_string());
                row.push("-".to_string());
                row.push("-".to_string());
            }
        }

        table.add_row(row);
    }

    output.push_str(&table.to_string());
    output.push('\n');

    output
}

/// New config-driven balance function
pub async fn balance_from_config(config: GetConfigResponse, privkey: String) -> Result<()> {
    let configuration = config
        .config
        .ok_or_else(|| eyre::eyre!("No configuration found in response"))?;

    // Extract all unique tokens from configuration
    let tokens = extract_all_tokens_from_config(&configuration);

    if tokens.is_empty() {
        info!("No tokens found in configuration");
        return Ok(());
    }

    info!("Found {} unique token(s) across all chains", tokens.len());

    // Query balances for all tokens across all chains
    let mut all_token_balances: Vec<TokenBalance> = Vec::new();

    for (_symbol, token_info) in tokens {
        let mut chain_balances = Vec::new();

        // Query balance on each chain where this token exists
        for (chain_id, location) in &token_info.chain_locations {
            let chain_balance = query_token_balance_on_chain(*chain_id, location, &privkey).await;
            chain_balances.push(chain_balance);
        }

        all_token_balances.push(TokenBalance {
            token_info: token_info.clone(),
            chain_balances,
        });
    }

    // Sort tokens by symbol for consistent display
    all_token_balances.sort_by(|a, b| a.token_info.symbol.cmp(&b.token_info.symbol));

    // Display all token balances
    let output = display_all_token_balances(&all_token_balances);
    info!("{}", output);

    Ok(())
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
    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
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
    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
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
