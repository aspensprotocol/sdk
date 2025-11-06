use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

use super::{MidribV2, IERC20};

pub async fn call_deposit(
    chain: NamedChain,
    rpc_url: String,
    token_address: String,
    contract_address: String,
    privkey: String,
    amount: u64,
) -> Result<()> {
    let allowance_amount = U256::from(amount.saturating_add(1000));
    let deposit_amount = U256::from(amount);
    let contract_address: Address = Address::parse_checksummed(&contract_address, None)?;
    let token_addr: Address = token_address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    tracing::debug!("{:?}", &signer);
    let signer_address = signer.address();
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&rpc_url)?;

    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain)
        .wallet(wallet)
        .connect_http(rpc_url);

    // Get an instance of the contract
    let contract = MidribV2::new(contract_address, &provider);

    let erc20 = IERC20::new(token_addr, &provider);
    // Get the allowance
    let allowance_result = erc20
        .allowance(signer_address, signer_address)
        .call()
        .await?;

    tracing::info!("Get allowance result: {allowance_result:?}");

    // Set the allowance
    let approve_result = erc20
        .approve(contract_address, allowance_amount)
        .send()
        .await?
        .watch()
        .await?;

    tracing::info!("Set allowance result: {approve_result:?}");

    // Call the contract function
    tracing::info!("Attempting deposit of {deposit_amount} tokens to contract {contract_address}");
    
    let deposit_tx = contract.deposit(token_addr, deposit_amount);
    
    // Try to estimate gas first to see if the transaction would succeed
    match deposit_tx.estimate_gas().await {
        Ok(gas_estimate) => {
            tracing::info!("Gas estimate for deposit: {gas_estimate:?}");
        }
        Err(e) => {
            tracing::error!("Failed to estimate gas for deposit: {e:?}");
            return Err(e.into());
        }
    }
    
    let result = deposit_tx
        .send()
        .await?;
    
    tracing::info!("Deposit transaction sent: {result:?}");
    
    let receipt = result
        .with_required_confirmations(1)
        .watch()
        .await?;

    tracing::info!("Deposit transaction hash: {receipt:?}");

    Ok(())
}
