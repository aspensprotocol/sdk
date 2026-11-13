use std::str::FromStr;
use std::time::Duration;

use crate::chain_client::named_chain_for;
use alloy::consensus::TxEnvelope;
use alloy::eips::eip2718::Encodable2718;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::fillers::{
    BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller,
    SimpleNonceManager, WalletFiller,
};
use alloy::providers::{
    Identity, PendingTransactionBuilder, Provider, ProviderBuilder, RootProvider,
};
use alloy::signers::local::PrivateKeySigner;
use eyre::Result;
use url::Url;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::evm::rpc::MidribV3;
use crate::grpc::create_channel;
use crate::wallet::{Architecture, CurveType, Wallet};

use super::arborter_pb::WithdrawRequest;
use super::arborter_pb::arborter_service_client::ArborterServiceClient;

/// Minimum gas balance required for transactions (0.0001 ETH = 100000 gwei)
const MIN_GAS_BALANCE: u128 = 100_000_000_000_000; // 0.0001 ETH in wei

/// Where the TEE-signed withdrawal voucher is requested from.
///
/// The voucher is the ONLY part of a withdraw that needs the arborter — the gas
/// pre-check, the on-chain submit, and the resubmit-on-transient-revert logic
/// below are pure chain work and are identical either way. Keeping the choice in
/// one enum is what lets the FCE path reuse all of that verbatim instead of
/// forking a second withdraw implementation that could drift.
pub(crate) enum VoucherSource<'a> {
    /// Arborter gRPC at this stack URL (the default).
    Grpc(&'a str),
    /// The FCE ext-proxy (`POST /direct` + poll, command WITHDRAW).
    #[cfg(feature = "fce")]
    Fce(&'a crate::fce::FceClient),
}

/// How [`withdraw_with_retry`] paces its attempts: one wait after each refused
/// attempt but the last, and a bound on how long after the first attempt a new
/// one may still start.
pub(crate) struct RetrySchedule<'a> {
    /// Waits between attempts; the attempts are one more than this.
    pub(crate) waits: &'a [std::time::Duration],
    /// No attempt starts later than this after the first one started.
    pub(crate) deadline: std::time::Duration,
}

impl RetrySchedule<'static> {
    /// Five attempts, 0.5 s, 1 s, 2 s and 4 s apart (7.5 s of waiting), and no
    /// new attempt more than 60 s after the first. One refused attempt can
    /// itself take about 20 s (the venue waits up to 10 s twice for a
    /// chain's settlement lock), so without the bound five of them would run
    /// close to two minutes.
    pub(crate) const WITHDRAW: Self = Self {
        waits: &[
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(4),
        ],
        deadline: std::time::Duration::from_secs(60),
    };
}

/// The message tonic 0.14 gives a status it infers from an HTTP 429 whose
/// body is empty (no `grpc-status` header, nothing to decode).
const HTTP_429_INFERRED_MESSAGE: &str =
    "grpc-status header missing, mapped from HTTP status code 429";

/// The text tonic 0.14 puts in the `INTERNAL` status it raises when a 429's
/// non-empty body (such as a rate limiter's "Too Many Requests") does not
/// decode as a gRPC frame; it names the HTTP status of the response.
const HTTP_429_BODY_MESSAGE: &str = "while receiving response with status: 429";

/// The words every venue refusal of "come back shortly" carries (another
/// withdrawal in progress for the account, a settlement batch being resolved,
/// the settle lock busy past its bound, a hold that could not be made
/// durable). The venue writes them only on paths that return before any
/// voucher is signed; some come after a hold was placed, and the venue
/// releases it before answering.
const VENUE_NOT_ISSUED_MARKER: &str = "withdrawal not issued";

/// Whether `status` came from an HTTP 429 answered by the gateway in front of
/// the venue. The venue itself never answers 429, so such a request was not
/// forwarded. tonic gives the status a different code and text depending on
/// whether the 429 had a body, and only these two pairings are recognised:
/// `UNAVAILABLE` with the inferred text (empty body), and `INTERNAL` carrying
/// the response-status text (a body that does not decode). The same text
/// under any other code is not tonic's doing and is not a retry request.
fn is_gateway_rate_limit(status: &tonic::Status) -> bool {
    let message = status.message();
    match status.code() {
        tonic::Code::Unavailable => message == HTTP_429_INFERRED_MESSAGE,
        tonic::Code::Internal => message.contains(HTTP_429_BODY_MESSAGE),
        _ => false,
    }
}

/// Whether `status` is a refusal that provably happened before any voucher
/// was issued, so asking again cannot produce a second voucher.
///
/// `UNAVAILABLE` alone is not enough. A connection that drops after the venue
/// has placed a hold and signed a voucher also surfaces as `UNAVAILABLE` with
/// a transport message, and the withdrawal request carries no nonce, so the
/// venue would treat a repeat as a new request and issue a second voucher for
/// a further amount while the balance allows. Only two shapes are known to be
/// pre-issue: `UNAVAILABLE` with the venue's own message saying the
/// withdrawal was not issued, and the gateway's HTTP 429
/// ([`is_gateway_rate_limit`]). Every other status is returned to the caller,
/// including any transport failure and a gateway 502, 503 or 504 (which can
/// follow a request the venue did serve). The venue's "exits paused"
/// refusal (its signer locked, unreachable or failing attestation) is retried
/// when it says the withdrawal was not issued, like any other such refusal.
fn refused_before_issue(status: &tonic::Status) -> bool {
    (status.code() == tonic::Code::Unavailable
        && status.message().contains(VENUE_NOT_ISSUED_MARKER))
        || is_gateway_rate_limit(status)
}

/// Run `call` (one `Withdraw` request, built fresh or cloned each time), and
/// repeat it after each wait of `schedule` while the answer is a refusal that
/// happened before any voucher was issued ([`refused_before_issue`]). The last
/// status is returned once the waits run out, or once the next attempt would
/// start more than the schedule's deadline after the first (an attempt that
/// would start exactly at the deadline does start). Any other answer, success
/// or failure, is returned at once.
///
/// The same signed request is sent each time: the venue authenticates the
/// signature over `network|token|account|amount` only, with no nonce or
/// expiry, so a repeat verifies exactly as the first did.
pub(crate) async fn withdraw_with_retry<F, Fut>(
    schedule: &RetrySchedule<'_>,
    mut call: F,
) -> Result<super::arborter_pb::WithdrawResponse, tonic::Status>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<super::arborter_pb::WithdrawResponse, tonic::Status>>,
{
    let first = tokio::time::Instant::now();
    let mut waits = schedule.waits.iter();
    loop {
        match call().await {
            Err(status) if refused_before_issue(&status) => match waits.next() {
                Some(wait) if first.elapsed() + *wait <= schedule.deadline => {
                    tracing::warn!(
                        message = status.message(),
                        wait_ms = wait.as_millis() as u64,
                        "withdrawal refused before issue; retrying"
                    );
                    tokio::time::sleep(*wait).await;
                }
                _ => return Err(status),
            },
            other => return other,
        }
    }
}

/// Request a voucher over gRPC, retrying per `schedule`.
async fn fetch_grpc(
    url: &str,
    request: WithdrawRequest,
    schedule: &RetrySchedule<'_>,
) -> Result<Voucher> {
    let channel = create_channel(url).await?;
    let client = ArborterServiceClient::new(channel);
    let v = withdraw_with_retry(schedule, || {
        let mut client = client.clone();
        let request = request.clone();
        async move {
            Ok(client
                .withdraw(tonic::Request::new(request))
                .await?
                .into_inner())
        }
    })
    .await?;
    Ok(Voucher {
        amount: v.amount,
        nonce: v.nonce,
        expiry: v.expiry,
        signature: v.signature,
    })
}

/// The voucher fields both transports return, normalized. Deliberately not the
/// gRPC `WithdrawResponse`: the submit code below should not care which wire
/// produced these bytes.
pub(crate) struct Voucher {
    /// u128 decimal, echoed by the arborter.
    amount: String,
    nonce: u64,
    expiry: u64,
    signature: Vec<u8>,
}

