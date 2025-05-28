use alloy::primitives::{Address, Uint};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use anyhow::Result;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use tracing::info;
use url::Url;

use super::{MidribV2, IERC20};

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
        .getBalance(depositer_address, token_addr)
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
        .getLockedBalance(depositer_address, token_addr)
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
