use alloy::primitives::{Address, Uint};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use anyhow::Result;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use tracing::info;
use url::Url;

use super::{MidribV2, IERC20};

pub async fn balance(_args: &[String]) -> Result<()> {
    let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL")?;
    let base_chain_usdc_token_address = std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS")?;
    let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL")?;
    let quote_chain_usdc_token_address = std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS")?;

    let base_wallet_balance = call_get_erc20_balance(
        NamedChain::BaseGoerli,
        base_chain_rpc_url.as_str(),
        base_chain_usdc_token_address.as_str(),
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());

    let base_available_balance = call_get_balance(
        NamedChain::BaseGoerli,
        base_chain_rpc_url.as_str(),
        base_chain_usdc_token_address.as_str(),
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());

    let base_locked_balance = call_get_locked_balance(
        NamedChain::BaseGoerli,
        base_chain_rpc_url.as_str(),
        base_chain_usdc_token_address.as_str(),
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());

    let quote_wallet_balance = call_get_erc20_balance(
        NamedChain::BaseSepolia,
        base_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());

    let quote_available_balance = call_get_balance(
        NamedChain::BaseSepolia,
        quote_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
    )
    .await
    .map_or("error".to_string(), |v| v.to_string());

    let quote_locked_balance = call_get_locked_balance(
        NamedChain::BaseSepolia,
        quote_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
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
) -> Result<Uint<256, 4>> {
    let base_chain_contract_address = std::env::var("BASE_CHAIN_CONTRACT_ADDRESS")?;
    let quote_chain_contract_address = std::env::var("QUOTE_CHAIN_CONTRACT_ADDRESS")?;
    let contract_address = match chain {
        NamedChain::BaseGoerli => base_chain_contract_address,
        NamedChain::BaseSepolia => quote_chain_contract_address,
        _ => unreachable!(),
    };

    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .connect_http(rpc_url);

    // Get an instance of the contract
    let contract = MidribV2::new(contract_addr, &provider);

    // Call the contract function
    let result = contract
        .getBalance(depositer_address, token_addr)
        .call()
        .await?;

    Ok(result)
}

pub async fn call_get_locked_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
) -> Result<Uint<256, 4>> {
    let base_chain_contract_address = std::env::var("BASE_CHAIN_CONTRACT_ADDRESS")?;
    let quote_chain_contract_address = std::env::var("QUOTE_CHAIN_CONTRACT_ADDRESS")?;
    let contract_address = match chain {
        NamedChain::BaseGoerli => base_chain_contract_address,
        NamedChain::BaseSepolia => quote_chain_contract_address,
        _ => unreachable!(),
    };

    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    // Get an instance of the contract
    let contract = MidribV2::new(contract_addr, &provider);

    // Call the contract function
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
) -> Result<Uint<256, 4>> {
    let token_addr: Address = token_address.parse()?;
    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .connect_http(rpc_url);

    // Get an instance of the contract
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
        .add_row(vec![
            "Wallet Balance",
            base_wallet_bal,
            quote_wallet_bal,
        ])
        .add_row(vec![
            "Available Balance",
            base_available_bal,
            quote_available_bal,
        ])
        .add_row(vec![
            "Locked Balance",
            base_locked_bal,
            quote_locked_bal,
        ]);

    table
}