impl VoucherSource<'_> {
    /// Request a voucher for an already-authenticated withdraw. `signature` is
    /// the caller's signature over the canonical `network|token|account|amount`
    /// bytes — both transports carry exactly those bytes, so the arborter
    /// authenticates identically regardless of which one is in use.
    async fn fetch(
        &self,
        network: &str,
        token: &str,
        account: &str,
        amount: &str,
        signature: Vec<u8>,
    ) -> Result<Voucher> {
        match self {
            Self::Grpc(url) => {
                let request = WithdrawRequest {
                    network: network.to_string(),
                    token: token.to_string(),
                    account: account.to_string(),
                    amount: amount.to_string(),
                    signature,
                };
                fetch_grpc(url, request, &RetrySchedule::WITHDRAW).await
            }
            #[cfg(feature = "fce")]
            Self::Fce(fce) => {
                let outcome = fce
                    .withdraw(&crate::fce::WithdrawRequest {
                        network: network.to_string(),
                        token: token.to_string(),
                        account: account.to_string(),
                        amount: amount.to_string(),
                        signature,
                    })
                    .await?;
                let v = outcome.into_data()?;
                Ok(Voucher {
                    amount: v.amount,
                    nonce: v.nonce,
                    expiry: v.expiry,
                    signature: v.signature,
                })
            }
        }
    }

    /// The instance signer pubkey for `chain` — the Ed25519 key that signed a
    /// Solana voucher, which the on-chain program's verify must recover to.
    ///
    /// Only the gRPC transport has a `GetSignerPublicKey` RPC; the direct-action
    /// wire has no such command. Over FCE this comes from the chain config
    /// instead, which the same proxy already serves via `GET_CONFIG`.
    #[cfg(feature = "solana")]
    async fn instance_signer_pubkey(
        &self,
        chain: &crate::commands::config::config_pb::Chain,
    ) -> Result<String> {
        match self {
            Self::Grpc(url) => {
                let resp = crate::commands::config::get_signer_public_key(
                    url.to_string(),
                    Some(chain.network.clone()),
                )
                .await?;
                resp.chain_keys
                    .get(&chain.network)
                    .map(|k| k.public_key.clone())
                    .ok_or_else(|| {
                        eyre::eyre!("no signer public key for chain '{}'", chain.network)
                    })
            }
            #[cfg(feature = "fce")]
            Self::Fce(_) => {
                let key = chain.instance_signer_address.trim();
                if key.is_empty() {
                    // Fail loudly rather than submitting a voucher whose
                    // signature can't be verified on-chain: the tx would revert
                    // after the hold is already placed.
                    return Err(eyre::eyre!(
                        "chain '{}' has no instance_signer_address in the config, and the FCE \
                         direct-action wire has no GetSignerPublicKey command — the Solana \
                         voucher's signer key cannot be resolved. Set it on the chain entry \
                         (aspens-admin set-chain --instance-signer-address) and re-fetch config.",
                        chain.network
                    ));
                }
                Ok(key.to_string())
            }
        }
    }
}

/// Withdraw tokens using a curve-agnostic wallet.
///
/// Branches on `chain.architecture`:
/// - **EVM**: requests a TEE-signed withdrawal voucher from the arborter
///   (`url`) over gRPC, then submits `MidribV3.withdraw(voucher, signature)`
///   on-chain (the wallet pays gas). There is no permissionless on-chain
///   `withdraw`; the voucher is the authorization.
/// - **Solana**: requests a TEE-signed withdrawal voucher from the arborter,
///   then submits the Midrib program's `withdraw_voucher` instruction
///   on-chain (the wallet pays gas). The voucher is the authorization,
///   mirroring the EVM path.
/// - **Tron**: the EVM voucher flow with the request signed under the TRON
///   message prefix and `withdraw(voucher, signature)` sent as a native Tron
///   transaction (the wallet burns TRX for its energy).
///
/// A voucher request the venue refuses before issuing anything is repeated:
/// five attempts, and no new attempt more than a minute after the first. The
/// last attempt can start at the minute and then has the channel's own
/// one-minute timeout, so the voucher can take about two minutes to arrive.
/// The EVM submit that follows is bounded too: up to four attempts, each
/// given a minute to confirm and then ten seconds to read whether the voucher
/// was paid, so nearly five minutes more, and about seven for the whole call.
/// When a transaction was broadcast but the chain never said the voucher was
/// paid, the error is a [`WithdrawOutcomeUnknown`]: the withdrawal can still
/// land, so check the chain before asking again. A voucher whose nonce the
/// contract already shows used before anything is sent is a
/// [`WithdrawNonceAlreadyUsed`], and nothing is submitted.
///
/// Each call yields at most one voucher. A second concurrent call for the
/// same account, which the venue refuses while the first is in progress,
/// waits and then proceeds as its own withdrawal; de-duplicating intent is
/// the caller's job. Dropping the future, or timing it out from the outside,
/// before the voucher has arrived withdraws nothing twice: nobody holds the
/// signature, and the venue keeps the funds held until its expiry sweep
/// releases them. A second withdrawal happens when the first call keeps
/// running after the caller gave up (as when a tool host times out a call it
/// does not abort), or was dropped after the on-chain submit was broadcast,
/// and the caller then asks again. A caller that gives up on a call must
/// check the balance and the voucher state before withdrawing again for the
/// same intent.
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
    withdraw_from_source(
        VoucherSource::Grpc(&url),
        network,
        token_symbol,
        amount,
        wallet,
        config,
        opts,
    )
    .await
}

/// [`call_withdraw_from_config_with_wallet_opts`], sourcing the voucher from the
/// FCE ext-proxy instead of arborter gRPC.
///
/// Everything after the voucher is byte-identical to the gRPC path — same
/// canonical request signature, same on-chain `MidribV3.withdraw` submit, same
/// retries. Only the transport that carries the voucher request differs.
#[cfg(feature = "fce")]
pub async fn call_withdraw_from_config_with_fce_opts(
    fce: &crate::fce::FceClient,
    network: String,
    token_symbol: String,
    amount: u128,
    wallet: &Wallet,
    config: GetConfigResponse,
    opts: WithdrawOpts,
) -> Result<()> {
    withdraw_from_source(
        VoucherSource::Fce(fce),
        network,
        token_symbol,
        amount,
        wallet,
        config,
        opts,
    )
    .await
}

async fn withdraw_from_source(
    source: VoucherSource<'_>,
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

    match crate::wallet::chain_architecture(chain_for_arch)? {
        Architecture::Solana => {
            // Solana SPL token amounts are natively u64 — downcast (checked) at the
            // boundary (DEC-1: u128 upstream, u64 on Solana).
            let spl_amount: u64 = amount.try_into().map_err(|_| {
                eyre::eyre!(
                    "amount {amount} exceeds the SPL token u64 max on Solana chain '{network}'"
                )
            })?;
            return solana_withdraw(
                source,
                chain_for_arch,
                &token_symbol,
                spl_amount,
                wallet,
                opts.unwrap_native,
            )
            .await;
        }
        Architecture::Tron => {
            return tron_withdraw(source, chain_for_arch, &token_symbol, amount, wallet).await;
        }
        Architecture::Evm => {}
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

    call_withdraw_from_config_evm(source, network, token_symbol, amount, signer, config).await
}

/// Solana withdraw — requests a TEE-signed withdrawal voucher from the
/// arborter, then builds and submits the Midrib program's `withdraw_voucher`
/// instruction. Requires the `solana` feature.
#[cfg(feature = "solana")]
async fn solana_withdraw(
    source: VoucherSource<'_>,
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
    // Before requesting a voucher (which places a hold): a missing or dead
    // endpoint fails here, politely, with nothing held.
    let rpc_url = crate::chain_client::checked_chain_rpc_url(chain).await?;
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
        let rpc = RpcClient::new(rpc_url.clone());
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
    let req_sig = super::sign_withdraw_request(wallet, Architecture::Solana, &canonical).await?;

    // 2) Request the TEE-signed voucher.
    let voucher = source
        .fetch(
            &chain.network,
            &token.address,
            &account_str,
            &amount.to_string(),
            req_sig,
        )
        .await?;
    tracing::info!(
        "Received Solana withdrawal voucher (nonce={}, deadline_slot={})",
        voucher.nonce,
        voucher.expiry
    );

    // 3) Fetch the instance signer pubkey (the Ed25519 key that signed the
    //    voucher) so the program's Ed25519 verify recovers to it.
    let signer_str = source.instance_signer_pubkey(chain).await?;
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
    // that force-settles the chain right before returning the voucher. In
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
        match crate::solana::client::submit_user_signed_multi(&rpc_url, keypair, &ixs).await {
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
    _source: VoucherSource<'_>,
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

/// TRX a Tron withdraw needs before a voucher is requested, in sun: the
/// costliest measured voucher withdraw on Nile (84,386 energy x 100 sun +
/// 604,000 sun of bandwidth). A voucher places a hold, so one the wallet
/// cannot pay to submit would strand it until expiry.
#[cfg(feature = "tron")]
const MIN_TRX_FOR_WITHDRAW_SUN: u128 = 84_386 * 100 + 604_000;

/// Tron withdraw via the TEE voucher flow: the EVM flow with the request
/// signed under the TRON prefix and the voucher redeemed by a native Tron
/// transaction ([`submit_tron_voucher`]).
#[cfg(feature = "tron")]
async fn tron_withdraw(
    source: VoucherSource<'_>,
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u128,
    wallet: &Wallet,
) -> Result<()> {
    match source {
        VoucherSource::Grpc(_) => {}
        #[cfg(feature = "fce")]
        VoucherSource::Fce(_) => {
            return Err(eyre::eyre!(
                "chain '{}' is Tron, which the FCE direct-action transport does not carry; \
                 use the gRPC transport",
                chain.network
            ));
        }
    }
    let token = chain.tokens.get(token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on Tron chain '{}'",
            token_symbol,
            chain.network
        )
    })?;
    let owner = wallet
        .as_evm()
        .ok_or_else(|| {
            eyre::eyre!(
                "Tron chain '{}' requires a secp256k1 wallet (TRADER_PRIVKEY)",
                chain.network
            )
        })?
        .address();
    let contract: Address = chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre::eyre!("Trade contract not found for chain '{}'", chain.network))?
        .address
        .parse()?;
    let token_addr: Address = token.address.parse()?;

    // Before requesting a voucher (which places a hold): a missing or dead
    // endpoint, or a wallet with too little TRX to pay for the withdraw,
    // fails here with nothing held.
    let base = crate::chain_client::checked_chain_rpc_url(chain).await?;
    let http = crate::tron::client::TronHttp::for_network(&base, &chain.network)?;
    let jsonrpc = Url::parse(&crate::tron::client::jsonrpc_url(&base)?)?;
    let trx = ProviderBuilder::new()
        .connect_http(jsonrpc)
        .get_balance(owner)
        .await?;
    if trx < U256::from(MIN_TRX_FOR_WITHDRAW_SUN) {
        return Err(eyre::eyre!(
            "insufficient TRX: {owner:#x} holds {trx} sun, and a voucher withdraw burns up to \
             about {MIN_TRX_FOR_WITHDRAW_SUN} sun. Fund it with TRX on {}. (No voucher \
             requested — your withdrawable balance is untouched.)",
            chain.network
        ));
    }

    // The venue carries a Tron account as lowercase 0x hex; the request text
    // is signed as is under the TRON prefix.
    let req_account = format!("{owner:#x}");
    let req_amount = amount.to_string();
    let canonical = format!(
        "{}|{}|{}|{}",
        chain.network, token.address, req_account, req_amount
    );
    let req_sig = super::sign_withdraw_request(wallet, Architecture::Tron, &canonical).await?;
    let voucher = source
        .fetch(
            &chain.network,
            &token.address,
            &req_account,
            &req_amount,
            req_sig,
        )
        .await?;
    tracing::info!(
        "Received Tron withdrawal voucher (nonce={}, expiry={})",
        voucher.nonce,
        voucher.expiry
    );
    submit_tron_voucher(
        &http,
        wallet,
        contract,
        token_addr,
        amount,
        voucher,
        &crate::tron::client::SendConfig::default(),
    )
    .await
}

