use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U160, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

use super::{MidribV2, IERC20};
use crate::chain_client::ARCH_SOLANA;
use crate::commands::config::config_pb::GetConfigResponse;
use crate::wallet::{CurveType, Wallet};

/// Minimum gas balance required for transactions (0.0001 ETH = 100000 gwei)
const MIN_GAS_BALANCE: u128 = 100_000_000_000_000; // 0.0001 ETH in wei

/// Deposit tokens using configuration from the server (legacy EVM API).
///
/// Wraps `call_deposit_from_config_with_wallet` for backward compatibility.
pub async fn call_deposit_from_config(
    network: String,
    token_symbol: String,
    amount: u64,
    privkey: String,
    config: GetConfigResponse,
) -> Result<()> {
    let wallet = Wallet::from_evm_hex(&privkey)?;
    call_deposit_from_config_with_wallet(network, token_symbol, amount, &wallet, config).await
}

/// Deposit tokens using a curve-agnostic wallet.
///
/// Branches on `chain.architecture`:
/// - **EVM**: existing MidribV2 deposit flow
/// - **Solana**: scaffolded — returns a clear error until the on-chain
///   trade program is finalized and its instruction layout is known
pub async fn call_deposit_from_config_with_wallet(
    network: String,
    token_symbol: String,
    amount: u64,
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<()> {
    // Look up chain to determine the dispatch path
    let chain_for_arch = config
        .get_chain(&network)
        .ok_or_else(|| eyre::eyre!("Chain '{}' not found in configuration", network))?;

    if chain_for_arch
        .architecture
        .eq_ignore_ascii_case(ARCH_SOLANA)
    {
        return solana_deposit_scaffold(chain_for_arch, &token_symbol, amount, wallet);
    }

    // EVM path requires an EVM wallet
    if wallet.curve() != CurveType::Secp256k1 {
        return Err(eyre::eyre!(
            "EVM chain '{}' requires a secp256k1 wallet, got Ed25519",
            network
        ));
    }
    let signer = wallet
        .as_evm()
        .ok_or_else(|| eyre::eyre!("expected EVM wallet for chain '{}'", network))?
        .clone();

    call_deposit_from_config_evm(network, token_symbol, amount, signer, config).await
}

/// Solana deposit scaffold — structure is in place but instruction encoding
/// is pending the on-chain trade program design.
fn solana_deposit_scaffold(
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u64,
    wallet: &Wallet,
) -> Result<()> {
    let program_id = chain
        .trade_contract
        .as_ref()
        .and_then(|tc| tc.contract_id.clone())
        .ok_or_else(|| {
            eyre::eyre!(
                "Solana chain '{}' has no trade_contract.contract_id (program ID) configured",
                chain.network
            )
        })?;
    let _token = chain.tokens.get(token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on Solana chain '{}'",
            token_symbol,
            chain.network
        )
    })?;
    let _owner = wallet.address();
    let _amount = amount;
    let _program_id = program_id;

    // TODO: Build and submit Solana deposit instruction once the on-chain
    // trade program's instruction layout is finalized. Structure:
    //   1. Resolve owner ATA for `_token.address` (SPL mint)
    //   2. Build deposit instruction targeting `_program_id` with
    //      accounts [owner, owner_ata, program_pda, mint, token_program, ...]
    //      and data [discriminator, _amount as u64 LE]
    //   3. Wrap in a Transaction, recent blockhash, sign with `wallet.as_solana()`
    //   4. Submit via `ChainClient::Solana`
    Err(eyre::eyre!(
        "Solana deposit is not yet implemented — the on-chain trade program \
         instruction layout is pending. Configure trade_contract.contract_id \
         in the chain config to point at the eventual program."
    ))
}

/// Original EVM deposit logic — kept private and called from the wallet-aware
/// dispatcher above.
async fn call_deposit_from_config_evm(
    network: String,
    token_symbol: String,
    amount: u64,
    signer: PrivateKeySigner,
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
        "Depositing {} {} on {} (chain_id: {}, rpc: {})",
        amount,
        token_symbol,
        network,
        chain.chain_id,
        chain.rpc_url
    );

    // Perform the deposit
    let allowance_amount = U256::from(amount.saturating_add(1000));
    let deposit_amount = U160::from(amount);
    let contract_addr: Address = Address::parse_checksummed(&contract_address, None)?;
    let token_addr: Address = token.address.parse()?;
    let signer_address = signer.address();
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&chain.rpc_url)?;

    // Set up the provider
    let provider = ProviderBuilder::new()
        .with_chain(chain_type)
        .wallet(wallet)
        .connect_http(rpc_url);

    // Check gas balance before attempting any transactions
    let gas_balance = provider.get_balance(signer_address).await?;
    tracing::info!("Gas balance: {} wei", gas_balance);

    if gas_balance < U256::from(MIN_GAS_BALANCE) {
        let balance_eth = gas_balance.to::<u128>() as f64 / 1e18;
        return Err(eyre::eyre!(
            "insufficient gas: wallet has {:.6} native tokens, need at least 0.0001 for gas. \
            Fund your wallet ({}) with native tokens on {} to pay for transaction fees.",
            balance_eth,
            signer_address,
            network
        ));
    }

    // Get an instance of the contract
    let contract = MidribV2::new(contract_addr, &provider);

    let erc20 = IERC20::new(token_addr, &provider);
    // Get the allowance
    let allowance_result = erc20
        .allowance(signer_address, contract_addr)
        .call()
        .await?;

    tracing::info!("Get allowance result: {allowance_result:?}");

    // Only set allowance if current allowance is insufficient
    if allowance_result < allowance_amount {
        tracing::info!(
            "Current allowance insufficient, approving {} tokens",
            allowance_amount
        );
        let approve_result = erc20
            .approve(contract_addr, allowance_amount)
            .send()
            .await?
            .watch()
            .await?;
        tracing::info!("Set allowance result: {approve_result:?}");
    } else {
        tracing::info!("Sufficient allowance already set: {}", allowance_result);
    }

    // Call the contract function
    tracing::info!("Attempting deposit of {deposit_amount} tokens to contract {contract_addr}");

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

    let result = deposit_tx.send().await?;

    tracing::info!("Deposit transaction sent: {result:?}");

    let receipt = result.with_required_confirmations(1).watch().await?;

    tracing::info!("Deposit transaction hash: {receipt:?}");

    Ok(())
}
