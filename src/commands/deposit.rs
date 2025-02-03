use std::env;

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy_chains::NamedChain;
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use url::Url;

use super::{Midrib, IERC20};

pub(crate) async fn call_deposit(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    amount: u64,
) -> Result<()> {
    let allowance_amount = U256::from(amount.saturating_add(1000));
    let deposit_amount = U256::from(amount);

    let contract_address = match chain {
        NamedChain::BaseSepolia => super::BASE_SEPOLIA_CONTRACT_ADDRESS,
        NamedChain::OptimismSepolia => super::OP_SEPOLIA_CONTRACT_ADDRESS,
        _ => unreachable!(),
    };
    let contract_address: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    dbg!(&signer);
    let signer_address = signer.address();
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(rpc_url)?;

    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url);

    // Get an instance of the contract
    let contract = Midrib::new(contract_address, &provider);

    let erc20 = IERC20::new(token_addr, &provider);
    // Get the allowance
    let allowance_result = erc20
        .allowance(signer_address, signer_address)
        .call()
        .await?
        ._0;

    println!("Get allowance result: {allowance_result:?}");

    // Set the allowance
    let approve_result = erc20
        .approve(contract_address, allowance_amount)
        .send()
        .await?
        .watch()
        .await?;

    println!("Set allowance result: {approve_result:?}");

    // Call the contract function
    let result = contract
        .deposit(token_addr, deposit_amount)
        .send()
        .await?
        .with_required_confirmations(1)
        .watch()
        .await?;

    println!("Deposit result: {result:?}");

    Ok(())
}