#[cfg(not(feature = "tron"))]
async fn tron_withdraw(
    _source: VoucherSource<'_>,
    chain: &crate::commands::config::config_pb::Chain,
    _token_symbol: &str,
    _amount: u128,
    _wallet: &Wallet,
) -> Result<()> {
    Err(eyre::eyre!(
        "chain '{}' is Tron but the `tron` feature is disabled",
        chain.network
    ))
}

/// Redeem `voucher` with `withdraw(voucher, signature)` on a Tron instance.
///
/// A withdraw the simulation refuses was never signed or sent (a fresh
/// voucher's backing settle may not be visible yet), so the same voucher is
/// tried again, as on EVM. One that was sent and failed is an `Err` at once:
/// it burned its energy, and its receipt says why.
#[cfg(feature = "tron")]
pub(crate) async fn submit_tron_voucher(
    http: &crate::tron::client::TronHttp,
    wallet: &Wallet,
    contract: Address,
    token: Address,
    requested_amount: u128,
    voucher: Voucher,
    send: &crate::tron::client::SendConfig,
) -> Result<()> {
    use crate::tron::client::{TronTxError, WITHDRAW_FEE_LIMIT_SUN, send_tron_tx_with};
    use alloy::sol_types::SolCall;

    let account = wallet
        .as_evm()
        .ok_or_else(|| eyre::eyre!("a Tron withdraw needs a secp256k1 wallet"))?
        .address();
    let data = MidribV3::withdrawCall {
        _v: MidribV3::WithdrawalVoucher {
            account,
            token,
            // Echoed back by the arborter; fall back to the requested amount.
            amount: U256::from_str(&voucher.amount).unwrap_or(U256::from(requested_amount)),
            nonce: U256::from(voucher.nonce),
            expiry: U256::from(voucher.expiry),
        },
        _signature: Bytes::from(voucher.signature),
    }
    .abi_encode();

    let mut attempt = 0;
    loop {
        attempt += 1;
        match send_tron_tx_with(
            http,
            wallet,
            contract,
            &data,
            0,
            WITHDRAW_FEE_LIMIT_SUN,
            send,
        )
        .await
        {
            Ok(receipt) => {
                tracing::info!(
                    "Tron withdraw voucher redeemed: {} (block {})",
                    hex::encode(receipt.txid),
                    receipt.block_number
                );
                return Ok(());
            }
            Err(TronTxError::NotSent(e)) if attempt < EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS => {
                tracing::warn!(
                    attempt,
                    error = %e,
                    "Tron voucher withdraw refused before sending (possibly a settle not yet \
                     visible); trying the same voucher again"
                );
                tokio::time::sleep(std::time::Duration::from_millis(
                    EVM_VOUCHER_SUBMIT_RETRY_MS,
                ))
                .await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// EVM withdraw via the TEE voucher flow: authenticate the request, get an
/// owner-signed voucher from the arborter, submit it on-chain. There is no
/// permissionless on-chain `withdraw`; the voucher is the only authorization
/// for an exit.
async fn call_withdraw_from_config_evm(
    source: VoucherSource<'_>,
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

    // alloy's chain metadata, when alloy knows the id. The hand-written match
    // this replaces listed nine chains and then ERRORED on anything else, so a
    // chain the venue happily lists but alloy has no variant for -- HyperEVM
    // testnet 998 -- could not deposit or withdraw at all. See
    // [`named_chain_for`]: an unnamed chain is a normal EVM chain, it just gets
    // no metadata. The RPC reports its own id and the tx is signed here.
    let named_chain = named_chain_for(chain.chain_id as u64);

    // Before requesting a voucher (which places a hold): a missing or dead
    // endpoint fails here, politely, with nothing held.
    let rpc_url = crate::chain_client::checked_chain_rpc_url(chain).await?;

    tracing::info!(
        "Withdrawing {} {} from {} (chain_id: {}, rpc: {})",
        amount,
        token_symbol,
        network,
        chain.chain_id,
        crate::chain_client::redact_rpc_url(&rpc_url)
    );

    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token.address.parse()?;
    let signer_address = signer.address();

    // Two providers on the same endpoint: `provider` for reads and for
    // watching a transaction (with the chain's poll interval when alloy knows
    // the chain), and `signing`, which fills and signs the withdraw locally so
    // its hash is known before it is broadcast.
    let wallet = EthereumWallet::new(signer.clone());
    let rpc_url = Url::parse(&rpc_url)?;
    let provider = match named_chain {
        Some(named) => ProviderBuilder::new()
            .with_chain(named)
            .connect_http(rpc_url.clone())
            .erased(),
        None => ProviderBuilder::new()
            .connect_http(rpc_url.clone())
            .erased(),
    };
    let signing = signing_provider(wallet, rpc_url);

    // 1) Pre-flight gas check BEFORE requesting a voucher. A voucher places an
    //    off-chain withdraw HOLD on the funds (reserved until the voucher lands
    //    or expires), so requesting one we can't submit (no gas) would strand
    //    that hold until expiry. Fail fast instead so the funds stay
    //    immediately withdrawable.
    let gas_balance = tokio::time::timeout(
        crate::chain_client::RPC_PROBE_TIMEOUT,
        provider.get_balance(signer_address),
    )
    .await
    .map_err(|_| {
        eyre::eyre!(
            "reading the gas balance on {network} timed out after {}s (no voucher requested)",
            crate::chain_client::RPC_PROBE_TIMEOUT.as_secs()
        )
    })??;
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
    let req_sig =
        super::sign_withdraw_request(&Wallet::Evm(signer.clone()), Architecture::Evm, &canonical)
            .await?;

    let voucher = source
        .fetch(&network, &req_token, &req_account, &req_amount, req_sig)
        .await?;
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
    //    Each attempt is bounded, and after each one that did not confirm the
    //    contract is asked whether the voucher was paid (see
    //    [`submit_evm_voucher`]).
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
    let signing = &signing;
    let tx_hash = submit_evm_voucher(
        voucher.nonce,
        voucher.expiry,
        || async {
            let request = contract
                .withdraw(onchain_voucher.clone(), voucher_sig.clone())
                .into_transaction_request();
            let envelope = signing
                .fill(request)
                .await?
                .try_into_envelope()
                .map_err(|e| eyre::eyre!("withdraw transaction was not signed: {e}"))?;
            Ok((*envelope.tx_hash(), envelope))
        },
        move |envelope: TxEnvelope| async move {
            let _pending = signing
                .send_raw_transaction(&envelope.encoded_2718())
                .await?;
            Ok(())
        },
        |hash| {
            let root = provider.root().clone();
            async move {
                let receipt = PendingTransactionBuilder::new(root, hash)
                    .with_required_confirmations(1)
                    .get_receipt()
                    .await?;
                receipt_paid(hash, receipt.status())
            }
        },
        || async {
            Ok(contract
                .usedWithdrawNonces(signer_address, U256::from(voucher.nonce))
                .call()
                .await?)
        },
        EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS,
    )
    .await?;

    match tx_hash {
        Some(hash) => tracing::info!(
            "Withdraw voucher nonce {} paid on-chain; last transaction seen: {hash}",
            voucher.nonce
        ),
        None => tracing::info!(
            "Withdraw voucher nonce {} paid on-chain; no transaction of this call was seen",
            voucher.nonce
        ),
    }

    Ok(())
}

/// How long one EVM voucher submit may take, from sending the transaction to
/// its first confirmation, before the contract is asked whether the voucher
/// was paid.
const EVM_VOUCHER_WATCH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the read of whether a voucher nonce was used may take before it
/// counts as unanswered.
const EVM_NONCE_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// An EVM withdrawal whose transaction may have been broadcast but whose
/// outcome could not be learned: it was not seen to confirm, and the contract
/// never said the voucher was paid. The transaction can still land, paying
/// the voucher; a voucher pays at most once, but a new withdrawal for the same
/// intent would be a second voucher, so the chain must be checked first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawOutcomeUnknown {
    /// The voucher's nonce: `usedWithdrawNonces(account, nonce)` on the
    /// contract says whether it was paid.
    pub voucher_nonce: u64,
    /// The voucher's expiry as the venue returned it, a unix time in seconds:
    /// once a block's timestamp is past it the voucher can no longer pay.
    pub voucher_expiry: u64,
    /// The last transaction broadcast for the voucher, or `None` when it is
    /// not known.
    pub tx_hash: Option<B256>,
}

impl std::fmt::Display for WithdrawOutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "withdrawal submitted, outcome unknown: voucher nonce {}, transaction ",
            self.voucher_nonce
        )?;
        match &self.tx_hash {
            Some(hash) => write!(f, "{hash}")?,
            None => write!(f, "unknown")?,
        }
        write!(
            f,
            ", expiry {}; check the chain before asking again: if the nonce is still \
             unused once a block's timestamp is past the expiry, the voucher was not \
             paid and never will be",
            self.voucher_expiry
        )
    }
}

