use std::str::FromStr;

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;
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
use arborter_pb::WithdrawRequest;
use arborter_pb::arborter_service_client::ArborterServiceClient;

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
    amount: u128,
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<()> {
    call_withdraw_from_config_with_wallet_opts(
        url,
        network,
        token_symbol,
        amount,
        wallet,
        config,
        WithdrawOpts::default(),
    )
    .await
}

/// Behavior options for [`call_withdraw_from_config_with_wallet_opts`].
#[derive(Debug, Clone)]
pub struct WithdrawOpts {
    /// Solana WSOL (native SOL) withdrawals only: after the voucher withdraw
    /// lands, close the WSOL ATA in the same transaction, unwrapping its
    /// ENTIRE wrapped balance (withdrawn amount + any pre-existing WSOL) plus
    /// rent back to SOL — standard wallet behavior. Set `false` to keep the
    /// withdrawn funds as WSOL (e.g. when deliberately holding wrapped SOL).
    pub unwrap_native: bool,
}

impl Default for WithdrawOpts {
    fn default() -> Self {
        Self {
            unwrap_native: true,
        }
    }
}

/// [`call_withdraw_from_config_with_wallet`] with explicit [`WithdrawOpts`].
pub async fn call_withdraw_from_config_with_wallet_opts(
    url: String,
    network: String,
    token_symbol: String,
    amount: u128,
    wallet: &Wallet,
    config: GetConfigResponse,
    opts: WithdrawOpts,
) -> Result<()> {
    let chain_for_arch = config
        .get_chain(&network)
        .ok_or_else(|| eyre::eyre!("Chain '{}' not found in configuration", network))?;

    if chain_for_arch
        .architecture
        .eq_ignore_ascii_case(ARCH_SOLANA)
    {
        // Solana SPL token amounts are natively u64 — downcast (checked) at the
        // boundary (DEC-1: u128 upstream, u64 on Solana).
        let spl_amount: u64 = amount.try_into().map_err(|_| {
            eyre::eyre!("amount {amount} exceeds the SPL token u64 max on Solana chain '{network}'")
        })?;
        return solana_withdraw(
            url,
            chain_for_arch,
            &token_symbol,
            spl_amount,
            wallet,
            opts.unwrap_native,
        )
        .await;
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
    unwrap_native: bool,
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

    // 0) Pre-flight SOL check BEFORE requesting a voucher. A voucher places an
    //    off-chain withdraw HOLD on the funds (the arborter reserves them so
    //    they can't be re-withdrawn/traded until the voucher lands or expires).
    //    Requesting one we can't pay to submit — a 0-lamport fee-payer fails
    //    with "Attempt to debit an account but found no record of a prior
    //    credit" — would strand that hold until expiry. Fail fast instead so
    //    the funds stay immediately withdrawable.
    {
        use solana_client::nonblocking::rpc_client::RpcClient;
        let rpc = RpcClient::new(chain.rpc_url.clone());
        let lamports = rpc.get_balance(&user).await.unwrap_or(0);
        if lamports < MIN_SOL_LAMPORTS {
            return Err(eyre::eyre!(
                "insufficient SOL for fees: wallet {user} has {lamports} lamports, \
                 need >= {MIN_SOL_LAMPORTS}. Fund/airdrop SOL before withdrawing — \
                 requesting a voucher now would place an off-chain hold you can't submit.",
            ));
        }
    }

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

    // 5) Submit [create-ATA (idempotent), Ed25519 verify ix, withdraw_voucher ix]
    //    — the user pays + signs. The ATA-create ensures the recipient SPL
    //    account exists (the program won't `init` it); it goes FIRST so the
    //    verify+withdraw pair stay adjacent (the program checks that the ix
    //    immediately before `withdraw_voucher` is the Ed25519 verify).
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
    // Ensure the withdrawer's destination ATA exists before the SPL transfer
    // (SOL-VOUCHER-ATA). Idempotent — harmless if already present — and ordered
    // FIRST so `verify_ix` stays immediately before `wd_ix`.
    let ata_ix = crate::solana::create_idempotent_ata_ix(&user, &user, &mint, &user_ata);
    let mut ixs = vec![ata_ix, verify_ix, wd_ix];
    // Native SOL (WSOL) withdrawal: unwrap in the same tx by closing the WSOL
    // ATA AFTER the voucher transfer credits it — the close sends the account's
    // ENTIRE wrapped balance + rent back to the user as SOL. Appending keeps
    // verify_ix immediately before wd_ix (the program introspects that pair).
    if unwrap_native && crate::solana::is_wsol_mint(&token.address) {
        tracing::info!("WSOL withdrawal: appending unwrap (CloseAccount) to the voucher tx");
        ixs.push(crate::solana::close_token_account_ix(
            &user_ata, &user, &user,
        ));
    }
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

/// Minimum SOL (lamports) the fee-payer needs before we request a Solana
/// voucher. ~0.003 SOL — covers the worst case where the withdrawer's
/// destination ATA doesn't exist yet and the idempotent create must pay rent
/// (~0.00204 SOL) plus a couple of tx fees of headroom (SOL-VOUCHER-ATA).
#[cfg(feature = "solana")]
const MIN_SOL_LAMPORTS: u64 = 3_000_000;

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
    _unwrap_native: bool,
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
    amount: u128,
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

    // Build the wallet-enabled provider up front so the gas pre-check and the
    // submit share it.
    let wallet = EthereumWallet::new(signer.clone());
    let rpc_url = Url::parse(&chain.rpc_url)?;
    let provider = ProviderBuilder::new()
        .with_chain(chain_type)
        .wallet(wallet)
        .connect_http(rpc_url);

    // 1) Pre-flight gas check BEFORE requesting a voucher. A voucher places an
    //    off-chain withdraw HOLD on the funds (reserved until the voucher lands
    //    or expires), so requesting one we can't submit (no gas) would strand
    //    that hold until expiry. Fail fast instead so the funds stay
    //    immediately withdrawable.
    let gas_balance = provider.get_balance(signer_address).await?;
    tracing::info!("Gas balance: {} wei", gas_balance);
    if gas_balance < U256::from(MIN_GAS_BALANCE) {
        let balance_eth = gas_balance.to::<u128>() as f64 / 1e18;
        return Err(eyre::eyre!(
            "insufficient gas: wallet has {:.6} native tokens, need at least 0.0001 for gas. \
            Fund your wallet ({}) with native tokens on {} to pay for transaction fees. \
            (No voucher requested — your withdrawable balance is untouched.)",
            balance_eth,
            signer_address,
            network
        ));
    }

    // 2) Request a TEE-signed voucher from the arborter. Authenticate the
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

    // 3) Submit withdraw(voucher, signature) on-chain, RESUBMITTING the SAME
    //    voucher on a transient failure. A settle-propagation race can briefly
    //    revert `MidribV3.withdraw` with INSUFFICIENT_BALANCE (the drain settle
    //    that backs the voucher isn't visible to this tx yet); a reverted tx
    //    never sets `usedWithdrawNonces`, so resubmitting the identical voucher
    //    is safe and avoids re-requesting (which would hit the now-held balance).
    let contract = MidribV3::new(contract_addr, &provider);
    let onchain_voucher = MidribV3::WithdrawalVoucher {
        account: signer_address,
        token: token_addr,
        // Echoed back by the arborter; fall back to the requested amount.
        amount: U256::from_str(&voucher.amount).unwrap_or(U256::from(amount)),
        nonce: U256::from(voucher.nonce),
        expiry: U256::from(voucher.expiry),
    };
    let voucher_sig = Bytes::from(voucher.signature);
    let mut last_err = None;
    let mut result = None;
    for attempt in 0..EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS {
        // `send` and `watch` surface distinct alloy error types, so match each
        // rather than `?`-unify them; flatten both into the retry's last_err.
        let outcome = match contract
            .withdraw(onchain_voucher.clone(), voucher_sig.clone())
            .send()
            .await
        {
            Ok(pending) => pending
                .with_required_confirmations(1)
                .watch()
                .await
                .map_err(|e| eyre::eyre!("{e}")),
            Err(e) => Err(eyre::eyre!("{e}")),
        };
        match outcome {
            Ok(tx) => {
                result = Some(tx);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS {
                    tracing::warn!(
                        attempt,
                        "voucher submit failed (possibly settle-propagation race); \
                         resubmitting the same voucher"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(
                        EVM_VOUCHER_SUBMIT_RETRY_MS,
                    ))
                    .await;
                }
            }
        }
    }
    let result = result.ok_or_else(|| {
        last_err.unwrap_or_else(|| eyre::eyre!("voucher submit failed after retries"))
    })?;

    tracing::info!("Withdraw voucher submitted on-chain: {result:?}");

    Ok(())
}

/// EVM voucher-submit retries: resubmit the SAME voucher on a transient
/// settle-propagation revert. Bounded; a reverted tx doesn't consume the
/// on-chain nonce, so resubmission is safe.
const EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS: usize = 4;
/// Backoff between EVM voucher-submit retries.
const EVM_VOUCHER_SUBMIT_RETRY_MS: u64 = 700;
