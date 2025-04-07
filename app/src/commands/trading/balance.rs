use alloy::primitives::{Address, Uint};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use anyhow::Result;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use tracing::info;
use url::Url;

use super::{Midrib, IERC20};

pub async fn balance(_args: &[String]) -> Result<()> {
    let error_val = Uint::from(99999);
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
    .unwrap_or(error_val);

    let base_available_balance = call_get_balance(
        NamedChain::BaseGoerli,
        base_chain_rpc_url.as_str(),
        base_chain_usdc_token_address.as_str(),
    )
    .await
    .unwrap_or(error_val);

    let base_locked_balance = call_get_locked_balance(
        NamedChain::BaseGoerli,
        base_chain_rpc_url.as_str(),
        base_chain_usdc_token_address.as_str(),
    )
    .await
    .unwrap_or(error_val);

    let quote_wallet_balance = call_get_erc20_balance(
        NamedChain::BaseSepolia,
        base_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
    )
    .await
    .unwrap_or(error_val);

    let quote_available_balance = call_get_balance(
        NamedChain::BaseSepolia,
        quote_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
    )
    .await
    .unwrap_or(error_val);

    let quote_locked_balance = call_get_locked_balance(
        NamedChain::BaseSepolia,
        quote_chain_rpc_url.as_str(),
        quote_chain_usdc_token_address.as_str(),
    )
    .await
    .unwrap_or(error_val);

    let balance_table = balance_table(
        vec!["USDC", "Base Chain", "Quote Chain"],
        base_wallet_balance,
        base_available_balance,
        base_locked_balance,
        quote_wallet_balance,
        quote_available_balance,
        quote_locked_balance,
    );

    if quote_wallet_balance.eq(&error_val)
        | quote_available_balance.eq(&error_val)
        | quote_locked_balance.eq(&error_val)
        | base_wallet_balance.eq(&error_val)
        | base_available_balance.eq(&error_val)
        | base_locked_balance.eq(&error_val)
    {
        tracing::error!("** A '99999' value represents an error in fetching the actual value");
    }

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
    let provider = ProviderBuilder::new().with_chain(chain).on_http(rpc_url);

    // Get an instance of the contract
    let contract = Midrib::new(contract_addr, &provider);

    // Call the contract function
    let result = contract
        .getBalance(depositer_address, token_addr)
        .call()
        .await?
        ._0;

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
    let provider = ProviderBuilder::new().on_http(rpc_url);

    // Get an instance of the contract
    let contract = Midrib::new(contract_addr, &provider);

    // Call the contract function
    let result = contract
        .getLockedBalance(depositer_address, token_addr)
        .call()
        .await?
        ._0;

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
    let provider = ProviderBuilder::new().with_chain(chain).on_http(rpc_url);

    // Get an instance of the contract
    let contract = IERC20::new(token_addr, &provider);
    let result = contract.balanceOf(depositer_address).call().await?._0;

    Ok(result)
}

pub fn balance_table(
    header: Vec<&str>,
    base_wallet_bal: Uint<256, 4>,
    base_available_bal: Uint<256, 4>,
    base_locked_bal: Uint<256, 4>,
    quote_wallet_bal: Uint<256, 4>,
    quote_available_bal: Uint<256, 4>,
    quote_locked_bal: Uint<256, 4>,
) -> Table {
    let mut table = Table::new();

    table
        .load_preset(UTF8_BORDERS_ONLY)
        .set_header(header)
        .add_row(vec![
            "Wallet Balance",
            &format!("{base_wallet_bal}"),
            &format!("{quote_wallet_bal}"),
        ])
        .add_row(vec![
            "Available Balance",
            &format!("{base_available_bal:?}"),
            &format!("{quote_available_bal:?}"),
        ])
        .add_row(vec![
            "Locked Balance",
            &format!("{base_locked_bal:?}"),
            &format!("{quote_locked_bal:?}"),
        ]);

    table
}
