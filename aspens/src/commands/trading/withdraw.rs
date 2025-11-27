use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U160};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

use super::MidribV2;
use crate::commands::config::config_pb::GetConfigResponse;

/// Withdraw tokens using configuration from the server
///
/// This is the recommended way to withdraw tokens. It uses the configuration
/// fetched from the server to determine RPC URLs, contract addresses, and token addresses.
///
/// # Arguments
/// * `network` - The network name (e.g., "anvil-1", "base-sepolia")
/// * `token_symbol` - The token symbol (e.g., "USDC", "WETH")
/// * `amount` - The amount to withdraw (in token's smallest unit)
/// * `privkey` - The private key of the user's wallet
/// * `config` - The configuration response from the server
pub async fn call_withdraw_from_config(
    network: String,
    token_symbol: String,
    amount: u64,
    privkey: String,
    config: GetConfigResponse,
) -> Result<()> {
    // Look up chain info
    let chain = config.get_chain(&network).ok_or_else(|| {
        let available_chains = config
            .config
            .as_ref()
            .map(|c| {
                c.chains
                    .iter()
                    .map(|ch| ch.network.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        eyre::eyre!(
            "Chain '{}' not found in configuration. Available chains: {}",
            network,
            available_chains
        )
    })?;

    // Look up token info
    let token = config.get_token(&network, &token_symbol).ok_or_else(|| {
        let available_tokens = chain
            .tokens
            .keys()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        eyre::eyre!(
            "Token '{}' not found on chain '{}'. Available tokens: {}",
            token_symbol,
            network,
            available_tokens
        )
    })?;

    // Get trade contract address
    let contract_address = chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| {
            eyre::eyre!(
                "Trade contract not found for chain '{}'. Please ensure the contract is deployed.",
                network
            )
        })?
        .address
        .clone();

    // Derive NamedChain from chain_id
    let chain_type = match chain.chain_id {
        1 => NamedChain::Mainnet,
        5 => NamedChain::Goerli,
        11155111 => NamedChain::Sepolia,
        8453 => NamedChain::Base,
        84531 => NamedChain::BaseGoerli,
        84532 => NamedChain::BaseSepolia,
        10 => NamedChain::Optimism,
        420 => NamedChain::OptimismGoerli,
        11155420 => NamedChain::OptimismSepolia,
        _ => {
            tracing::warn!(
                "Unknown chain ID {}, using chain ID directly",
                chain.chain_id
            );
            NamedChain::try_from(chain.chain_id as u64)?
        }
    };

    tracing::info!(
        "Withdrawing {} {} from {} (chain_id: {}, rpc: {})",
        amount,
        token_symbol,
        network,
        chain.chain_id,
        chain.rpc_url
    );

    // Perform the withdrawal
    let withdrawal_amount = U160::from(amount);
    let contract_addr: Address = Address::parse_checksummed(&contract_address, None)?;
    let token_addr: Address = token.address.parse()?;
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&chain.rpc_url)?;

    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain_type)
        .wallet(wallet)
        .connect_http(rpc_url);

    // Get an instance of the contract
    let contract = MidribV2::new(contract_addr, &provider);

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
