use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use anyhow::Result;
use url::Url;

use super::MidribV2;

pub async fn call_withdraw(
    chain: NamedChain,
    rpc_url: &str,
    token_address: &str,
    amount: u64,
) -> Result<()> {
    let withdrawal_amount = U256::from(amount);
    let base_chain_contract_address = std::env::var("BASE_CHAIN_CONTRACT_ADDRESS")?;
    let quote_chain_contract_address = std::env::var("QUOTE_CHAIN_CONTRACT_ADDRESS")?;
    let contract_address = match chain {
        NamedChain::BaseGoerli => base_chain_contract_address,
        NamedChain::BaseSepolia => quote_chain_contract_address,
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
        .wallet(wallet)
        .connect_http(rpc_url);

    // Get an instance of the contract
    let contract = MidribV2::new(contract_address, &provider);

    // Call the contract function
    let result = contract
        .withdraw(token_addr, withdrawal_amount)
        .send()
        .await?
        .with_required_confirmations(1)
        .watch()
        .await?;

    tracing::info!("Withdraw result: {result:?}");

    Ok(())
}