impl std::error::Error for WithdrawOutcomeUnknown {}

/// An EVM withdrawal voucher whose nonce the contract already showed used
/// before the call submitted anything, so nothing was submitted. Anyone can
/// submit a voucher, so there are two causes: this voucher was already paid
/// to the account by another submitter, or the venue issued a nonce that an
/// earlier voucher used. A `Withdrawn` event for the account and nonce, whose
/// token and amount say which voucher it paid, tells them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawNonceAlreadyUsed {
    /// The voucher's nonce, already used on the contract.
    pub voucher_nonce: u64,
    /// The voucher's expiry as the venue returned it, a unix time in seconds.
    pub voucher_expiry: u64,
}

impl std::fmt::Display for WithdrawNonceAlreadyUsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the contract already shows withdrawal voucher nonce {} used before this call \
             submitted anything: either this voucher (expiry {}) was already paid to the \
             account by another submitter, or the venue issued a nonce that an earlier \
             voucher used; check the chain for a Withdrawn event for this account and nonce \
             (its token and amount say which) before withdrawing again",
            self.voucher_nonce, self.voucher_expiry
        )
    }
}

impl std::error::Error for WithdrawNonceAlreadyUsed {}

/// The provider that fills and signs a withdraw transaction.
type SigningProvider = FillProvider<
    JoinFill<
        JoinFill<
            JoinFill<
                JoinFill<JoinFill<Identity, GasFiller>, BlobGasFiller>,
                NonceFiller<SimpleNonceManager>,
            >,
            ChainIdFiller,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider,
>;

/// A provider that fills and signs transactions for `wallet` on `url`.
///
/// It has alloy's recommended fillers except that the account nonce is read
/// from the node's pending transaction count on every fill instead of being
/// cached and advanced locally. A locally advanced nonce moves on a fill
/// whose transaction is never broadcast, such as one whose gas estimate
/// reverts while the settlement backing a voucher is not yet visible: the
/// next transaction is then signed with a nonce past one never used, cannot
/// be mined until that gap is filled, and the voucher submit's retry can
/// never succeed.
fn signing_provider(wallet: EthereumWallet, url: Url) -> SigningProvider {
    ProviderBuilder::new()
        .disable_recommended_fillers()
        .with_gas_estimation()
        .with_blob_gas_estimation()
        .with_simple_nonce_management()
        .fetch_chain_id()
        .wallet(wallet)
        .connect_http(url)
}

/// Whether a confirmed withdraw transaction succeeded: a reverted one paid
/// nothing.
fn receipt_paid(hash: B256, succeeded: bool) -> Result<()> {
    if succeeded {
        Ok(())
    } else {
        Err(eyre::eyre!("withdraw transaction {hash} reverted"))
    }
}

/// Submit one EVM voucher, up to `attempts` times, and report what the chain
/// says about it.
///
/// Before the first attempt, `nonce_used` is asked once whether the voucher's
/// nonce is already used. If it is, nothing is sent and the result is
/// [`WithdrawNonceAlreadyUsed`]: either this voucher was already paid by
/// another submitter, or the venue issued a nonce an earlier voucher used.
/// Reading "used" after an attempt as this call's success relies on this
/// check. If that first read is not answered within
/// [`EVM_NONCE_READ_TIMEOUT`], the submit goes ahead without it rather than
/// block the withdrawal.
///
/// Each attempt runs `prepare` (fills and signs the transaction locally and
/// returns its hash with the signed transaction), then `broadcast` with the
/// signed transaction, then `watch` on the hash (returns once it confirms, an
/// error if it reverted), all three together bounded by
/// [`EVM_VOUCHER_WATCH_TIMEOUT`]. A confirmed transaction is the answer,
/// `Ok(Some(hash))`. After any other attempt, `nonce_used` asks the contract
/// whether the voucher was paid, bounded by [`EVM_NONCE_READ_TIMEOUT`]:
/// - paid: success, `Ok` with the last hash broadcast or attempted (`None`
///   if no attempt got as far as a broadcast; the voucher was paid by a
///   transaction not seen here);
/// - not paid, or no answer: another attempt with the same voucher after a
///   short wait, while attempts remain.
///
/// Once the attempts run out, the result is [`WithdrawOutcomeUnknown`] when
/// any attempt reached the broadcast, whether or not the broadcast answered,
/// since a node may have taken the transaction and it can still land;
/// otherwise every attempt failed before broadcasting and the last error is
/// returned. A voucher pays at most once and a reverted transaction leaves
/// its nonce unused, so resubmitting the same voucher cannot pay it twice.
///
/// The cost of that: an attempt that timed out or saw a transient watch
/// error re-signs at a fresh account nonce while its transaction may still
/// be pending. Only one of them can pay, but up to `attempts - 1` extra
/// transactions can later land, revert on the used nonce, and cost gas.
async fn submit_evm_voucher<T, P, PF, BC, BF, W, WF, N, NF>(
    voucher_nonce: u64,
    voucher_expiry: u64,
    mut prepare: P,
    mut broadcast: BC,
    mut watch: W,
    mut nonce_used: N,
    attempts: usize,
) -> Result<Option<B256>>
where
    P: FnMut() -> PF,
    PF: std::future::Future<Output = Result<(B256, T)>>,
    BC: FnMut(T) -> BF,
    BF: std::future::Future<Output = Result<()>>,
    W: FnMut(B256) -> WF,
    WF: std::future::Future<Output = Result<()>>,
    N: FnMut() -> NF,
    NF: std::future::Future<Output = Result<bool>>,
{
    match tokio::time::timeout(EVM_NONCE_READ_TIMEOUT, nonce_used()).await {
        Ok(Ok(true)) => {
            return Err(WithdrawNonceAlreadyUsed {
                voucher_nonce,
                voucher_expiry,
            }
            .into());
        }
        Ok(Ok(false)) => {}
        Ok(Err(e)) => tracing::warn!(
            error = %e,
            "could not read whether the voucher nonce is already used; submitting anyway"
        ),
        Err(_) => tracing::warn!(
            "reading whether the voucher nonce is already used timed out; submitting anyway"
        ),
    }

    let mut last_hash = None;
    let mut last_err = None;
    for attempt in 0..attempts {
        // Set once the transaction is signed, before it is broadcast: from
        // then on a node may hold it, whatever the broadcast answers.
        let mut broadcasting = None;
        let submitted = tokio::time::timeout(EVM_VOUCHER_WATCH_TIMEOUT, async {
            let (hash, signed) = prepare().await?;
            broadcasting = Some(hash);
            broadcast(signed).await?;
            watch(hash).await.map(|()| hash)
        })
        .await;
        if broadcasting.is_some() {
            last_hash = broadcasting;
        }
        let err = match submitted {
            Ok(Ok(hash)) => return Ok(Some(hash)),
            Ok(Err(e)) => e,
            Err(_) => eyre::eyre!(
                "withdraw transaction not confirmed within {}s",
                EVM_VOUCHER_WATCH_TIMEOUT.as_secs()
            ),
        };
        tracing::warn!(attempt, error = %err, "voucher submit did not confirm");
        last_err = Some(err);

        match tokio::time::timeout(EVM_NONCE_READ_TIMEOUT, nonce_used()).await {
            Ok(Ok(true)) => return Ok(last_hash),
            Ok(Ok(false)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "could not read whether the voucher was paid"),
            Err(_) => tracing::warn!("reading whether the voucher was paid timed out"),
        }

        if attempt + 1 < attempts {
            tracing::warn!(
                attempt,
                "voucher not paid (possibly settle-propagation race); \
                 resubmitting the same voucher"
            );
            tokio::time::sleep(Duration::from_millis(EVM_VOUCHER_SUBMIT_RETRY_MS)).await;
        }
    }
    if last_hash.is_some() {
        return Err(WithdrawOutcomeUnknown {
            voucher_nonce,
            voucher_expiry,
            tx_hash: last_hash,
        }
        .into());
    }
    Err(last_err.unwrap_or_else(|| eyre::eyre!("voucher submit made no attempt")))
}

