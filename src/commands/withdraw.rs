use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy_chains::NamedChain;
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use url::Url;

use super::Midrib;

pub(crate) async fn call_withdraw(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    amount: u64,
) -> Result<()> {
    let withdrawal_amount = U256::from(amount);

    let contract_address = match chain {
        NamedChain::BaseSepolia => super::BASE_SEPOLIA_CONTRACT_ADDRESS,
        NamedChain::OptimismSepolia => super::OP_SEPOLIA_CONTRACT_ADDRESS,
        _ => unreachable!(),
    };
    let contract_address: Address = Address::parse_checksummed(contract_address, None)?;
    let token_addr: Address = token_address.parse()?;

    let signer = std::env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
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

    // Call the contract function
    let result = contract
        .withdraw(token_addr, withdrawal_amount)
        .send()
        .await?
        .with_required_confirmations(1)
        .watch()
        .await?;

    println!("Withdraw result: {result:?}");

    Ok(())
}
