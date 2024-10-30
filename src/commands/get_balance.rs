use alloy::network::EthereumWallet;
use alloy::primitives::{Address, Uint};
use alloy::providers::ProviderBuilder;
use alloy_chains::NamedChain;
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use comfy_table::{presets::UTF8_BORDERS_ONLY, Table};
use url::Url;

use super::{IERC20, Midrib};

pub(crate) async fn call_get_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
) -> Result<Uint<256, 4>> {
    let contract_address = match chain {
        NamedChain::BaseSepolia => super::BASE_SEPOLIA_CONTRACT_ADDRESS,
        NamedChain::OptimismSepolia => super::OP_SEPOLIA_CONTRACT_ADDRESS,
        _ => unreachable!(),
    };

    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let wallet = EthereumWallet::new(signer);

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url);

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

pub(crate) async fn call_get_locked_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
) -> Result<Uint<256, 4>> {
    let contract_address = match chain {
        NamedChain::BaseSepolia => super::BASE_SEPOLIA_CONTRACT_ADDRESS,
        NamedChain::OptimismSepolia => super::OP_SEPOLIA_CONTRACT_ADDRESS,
        _ => unreachable!(),
    };
    let contract_addr: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let wallet = EthereumWallet::new(signer);

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url);

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



pub(crate) async fn call_get_erc20_balance(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
) -> Result<Uint<256, 4>> {
    let token_addr: Address = token_address.parse()?;
    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let depositer_address: Address = signer.address();

    let wallet = EthereumWallet::new(signer);

    let rpc_url = Url::parse(rpc_url)?;
    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url);

    // Get an instance of the contract
    let contract = IERC20::new(token_addr, &provider);
    let result = contract.balanceOf(depositer_address).call().await?._0;

    Ok(result)
}

pub(crate) fn get_balance_table(
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
        .set_header(vec!["USDC", "Base Chain", "Quote Chain"])
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