/// EVM voucher-submit retries: resubmit the SAME voucher on a transient
/// settle-propagation revert. Bounded; a reverted tx doesn't consume the
/// on-chain nonce, so resubmission is safe.
const EVM_VOUCHER_SUBMIT_MAX_ATTEMPTS: usize = 4;
/// Backoff between EVM voucher-submit retries.
const EVM_VOUCHER_SUBMIT_RETRY_MS: u64 = 700;

#[cfg(all(test, feature = "tron"))]
mod tron_tests {
    use super::*;
    use crate::tron::client::mock::*;
    use crate::tron::client::{TronHttp, WITHDRAW_FEE_LIMIT_SUN};
    use alloy::sol_types::SolCall;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn voucher() -> Voucher {
        Voucher {
            amount: "1000000".into(),
            nonce: 7,
            expiry: 1_900_000_000,
            signature: vec![0x11; 65],
        }
    }

    async fn submit(server: &MockServer) -> Result<()> {
        submit_tron_voucher(
            &TronHttp::new(&server.uri()).unwrap(),
            &wallet(),
            MIDRIB.parse().unwrap(),
            USDT.parse().unwrap(),
            1_000_000,
            voucher(),
            &fast(),
        )
        .await
    }

    /// A voucher withdraw that landed and reverted (the stock contract's
    /// `TransferFailed()` on USDT) is an `Err` naming the txID, and it is not
    /// broadcast again.
    #[tokio::test]
    async fn tron_withdraw_reverted_is_err() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("REVERT", 1)).await;
        let err = submit(&server).await.unwrap_err().to_string();
        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 1, "a landed revert is not resubmitted");
        let tx = decode_sent(&sent[0]);
        let id = crate::tron::txid(&tx.raw);
        assert!(err.contains(&hex::encode(id)), "{err}");
        assert!(err.contains("REVERT"), "{err}");
        let data = bytes_of(&tx.trigger, 4);
        assert_eq!(data[..4], MidribV3::withdrawCall::SELECTOR);
        assert_eq!(
            varint_of(&fields(&tx.raw), 18) as i64,
            WITHDRAW_FEE_LIMIT_SUN
        );
    }

    /// The voucher withdraw sends `withdraw(voucher, signature)` for the
    /// wallet's own account, with no call value, and succeeds on SUCCESS.
    #[tokio::test]
    async fn tron_withdraw_sends_the_voucher() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        submit(&server).await.unwrap();
        let tx = decode_sent(&broadcasts(&server).await[0]);
        assert_eq!(field(&tx.trigger, 3), None);
        let call = MidribV3::withdrawCall::abi_decode(&bytes_of(&tx.trigger, 4)).unwrap();
        assert_eq!(call._v.account, wallet().as_evm().unwrap().address());
        assert_eq!(call._v.token, USDT.parse::<Address>().unwrap());
        assert_eq!(call._v.amount, U256::from(1_000_000u64));
        assert_eq!(call._v.nonce, U256::from(7u64));
        assert_eq!(call._v.expiry, U256::from(1_900_000_000u64));
        assert_eq!(call._signature.to_vec(), vec![0x11; 65]);
    }

    /// The settle backing a fresh voucher may not be visible yet, so the
    /// simulation refuses it. Nothing was sent, so the same voucher is tried
    /// again, and goes through once the simulation passes.
    #[tokio::test]
    async fn tron_withdraw_retries_a_simulated_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/wallet/triggerconstantcontract"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "transaction": {"ret": [{"ret": "FAILED"}]},
                "result": {"result": true, "message": hex::encode("REVERT opcode executed")},
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        submit(&server).await.unwrap();
        assert_eq!(
            bodies(&server, "/wallet/triggerconstantcontract")
                .await
                .len(),
            2
        );
        assert_eq!(broadcasts(&server).await.len(), 1);
    }
}

