use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

use super::MidribV2;

pub async fn call_withdraw(
    chain: NamedChain,
    rpc_url: String,
    token_address: String,
    contract_address: String,
    privkey: String,
    amount: u64,
) -> Result<()> {
    let withdrawal_amount = U256::from(amount);
    let contract_address: Address = Address::parse_checksummed(&contract_address, None)?;
    let token_addr: Address = token_address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&rpc_url)?;

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
