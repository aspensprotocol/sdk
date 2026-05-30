use std::str::FromStr;

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy_chains::NamedChain;
use eyre::Result;
use url::Url;

use crate::chain_client::ARCH_SOLANA;
use crate::commands::config::config_pb::GetConfigResponse;
use crate::evm::rpc::MidribV3;
use crate::grpc::create_channel;
use crate::wallet::{CurveType, Wallet};

/// Generated protobuf bindings for the `arborter.v1` trading service.
#[allow(missing_docs)]
pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::WithdrawRequest;

/// Minimum gas balance required for transactions (0.0001 ETH = 100000 gwei)
const MIN_GAS_BALANCE: u128 = 100_000_000_000_000; // 0.0001 ETH in wei

/// Withdraw tokens using a curve-agnostic wallet.
///
/// Branches on `chain.architecture`:
/// - **EVM**: requests a TEE-signed withdrawal voucher from the arborter
///   (`url`) over gRPC, then submits `MidribV3.withdraw(voucher, signature)`
///   on-chain (the wallet pays gas). The permissionless on-chain `withdraw`
///   was removed (Track A §8); the voucher is the authorization.
/// - **Solana**: builds + submits the user-signed Midrib `withdraw` instruction
///   directly (the Solana program is unchanged; no voucher path yet).
pub async fn call_withdraw_from_config_with_wallet(
    url: String,
    network: String,
    token_symbol: String,
    amount: u64,
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<()> {
    let chain_for_arch = config
        .get_chain(&network)
        .ok_or_else(|| eyre::eyre!("Chain '{}' not found in configuration", network))?;

    if chain_for_arch
        .architecture
        .eq_ignore_ascii_case(ARCH_SOLANA)
    {
        return solana_withdraw(url, chain_for_arch, &token_symbol, amount, wallet).await;
    }

    if wallet.curve() != CurveType::Secp256k1 {
        return Err(eyre::eyre!(
            "EVM chain '{}' requires a secp256k1 wallet, got {:?}",
            network,
            wallet.curve()
        ));
    }
    let signer = wallet
        .as_evm()
        .ok_or_else(|| eyre::eyre!("expected EVM wallet for chain '{}'", network))?
        .clone();

    call_withdraw_from_config_evm(url, network, token_symbol, amount, signer, config).await
}

/// Solana withdraw — builds and submits the user-signed Midrib `withdraw`
/// instruction. Requires the `solana` feature.
#[cfg(feature = "solana")]
async fn solana_withdraw(
    url: String,
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u64,
    wallet: &Wallet,
) -> Result<()> {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    let token = chain.tokens.get(token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on Solana chain '{}'",
            token_symbol,
            chain.network
        )
    })?;
    let keypair = wallet.as_solana().ok_or_else(|| {
        eyre::eyre!(
            "Solana chain '{}' requires an Ed25519 wallet (TRADER_PRIVKEY_SOLANA)",
            chain.network
        )
    })?;

    let (program_id, instance) = crate::solana::client::resolve_program_and_instance(chain)?;
    let user = solana_sdk::signer::Signer::pubkey(keypair);
    let mint = Pubkey::from_str(&token.address)
        .map_err(|e| eyre::eyre!("invalid Solana mint '{}': {}", token.address, e))?;
    let user_ata = crate::solana::derive_associated_token_account(&user, &mint);

    // 1) Authenticate the request: Ed25519-sign the canonical bytes the arborter
    //    rebuilds (`network|token|account|amount`).
    let account_str = user.to_string();
    let canonical = format!(
        "{}|{}|{}|{}",
        chain.network, token.address, account_str, amount
    );
    let req_sig = wallet.sign_message(canonical.as_bytes()).await?;

    // 2) Request the TEE-signed voucher.
    let channel = create_channel(&url).await?;
    let mut client = ArborterServiceClient::new(channel);
    let voucher = client
        .withdraw(tonic::Request::new(WithdrawRequest {
            network: chain.network.clone(),
            token: token.address.clone(),
            account: account_str,
            amount: amount.to_string(),
            signature: req_sig,
        }))
        .await?
        .into_inner();
    tracing::info!(
        "Received Solana withdrawal voucher (nonce={}, deadline_slot={})",
        voucher.nonce,
        voucher.expiry
    );

    // 3) Fetch the instance signer pubkey (the Ed25519 key that signed the
    //    voucher) so the program's Ed25519 verify recovers to it.
    let signer_resp =
        crate::commands::config::get_signer_public_key(url.clone(), Some(chain.network.clone()))
            .await?;
    let signer_str = signer_resp
        .chain_keys
        .get(&chain.network)
        .map(|k| k.public_key.clone())
        .ok_or_else(|| eyre::eyre!("no signer public key for chain '{}'", chain.network))?;
    let signer_pk = Pubkey::from_str(&signer_str)
        .map_err(|e| eyre::eyre!("invalid signer pubkey '{}': {}", signer_str, e))?;

    // 4) Rebuild the exact signed payload + the voucher signature.
    let deadline = voucher.expiry; // Solana on-chain deadline is a slot
    let nonce = voucher.nonce;
    let voucher_amount: u64 = voucher
        .amount
        .parse()
        .map_err(|_| eyre::eyre!("invalid voucher amount '{}'", voucher.amount))?;
    let voucher_sig: [u8; 64] = voucher
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| eyre::eyre!("voucher signature must be 64 bytes (Ed25519)"))?;
    let msg = crate::solana::withdrawal_voucher_signing_message(
        &instance,
        &user,
        &mint,
        voucher_amount,
        nonce,
        deadline,
    )?;

    // 5) Submit [Ed25519 verify ix, withdraw_voucher ix] — the user pays + signs.
    let verify_ix = crate::solana::ed25519_verify_ix(&signer_pk.to_bytes(), &voucher_sig, &msg);
    let args = crate::solana::WithdrawVoucherArgs {
        amount: voucher_amount,
        nonce,
        deadline,
        signature: voucher_sig,
    };
    let wd_ix = crate::solana::withdraw_voucher_ix(
        &program_id,
        &instance,
        &user,
        &mint,
        &user_ata,
        &user,
        &args,
    )?;
    // Submit, with a bounded retry on a transient `InsufficientBalance` (custom
    // program error 0x1771 / 6001). The arborter only issues a voucher once it has
    // CONFIRMED a sufficient settled balance — including after a drain-on-demand
    // (§9) that force-settles the chain right before returning the voucher. In
    // that case the on-chain settle may not yet be visible to this tx's
    // `deposited >= amount` check, so the first submit can fail preflight. A
    // failed-at-simulation tx never executes (no `used_nonce` tombstone is
    // created), so resubmitting the SAME voucher is safe once the settle lands.
    let ixs = [verify_ix, wd_ix];
    let mut last_err = None;
    let mut sig = None;
    for attempt in 0..VOUCHER_SUBMIT_MAX_ATTEMPTS {
        match crate::solana::client::submit_user_signed_multi(&chain.rpc_url, keypair, &ixs).await {
            Ok(s) => {
                sig = Some(s);
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                let transient = msg.contains("0x1771") || msg.contains("InsufficientBalance");
                if transient && attempt + 1 < VOUCHER_SUBMIT_MAX_ATTEMPTS {
                    tracing::warn!(
                        attempt,
                        "voucher submit hit transient InsufficientBalance (settle not yet visible); retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(VOUCHER_SUBMIT_RETRY_MS))
                        .await;
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    let sig = sig.ok_or_else(|| {
        last_err.unwrap_or_else(|| eyre::eyre!("voucher submit failed after retries"))
    })?;
    tracing::info!("Solana withdraw voucher submitted: {}", sig);
    Ok(())
}

/// Max attempts for the Solana voucher submit (retries a transient post-drain
/// `InsufficientBalance` while the force-settle propagates).
#[cfg(feature = "solana")]
const VOUCHER_SUBMIT_MAX_ATTEMPTS: usize = 6;
/// Backoff between voucher-submit retries (~ a couple of slots at 400ms each).
#[cfg(feature = "solana")]
const VOUCHER_SUBMIT_RETRY_MS: u64 = 700;

#[cfg(not(feature = "solana"))]
async fn solana_withdraw(
    _url: String,
    chain: &crate::commands::config::config_pb::Chain,
    _token_symbol: &str,
    _amount: u64,
    _wallet: &Wallet,
) -> Result<()> {
    Err(eyre::eyre!(
        "chain '{}' is Solana but the `solana` feature is disabled",
        chain.network
    ))
}

/// EVM withdraw via the TEE voucher flow (Track A §8): authenticate the request,
/// get an owner-signed voucher from the arborter, submit it on-chain.
async fn call_withdraw_from_config_evm(
    url: String,
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
        "Withdrawing {} {} from {} (chain_id: {}, rpc: {})",
        amount,
        token_symbol,
        network,
        chain.chain_id,
        chain.rpc_url
    );

    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token.address.parse()?;
    let signer_address = signer.address();

    // 1) Request a TEE-signed voucher from the arborter. Authenticate the
    //    request by signing the canonical bytes the arborter rebuilds
    //    (`network|token|account|amount`) with the wallet key (EIP-191). The
    //    request strings below MUST match those bytes exactly.
    let req_account = signer_address.to_string();
    let req_token = token.address.clone();
    let req_amount = amount.to_string();
    let canonical = format!("{network}|{req_token}|{req_account}|{req_amount}");
    let req_sig = signer.sign_message(canonical.as_bytes()).await?;

    let channel = create_channel(&url).await?;
    let mut client = ArborterServiceClient::new(channel);
    let voucher = client
        .withdraw(tonic::Request::new(WithdrawRequest {
            network: network.clone(),
            token: req_token,
            account: req_account,
            amount: req_amount,
            signature: req_sig.as_bytes().to_vec(),
        }))
        .await?
        .into_inner();
    tracing::info!(
        "Received withdrawal voucher (nonce={}, expiry={})",
        voucher.nonce,
        voucher.expiry
    );

    // 2) Submit withdraw(voucher, signature) on-chain (the wallet pays gas).
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&chain.rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain(chain_type)
        .wallet(wallet)
        .connect_http(rpc_url);

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

    let contract = MidribV3::new(contract_addr, &provider);
    let onchain_voucher = MidribV3::WithdrawalVoucher {
        account: signer_address,
        token: token_addr,
        // Echoed back by the arborter; fall back to the requested amount.
        amount: U256::from_str(&voucher.amount).unwrap_or(U256::from(amount)),
        nonce: U256::from(voucher.nonce),
        expiry: U256::from(voucher.expiry),
    };
    let result = contract
        .withdraw(onchain_voucher, Bytes::from(voucher.signature))
        .send()
        .await?
        .with_required_confirmations(1)
        .watch()
        .await?;

    tracing::info!("Withdraw voucher submitted on-chain: {result:?}");

    Ok(())
}