#[cfg(test)]
mod retry_tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::time::Instant;
    use tonic::{Code, Status};

    use super::super::arborter_pb::WithdrawResponse;
    use super::{HTTP_429_INFERRED_MESSAGE, RetrySchedule, withdraw_with_retry};

    const IN_PROGRESS: &str = "another withdrawal for this account on base is in progress; \
                               withdrawal not issued — retry shortly";

    fn voucher() -> WithdrawResponse {
        WithdrawResponse {
            amount: "1000000".into(),
            nonce: 7,
            ..Default::default()
        }
    }

    /// Answers each call from `script` in order, counts the calls and notes
    /// when each started.
    struct Script {
        answers: Mutex<Vec<Result<WithdrawResponse, Status>>>,
        calls: AtomicUsize,
        started: Mutex<Vec<Duration>>,
        origin: Instant,
        /// Virtual time each call takes before it answers.
        takes: Duration,
    }

    impl Script {
        fn new(mut answers: Vec<Result<WithdrawResponse, Status>>) -> Self {
            answers.reverse();
            Self {
                answers: Mutex::new(answers),
                calls: AtomicUsize::new(0),
                started: Mutex::new(Vec::new()),
                origin: Instant::now(),
                takes: Duration::ZERO,
            }
        }
        async fn next(&self) -> Result<WithdrawResponse, Status> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.lock().unwrap().push(self.origin.elapsed());
            tokio::time::sleep(self.takes).await;
            self.answers.lock().unwrap().pop().expect("script ran out")
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn started(&self) -> Vec<Duration> {
            self.started.lock().unwrap().clone()
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_twice_then_ok_is_one_voucher_after_one_and_a_half_seconds() {
        let script = Script::new(vec![
            Err(Status::unavailable(IN_PROGRESS)),
            Err(Status::unavailable(IN_PROGRESS)),
            Ok(voucher()),
        ]);
        let start = Instant::now();
        let got = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next()).await;
        assert_eq!(got.unwrap().nonce, 7);
        assert_eq!(script.calls(), 3);
        // 0.5 s + 1 s.
        assert_eq!(start.elapsed(), ms(1500));
    }

    /// Each attempt starts after its own wait: 0.5, then 1, 2 and 4 s after
    /// the one before. Swapping any two waits changes one of these offsets.
    #[tokio::test(start_paused = true)]
    async fn always_unavailable_is_five_calls_started_at_the_scheduled_offsets() {
        let script = Script::new(
            (0..5)
                .map(|_| Err(Status::unavailable(IN_PROGRESS)))
                .collect(),
        );
        let start = Instant::now();
        let err = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Unavailable);
        assert_eq!(err.message(), IN_PROGRESS);
        assert_eq!(script.calls(), 5);
        assert_eq!(
            script.started(),
            vec![ms(0), ms(500), ms(1500), ms(3500), ms(7500)]
        );
        // No wait after the last attempt.
        assert_eq!(start.elapsed(), ms(7500));
    }

    /// Calls that each take 25 s: attempts start at 0 and 25.5 s and 51.5 s;
    /// the next would start at 78.5 s, past the 60 s bound, so it does not and
    /// the third refusal is returned.
    #[tokio::test(start_paused = true)]
    async fn no_attempt_starts_more_than_sixty_seconds_after_the_first() {
        let mut script = Script::new(
            (0..5)
                .map(|_| Err(Status::unavailable(IN_PROGRESS)))
                .collect(),
        );
        script.takes = Duration::from_secs(25);
        let err = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
            .await
            .unwrap_err();
        assert_eq!(err.message(), IN_PROGRESS);
        assert_eq!(
            script.started(),
            vec![ms(0), ms(25_500), ms(51_500)],
            "the fourth attempt would have started at 78.5 s"
        );
    }

    /// A 50 s call, then one wait: the second attempt would start at 50 s plus
    /// the wait. Returns how many calls ran under a 60 s deadline.
    async fn calls_with_wait(wait: Duration) -> usize {
        let mut script = Script::new(
            (0..2)
                .map(|_| Err(Status::unavailable(IN_PROGRESS)))
                .collect(),
        );
        script.takes = Duration::from_secs(50);
        let waits = [wait];
        let schedule = RetrySchedule {
            waits: &waits,
            deadline: Duration::from_secs(60),
        };
        let _ = withdraw_with_retry(&schedule, || script.next()).await;
        script.calls()
    }

    /// The bound is on when an attempt STARTS and an attempt starting exactly
    /// at the deadline is allowed: starts at 59.999 s and 60 s run, 60.001 s
    /// does not.
    #[tokio::test(start_paused = true)]
    async fn the_sixty_second_bound_includes_an_attempt_starting_exactly_at_it() {
        assert_eq!(calls_with_wait(ms(9_999)).await, 2);
        assert_eq!(calls_with_wait(ms(10_000)).await, 2);
        assert_eq!(calls_with_wait(ms(10_001)).await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_precondition_is_returned_at_once() {
        let script = Script::new(vec![Err(Status::failed_precondition("insufficient"))]);
        let start = Instant::now();
        let err = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::FailedPrecondition);
        assert_eq!(script.calls(), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    /// Both shapes tonic gives an HTTP 429 are retried, whatever the code.
    #[tokio::test(start_paused = true)]
    async fn the_gateway_rate_limit_is_retried_in_both_shapes() {
        for status in [
            Status::unavailable(HTTP_429_INFERRED_MESSAGE),
            Status::internal(
                "protocol error: received message with invalid compression flag: 84 (valid flags \
                 are 0 and 1) while receiving response with status: 429 Too Many Requests",
            ),
        ] {
            let script = Script::new(vec![Err(status), Ok(voucher())]);
            let start = Instant::now();
            withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
                .await
                .unwrap();
            assert_eq!(script.calls(), 2);
            assert_eq!(start.elapsed(), ms(500));
        }
    }

    /// A dropped connection may have followed an issued voucher, and a repeat
    /// would issue a second one, so a transport-level UNAVAILABLE and a
    /// gateway 502/503/504 are not retried. A signer-paused refusal without
    /// the venue's not-issued words is not retried either.
    #[tokio::test(start_paused = true)]
    async fn unavailable_that_may_follow_an_issued_voucher_is_not_retried() {
        for message in [
            "transport error",
            "error reading a body from connection",
            "exits paused: signer unreachable",
            "grpc-status header missing, mapped from HTTP status code 504",
            "grpc-status header missing, mapped from HTTP status code 503",
        ] {
            let script = Script::new(vec![Err(Status::unavailable(message))]);
            let start = Instant::now();
            let err = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
                .await
                .unwrap_err();
            assert_eq!(err.message(), message);
            assert_eq!(script.calls(), 1, "{message}");
            assert_eq!(start.elapsed(), Duration::ZERO, "{message}");
        }
    }

    /// A signer-paused refusal that says the withdrawal was not issued is
    /// retried like any other such refusal.
    #[tokio::test(start_paused = true)]
    async fn exits_paused_saying_not_issued_is_retried() {
        let paused = "exits paused: signer locked; withdrawal not issued — retry shortly";
        assert!(super::refused_before_issue(&Status::unavailable(paused)));
        let script = Script::new(vec![Err(Status::unavailable(paused)), Ok(voucher())]);
        let start = Instant::now();
        let got = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next()).await;
        assert_eq!(got.unwrap().nonce, 7);
        assert_eq!(script.calls(), 2);
        assert_eq!(start.elapsed(), ms(500));
    }

    /// The gateway's 429 is recognised by the code and text tonic pairs
    /// together; either text under any other pairing is not a retry request.
    #[tokio::test(start_paused = true)]
    async fn the_429_text_under_another_code_is_not_retried() {
        let body_text = "protocol error: received message with invalid compression flag: 84 \
                         (valid flags are 0 and 1) while receiving response with status: 429 \
                         Too Many Requests";
        for status in [
            Status::new(Code::Unknown, body_text),
            Status::new(Code::Unknown, HTTP_429_INFERRED_MESSAGE),
            Status::new(Code::FailedPrecondition, body_text),
            Status::new(Code::Unavailable, body_text),
            Status::new(Code::Internal, HTTP_429_INFERRED_MESSAGE),
        ] {
            let script = Script::new(vec![Err(status.clone()), Ok(voucher())]);
            let err = withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
                .await
                .unwrap_err();
            assert_eq!(err.code(), status.code());
            assert_eq!(script.calls(), 1, "{:?}", status.code());
        }
    }

    /// The venue's words must come with UNAVAILABLE: the same text under
    /// another code is not a retry request.
    #[tokio::test(start_paused = true)]
    async fn the_marker_under_another_code_is_not_retried() {
        let script = Script::new(vec![Err(Status::internal(IN_PROGRESS))]);
        withdraw_with_retry(&RetrySchedule::WITHDRAW, || script.next())
            .await
            .unwrap_err();
        assert_eq!(script.calls(), 1);
    }
}

/// `fetch_grpc` against a raw HTTP/2 server that answers from a script, so
/// the status each answer becomes is the one tonic really produces.
#[cfg(test)]
mod fetch_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use bytes::Bytes;
    use prost::Message;
    use tokio::net::TcpListener;

    use super::super::arborter_pb::{WithdrawRequest, WithdrawResponse};
    use super::{RetrySchedule, fetch_grpc};

    const NOT_ISSUED: &str =
        "settlement is in progress on base; withdrawal not issued — retry shortly";

    /// What the fake server answers one request with.
    #[derive(Clone)]
    enum Answer {
        /// A plain HTTP response with this status and body, no gRPC trailers.
        Http(u16, &'static str),
        /// A gRPC error, trailers-only.
        Grpc(u8, &'static str),
        /// A voucher.
        Voucher,
        /// The request is read, then the connection is dropped with no status.
        Drop,
    }

    /// Answer request `n` with `script[n]`, the last entry once it runs out.
    /// Returns the URL and the count of requests served.
    async fn fake_venue(script: Vec<Answer>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let served = count.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let script = script.clone();
                let count = count.clone();
                tokio::spawn(async move {
                    let mut conn = h2::server::handshake(socket).await.unwrap();
                    while let Some(Ok((_request, mut respond))) = conn.accept().await {
                        let n = count.fetch_add(1, Ordering::SeqCst);
                        let answer = script[n.min(script.len() - 1)].clone();
                        match answer {
                            Answer::Http(status, body) => {
                                let response =
                                    http::Response::builder().status(status).body(()).unwrap();
                                let mut send =
                                    respond.send_response(response, body.is_empty()).unwrap();
                                if !body.is_empty() {
                                    send.send_data(Bytes::from_static(body.as_bytes()), true)
                                        .unwrap();
                                }
                            }
                            Answer::Grpc(code, message) => {
                                let response = http::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/grpc")
                                    .header("grpc-status", code.to_string())
                                    .header("grpc-message", message.replace(' ', "%20"))
                                    .body(())
                                    .unwrap();
                                respond.send_response(response, true).unwrap();
                            }
                            Answer::Drop => return,
                            Answer::Voucher => {
                                let response = http::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/grpc")
                                    .body(())
                                    .unwrap();
                                let mut send = respond.send_response(response, false).unwrap();
                                let body = WithdrawResponse {
                                    amount: "1000000".into(),
                                    nonce: 7,
                                    expiry: 1_900_000_000,
                                    signature: vec![0x11; 65],
                                    ..Default::default()
                                }
                                .encode_to_vec();
                                let mut frame = vec![0u8];
                                frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
                                frame.extend_from_slice(&body);
                                send.send_data(Bytes::from(frame), false).unwrap();
                                let mut trailers = http::HeaderMap::new();
                                trailers.insert("grpc-status", "0".parse().unwrap());
                                send.send_trailers(trailers).unwrap();
                            }
                        }
                    }
                });
            }
        });
        (url, served)
    }

    /// Short real waits: the production schedule is tested on the paused
    /// clock; here the wiring is.
    const FAST: RetrySchedule<'static> = RetrySchedule {
        waits: &[
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(5),
            Duration::from_millis(5),
        ],
        deadline: Duration::from_secs(30),
    };

    fn request() -> WithdrawRequest {
        WithdrawRequest {
            network: "base".into(),
            token: "0xtoken".into(),
            account: "0xaccount".into(),
            amount: "1000000".into(),
            signature: vec![1; 65],
        }
    }

    async fn run(script: Vec<Answer>) -> (eyre::Result<super::Voucher>, usize) {
        let (url, served) = fake_venue(script).await;
        let got = fetch_grpc(&url, request(), &FAST).await;
        (got, served.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_429_with_a_text_body_twice_then_a_voucher() {
        let (got, served) = run(vec![
            Answer::Http(429, "Too Many Requests"),
            Answer::Http(429, "Too Many Requests"),
            Answer::Voucher,
        ])
        .await;
        assert_eq!(got.unwrap().nonce, 7);
        assert_eq!(served, 3);
    }

    #[tokio::test]
    async fn a_429_with_an_empty_body_twice_then_a_voucher() {
        let (got, served) = run(vec![
            Answer::Http(429, ""),
            Answer::Http(429, ""),
            Answer::Voucher,
        ])
        .await;
        assert_eq!(got.unwrap().nonce, 7);
        assert_eq!(served, 3);
    }

    #[tokio::test]
    async fn a_venue_refusal_not_issued_then_a_voucher() {
        let (got, served) = run(vec![Answer::Grpc(14, NOT_ISSUED), Answer::Voucher]).await;
        assert_eq!(got.unwrap().nonce, 7);
        assert_eq!(served, 2);
    }

    #[tokio::test]
    async fn a_transport_like_unavailable_is_one_request() {
        let (got, served) = run(vec![
            Answer::Grpc(14, "error reading a body from connection"),
            Answer::Voucher,
        ])
        .await;
        let err = got.err().expect("the refusal is returned");
        let status = err.downcast_ref::<tonic::Status>().expect("a status");
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(served, 1);
    }

    #[tokio::test]
    async fn a_503_is_one_request() {
        let (got, served) = run(vec![Answer::Http(503, ""), Answer::Voucher]).await;
        assert!(got.is_err());
        assert_eq!(served, 1);
    }

    #[tokio::test]
    async fn a_connection_dropped_after_the_request_is_one_request() {
        let (got, served) = run(vec![Answer::Drop, Answer::Voucher]).await;
        assert!(got.is_err());
        assert_eq!(served, 1);
    }
}

#[cfg(test)]
mod submit_tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use alloy::primitives::B256;
    use eyre::Result;
    use tokio::time::Instant;

    use super::{
        EVM_VOUCHER_WATCH_TIMEOUT, WithdrawNonceAlreadyUsed, WithdrawOutcomeUnknown, receipt_paid,
        submit_evm_voucher,
    };

    const NONCE: u64 = 9;
    const EXPIRY: u64 = 1_900_000_000;
    const ATTEMPTS: usize = 3;

    fn h(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[derive(Clone, Copy)]
    enum Prepare {
        /// Answer from the script in order.
        Scripted,
        /// Never answer.
        Hangs,
    }

    #[derive(Clone, Copy)]
    enum Broadcast {
        Accepted,
        Fails,
        Hangs,
    }

    #[derive(Clone, Copy)]
    enum Watch {
        Confirms,
        Reverts,
        Hangs,
    }

    #[derive(Clone, Copy)]
    enum Read {
        Used,
        Unused,
        /// Unused on the first read, used on every later one.
        UsedFromSecond,
        Fails,
        Hangs,
    }

    /// A chain that answers each step as told, and records the calls.
    struct Chain {
        prepare: Prepare,
        prepared: Mutex<Vec<Result<B256, String>>>,
        prepares: AtomicUsize,
        broadcast: Broadcast,
        broadcasts: Mutex<Vec<B256>>,
        watch: Watch,
        watched: Mutex<Vec<B256>>,
        /// The read made before the first attempt.
        pre_read: Read,
        /// Every read, the one before the first attempt included.
        read_calls: AtomicUsize,
        /// The reads made after attempts; `UsedFromSecond` counts these.
        read: Read,
        reads: AtomicUsize,
    }

    impl Chain {
        fn new(
            mut prepared: Vec<Result<B256, String>>,
            broadcast: Broadcast,
            watch: Watch,
            read: Read,
        ) -> Self {
            prepared.reverse();
            Self {
                prepare: Prepare::Scripted,
                prepared: Mutex::new(prepared),
                prepares: AtomicUsize::new(0),
                broadcast,
                broadcasts: Mutex::new(Vec::new()),
                watch,
                watched: Mutex::new(Vec::new()),
                pre_read: Read::Unused,
                read_calls: AtomicUsize::new(0),
                read,
                reads: AtomicUsize::new(0),
            }
        }

        /// The "signed transaction" handed to `broadcast` is its hash.
        async fn prepare(&self) -> Result<(B256, B256)> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            if let Prepare::Hangs = self.prepare {
                std::future::pending::<()>().await;
            }
            let next = self
                .prepared
                .lock()
                .unwrap()
                .pop()
                .expect("prepare script ran out");
            let hash = next.map_err(|e| eyre::eyre!(e))?;
            Ok((hash, hash))
        }

        async fn broadcast(&self, signed: B256) -> Result<()> {
            self.broadcasts.lock().unwrap().push(signed);
            match self.broadcast {
                Broadcast::Accepted => Ok(()),
                Broadcast::Fails => Err(eyre::eyre!("connection reset after the request")),
                Broadcast::Hangs => std::future::pending().await,
            }
        }

        async fn watch(&self, hash: B256) -> Result<()> {
            self.watched.lock().unwrap().push(hash);
            match self.watch {
                Watch::Confirms => Ok(()),
                Watch::Reverts => receipt_paid(hash, false),
                Watch::Hangs => std::future::pending().await,
            }
        }

        async fn nonce_used(&self) -> Result<bool> {
            // The first read is the one before any attempt.
            let (mode, read) = if self.read_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                (self.pre_read, 0)
            } else {
                (self.read, self.reads.fetch_add(1, Ordering::SeqCst) + 1)
            };
            match mode {
                Read::Used => Ok(true),
                Read::Unused => Ok(false),
                Read::UsedFromSecond => Ok(read >= 2),
                Read::Fails => Err(eyre::eyre!("read refused")),
                Read::Hangs => std::future::pending().await,
            }
        }

        async fn submit(&self) -> Result<Option<B256>> {
            submit_evm_voucher(
                NONCE,
                EXPIRY,
                || self.prepare(),
                |signed| self.broadcast(signed),
                |hash| self.watch(hash),
                || self.nonce_used(),
                ATTEMPTS,
            )
            .await
        }

        fn prepares(&self) -> usize {
            self.prepares.load(Ordering::SeqCst)
        }

        fn broadcasts(&self) -> Vec<B256> {
            self.broadcasts.lock().unwrap().clone()
        }

        fn watched(&self) -> Vec<B256> {
            self.watched.lock().unwrap().clone()
        }

        /// Reads after attempts, not counting the one before the first.
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        /// Every read, the one before the first attempt included.
        fn all_reads(&self) -> usize {
            self.read_calls.load(Ordering::SeqCst)
        }
    }

    fn three() -> Vec<Result<B256, String>> {
        vec![Ok(h(0xaa)), Ok(h(0xbb)), Ok(h(0xcc))]
    }

    fn outcome_unknown(err: &eyre::Report) -> &WithdrawOutcomeUnknown {
        err.downcast_ref::<WithdrawOutcomeUnknown>()
            .unwrap_or_else(|| panic!("not an unknown outcome: {err}"))
    }

    fn unknown_cc() -> WithdrawOutcomeUnknown {
        WithdrawOutcomeUnknown {
            voucher_nonce: 9,
            voucher_expiry: 1_900_000_000,
            tx_hash: Some(h(0xcc)),
        }
    }

    #[test]
    fn the_watch_timeout_is_a_minute() {
        assert_eq!(EVM_VOUCHER_WATCH_TIMEOUT, Duration::from_secs(60));
    }

    /// A reverted receipt paid nothing; a successful one is the payment.
    #[test]
    fn a_reverted_receipt_is_an_error_naming_the_transaction() {
        assert!(receipt_paid(h(0xaa), true).is_ok());
        assert_eq!(
            receipt_paid(h(0xbb), false).unwrap_err().to_string(),
            "withdraw transaction \
             0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb reverted"
        );
    }

    /// A confirmed transaction is the answer; the nonce is not asked about.
    #[tokio::test(start_paused = true)]
    async fn confirmed_is_its_hash_without_reading_the_nonce() {
        let chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Confirms,
            Read::Unused,
        );
        let start = Instant::now();
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(chain.broadcasts(), vec![h(0xaa)]);
        assert_eq!(chain.watched(), vec![h(0xaa)]);
        assert_eq!(chain.reads(), 0);
        assert_eq!(chain.all_reads(), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    /// The watch gives up at a minute; the contract says the voucher was
    /// paid, so that is the answer and nothing is sent again.
    #[tokio::test(start_paused = true)]
    async fn unconfirmed_but_paid_is_its_hash_at_a_minute() {
        let chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Hangs,
            Read::Used,
        );
        let start = Instant::now();
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(chain.reads(), 1);
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }

    /// Three broadcasts, none confirmed and the nonce still unused after
    /// each: any of them may yet land, so the outcome is unknown and names
    /// the last transaction. 3 x 60 s of watching plus two 0.7 s waits.
    #[tokio::test(start_paused = true)]
    async fn unconfirmed_and_unpaid_three_times_is_outcome_unknown() {
        let chain = Chain::new(three(), Broadcast::Accepted, Watch::Hangs, Read::Unused);
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(
            err.to_string(),
            "withdrawal submitted, outcome unknown: voucher nonce 9, transaction \
             0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc, \
             expiry 1900000000; check the chain before asking again: if the nonce \
             is still unused once a block's timestamp is past the expiry, the \
             voucher was not paid and never will be"
        );
        assert_eq!(chain.broadcasts(), vec![h(0xaa), h(0xbb), h(0xcc)]);
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(181_400));
    }

    /// A transaction that failed before it was broadcast, with the voucher
    /// already paid (by an earlier transaction of this voucher that nobody
    /// here saw): success, with no hash to report.
    #[tokio::test(start_paused = true)]
    async fn failed_before_broadcast_but_paid_is_success_without_a_hash() {
        let chain = Chain::new(
            vec![Err("nonce too low".into())],
            Broadcast::Accepted,
            Watch::Hangs,
            Read::Used,
        );
        assert_eq!(chain.submit().await.unwrap(), None);
        assert_eq!(chain.prepares(), 1);
        assert_eq!(chain.reads(), 1);
        assert!(chain.broadcasts().is_empty());
        assert!(chain.watched().is_empty());
    }

    /// Nothing was ever broadcast and the voucher is unpaid: the last error,
    /// not an unknown outcome.
    #[tokio::test(start_paused = true)]
    async fn failed_before_broadcast_three_times_unpaid_is_the_last_error() {
        let chain = Chain::new(
            vec![
                Err("revert one".into()),
                Err("revert two".into()),
                Err("revert three".into()),
            ],
            Broadcast::Accepted,
            Watch::Hangs,
            Read::Unused,
        );
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert!(err.downcast_ref::<WithdrawOutcomeUnknown>().is_none());
        assert_eq!(err.to_string(), "revert three");
        assert_eq!(chain.prepares(), 3);
        assert!(chain.broadcasts().is_empty());
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(1_400));
    }

    /// The nonce cannot be read after any of three unconfirmed broadcasts:
    /// the outcome is unknown and names the last transaction.
    #[tokio::test(start_paused = true)]
    async fn unconfirmed_and_unreadable_three_times_is_outcome_unknown() {
        let chain = Chain::new(three(), Broadcast::Accepted, Watch::Hangs, Read::Fails);
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(chain.prepares(), 3);
        assert_eq!(chain.reads(), 3);
    }

    /// An earlier unconfirmed broadcast, then a failure before broadcasting
    /// with the voucher paid: the earlier transaction is the one reported.
    #[tokio::test(start_paused = true)]
    async fn paid_after_a_failed_attempt_reports_the_earlier_transaction() {
        let chain = Chain::new(
            vec![Ok(h(0xaa)), Err("nonce too low".into())],
            Broadcast::Accepted,
            Watch::Hangs,
            Read::UsedFromSecond,
        );
        let start = Instant::now();
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 2);
        assert_eq!(chain.reads(), 2);
        assert_eq!(start.elapsed(), ms(60_700));
    }

    /// A broadcast that errors may still have reached a node, which can mine
    /// the transaction: unpaid three times, the outcome is unknown and names
    /// the last transaction, never a plain failure.
    #[tokio::test(start_paused = true)]
    async fn broadcast_failing_three_times_unpaid_is_outcome_unknown() {
        let chain = Chain::new(three(), Broadcast::Fails, Watch::Confirms, Read::Unused);
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(chain.broadcasts(), vec![h(0xaa), h(0xbb), h(0xcc)]);
        assert!(chain.watched().is_empty());
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(1_400));
    }

    /// A broadcast that errors, with the voucher then found paid: success,
    /// naming that transaction.
    #[tokio::test(start_paused = true)]
    async fn broadcast_failing_but_paid_is_its_hash() {
        let chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Fails,
            Watch::Confirms,
            Read::Used,
        );
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(chain.reads(), 1);
    }

    /// A broadcast that never answers is bounded like the watch and, unpaid
    /// three times, ends as an unknown outcome naming the last transaction.
    #[tokio::test(start_paused = true)]
    async fn broadcast_that_never_answers_is_outcome_unknown() {
        let chain = Chain::new(three(), Broadcast::Hangs, Watch::Confirms, Read::Unused);
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(181_400));
    }

    /// Filling and signing that never answers is bounded too, and since
    /// nothing was broadcast the result is a plain error.
    #[tokio::test(start_paused = true)]
    async fn preparing_that_never_answers_is_a_plain_error() {
        let mut chain = Chain::new(vec![], Broadcast::Accepted, Watch::Confirms, Read::Unused);
        chain.prepare = Prepare::Hangs;
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert!(err.downcast_ref::<WithdrawOutcomeUnknown>().is_none());
        assert_eq!(
            err.to_string(),
            "withdraw transaction not confirmed within 60s"
        );
        assert_eq!(chain.prepares(), 3);
        assert!(chain.broadcasts().is_empty());
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(181_400));
    }

    /// A transaction that confirms reverted paid nothing: with the nonce
    /// unused each time the same voucher is sent again, and three reverts
    /// end as an unknown outcome naming the last transaction.
    #[tokio::test(start_paused = true)]
    async fn reverted_three_times_unpaid_is_outcome_unknown() {
        let chain = Chain::new(three(), Broadcast::Accepted, Watch::Reverts, Read::Unused);
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(chain.watched(), vec![h(0xaa), h(0xbb), h(0xcc)]);
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(1_400));
    }

    /// A revert, then the nonce found used (the voucher was paid by another
    /// transaction): success, naming the last transaction seen.
    #[tokio::test(start_paused = true)]
    async fn reverted_but_paid_is_success() {
        let chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Reverts,
            Read::Used,
        );
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(chain.reads(), 1);
    }

    /// A nonce read that never answers is bounded at 10 s and counts as
    /// unanswered: 3 x (60 s + 10 s) plus two 0.7 s waits.
    #[tokio::test(start_paused = true)]
    async fn a_nonce_read_that_never_answers_is_bounded() {
        let chain = Chain::new(three(), Broadcast::Accepted, Watch::Hangs, Read::Hangs);
        let start = Instant::now();
        let err = chain.submit().await.unwrap_err();
        assert_eq!(*outcome_unknown(&err), unknown_cc());
        assert_eq!(chain.reads(), 3);
        assert_eq!(start.elapsed(), ms(211_400));
    }

    /// The contract already shows the voucher's nonce used before anything
    /// is sent: an error of its own, which says it may have been paid, and
    /// nothing is sent.
    #[tokio::test(start_paused = true)]
    async fn nonce_already_used_before_the_first_attempt_is_an_error_and_sends_nothing() {
        let mut chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Confirms,
            Read::Unused,
        );
        chain.pre_read = Read::Used;
        let err = chain.submit().await.unwrap_err();
        assert_eq!(
            err.downcast_ref::<WithdrawNonceAlreadyUsed>(),
            Some(&WithdrawNonceAlreadyUsed {
                voucher_nonce: 9,
                voucher_expiry: 1_900_000_000,
            })
        );
        assert_eq!(
            err.to_string(),
            "the contract already shows withdrawal voucher nonce 9 used before this call \
             submitted anything: either this voucher (expiry 1900000000) was already paid \
             to the account by another submitter, or the venue issued a nonce that an \
             earlier voucher used; check the chain for a Withdrawn event for this account \
             and nonce (its token and amount say which) before withdrawing again"
        );
        assert_eq!(chain.prepares(), 0);
        assert!(chain.broadcasts().is_empty());
        assert_eq!(chain.all_reads(), 1);
    }

    /// A read before the first attempt that fails does not block the
    /// withdrawal.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_read_before_the_first_attempt_proceeds() {
        let mut chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Confirms,
            Read::Unused,
        );
        chain.pre_read = Read::Fails;
        let start = Instant::now();
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    /// A read before the first attempt that never answers is bounded at 10 s
    /// and then the withdrawal proceeds.
    #[tokio::test(start_paused = true)]
    async fn a_hung_read_before_the_first_attempt_proceeds_after_ten_seconds() {
        let mut chain = Chain::new(
            vec![Ok(h(0xaa))],
            Broadcast::Accepted,
            Watch::Confirms,
            Read::Unused,
        );
        chain.pre_read = Read::Hangs;
        let start = Instant::now();
        assert_eq!(chain.submit().await.unwrap(), Some(h(0xaa)));
        assert_eq!(chain.prepares(), 1);
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }
}

#[cfg(test)]
mod signing_provider_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy::network::{EthereumWallet, TransactionBuilder};
    use alloy::primitives::{Address, Bytes};
    use alloy::rpc::types::TransactionRequest;
    use alloy::signers::local::PrivateKeySigner;
    use serde_json::{Value, json};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::method};

    use super::signing_provider;

    /// The account's pending transaction count on the mock node.
    const PENDING: u64 = 5;

    /// A JSON-RPC node that knows just enough to fill a transaction; it
    /// counts the nonce reads.
    struct Node {
        nonce_reads: Arc<AtomicUsize>,
    }

    impl Respond for Node {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let result = match body["method"].as_str().unwrap() {
                "eth_chainId" => json!("0x7a69"),
                "eth_getTransactionCount" => {
                    self.nonce_reads.fetch_add(1, Ordering::SeqCst);
                    json!(format!("{PENDING:#x}"))
                }
                "eth_estimateGas" => json!("0x30d40"),
                "eth_feeHistory" => json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]]
                }),
                other => panic!("mock node has no answer for {other}"),
            };
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        }
    }

    /// Two fills with nothing broadcast in between both take the node's
    /// pending count: the nonce is read each time, never advanced locally, so
    /// a fill that is not broadcast (a gas estimate that reverts, a broadcast
    /// that never reached the node) leaves no gap for the next one.
    #[tokio::test]
    async fn every_fill_reads_the_pending_nonce_from_the_node() {
        let server = MockServer::start().await;
        let nonce_reads = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .respond_with(Node {
                nonce_reads: nonce_reads.clone(),
            })
            .mount(&server)
            .await;
        let wallet = EthereumWallet::new(PrivateKeySigner::random());
        let signing = signing_provider(wallet, server.uri().parse().unwrap());
        let request = TransactionRequest::default()
            .with_to(Address::repeat_byte(0x42))
            .with_input(Bytes::from_static(&[0xde, 0xad]));

        let mut nonces = Vec::new();
        for _ in 0..2 {
            let envelope = signing
                .fill(request.clone())
                .await
                .unwrap()
                .try_into_envelope()
                .unwrap();
            nonces.push(alloy::consensus::Transaction::nonce(&envelope));
        }
        assert_eq!(nonces, vec![5, 5]);
        assert_eq!(nonce_reads.load(Ordering::SeqCst), 2);
    }
}
