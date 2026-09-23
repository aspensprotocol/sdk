//! Operator-authority on-chain actions, signed DIRECTLY by an offline
//! operator key.
//!
//! Everything else under `commands` reaches the chain through the arborter:
//! the caller sends gRPC, the arborter builds the transaction and signs it
//! with the key sealed inside the TEE. That shape is wrong for the controls
//! whose entire purpose is to bound what a compromised or buggy TEE can do —
//! routing them through the arborter would mean the TEE could raise its own
//! ceiling, which is exactly the case where the ceiling is worthless.
//!
//! So the flows here never involve the arborter's signer. They read stack
//! metadata (chain RPC URL, program id, instance PDA, mint address) from the
//! config the stack publishes, then build, sign and submit the transaction
//! locally with a key the operator holds.

use eyre::{Result, eyre};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::chain_client::ARCH_SOLANA;
use crate::commands::config::config_pb::GetConfigResponse;
use crate::solana::client::{SettleEpochState, WithdrawEpochState};
use crate::wallet::Wallet;

/// Minimum lamports the operator-admin wallet needs before we submit: the
/// transaction fee plus rent for the `WithdrawEpoch` / `SettleEpoch` PDA, which
/// the cap instructions create (`init_if_needed`, `payer = operator_admin`) when
/// the account does not exist yet.
const MIN_SOL_LAMPORTS: u64 = 3_000_000;

/// What `set_withdraw_epoch_cap` did, for the caller to report.
#[derive(Debug, Clone)]
pub struct SetWithdrawEpochCapOutcome {
    /// Confirmed transaction signature.
    pub signature: String,
    /// Midrib program the instruction went to.
    pub program_id: Pubkey,
    /// Trading instance whose cap was changed.
    pub instance: Pubkey,
    /// Mint the cap applies to.
    pub mint: Pubkey,
    /// The `WithdrawEpoch` PDA holding the cap.
    pub withdraw_epoch: Pubkey,
    /// Rate-limit state read back after confirmation.
    pub state: Option<WithdrawEpochState>,
    /// `true` when the instance's `operator_admin` is also its TEE `signer`.
    /// The program refuses that pairing at `create_instance` and
    /// `set_operator_admin`, so only an instance created before that check can
    /// carry it; there the cap bounds bugs but NOT a compromised TEE.
    pub admin_is_tee_signer: bool,
}

/// What `set_settle_epoch_cap` did, for the caller to report.
#[derive(Debug, Clone)]
pub struct SetSettleEpochCapOutcome {
    /// Confirmed transaction signature.
    pub signature: String,
    /// Midrib program the instruction went to.
    pub program_id: Pubkey,
    /// Trading instance whose cap was changed.
    pub instance: Pubkey,
    /// Mint the cap applies to.
    pub mint: Pubkey,
    /// The `SettleEpoch` PDA holding the cap.
    pub settle_epoch: Pubkey,
    /// Rate-limit state read back after confirmation.
    pub state: Option<SettleEpochState>,
    /// See [`SetWithdrawEpochCapOutcome::admin_is_tee_signer`].
    pub admin_is_tee_signer: bool,
}

/// Arm (or disarm) the per-`(instance, mint)` per-epoch withdrawal cap on a
/// Solana Midrib instance, signing with `wallet` and submitting to the
/// cluster directly.
///
/// `cap` is in the mint's BASE units — the same scale the program accumulates
/// withdrawals in — and `0` means unlimited (the shipped default, matching
/// EVM's `MidribV3.setWithdrawEpochCap`).
///
/// The window is TUMBLING, not sliding: the program tracks `slot /
/// WITHDRAW_EPOCH_SLOTS` and resets the running total on rollover, so a
/// withdrawal of `cap` just before a boundary and another just after puts `2 *
/// cap` out inside one hour. To guarantee at most X per hour, set `cap = X/2`.
/// EVM has the identical property.
///
/// `wallet` must hold the key equal to the instance's on-chain
/// `operator_admin`; that is checked against the chain before anything is
/// submitted, because the program's rejection is an opaque `Unauthorized`.
pub async fn set_withdraw_epoch_cap(
    config: &GetConfigResponse,
    network: &str,
    token_symbol: &str,
    cap: u64,
    wallet: &Wallet,
) -> Result<SetWithdrawEpochCapOutcome> {
    let ctx = CapContext::prepare(
        config,
        network,
        token_symbol,
        wallet,
        "withdrawal",
        "`MidribV3.setWithdrawEpochCap(token, cap)`, armed with infra's \
         `just set-withdraw-cap` signing as the operator admin",
    )
    .await?;
    let (withdraw_epoch, _) =
        crate::solana::derive_withdraw_epoch_pda(&ctx.instance, &ctx.mint, &ctx.program_id);

    let before = crate::solana::client::fetch_withdraw_epoch(
        &ctx.rpc_url,
        &ctx.instance,
        &ctx.mint,
        &ctx.program_id,
    )
    .await
    .unwrap_or(None);
    match before {
        Some(state) => tracing::info!(
            "current withdrawal cap for {token_symbol}: {} (epoch {}, {} base units \
             already withdrawn this epoch)",
            state.cap,
            state.epoch,
            state.withdrawn
        ),
        None => tracing::info!(
            "no WithdrawEpoch account yet for {token_symbol} — cap is unlimited; this \
             call creates the account (rent paid by {})",
            ctx.operator_admin
        ),
    }

    let ix = crate::solana::set_withdraw_epoch_cap_ix(
        &ctx.program_id,
        &ctx.instance,
        &ctx.mint,
        &ctx.operator_admin,
        cap,
    )?;
    let signature =
        crate::solana::client::submit_user_signed(&ctx.rpc_url, ctx.keypair, ix).await?;

    // Read back rather than echoing the requested value: the point of this
    // command is that the ceiling is really in place.
    let state = crate::solana::client::fetch_withdraw_epoch(
        &ctx.rpc_url,
        &ctx.instance,
        &ctx.mint,
        &ctx.program_id,
    )
    .await
    .unwrap_or(None);

    Ok(SetWithdrawEpochCapOutcome {
        signature,
        program_id: ctx.program_id,
        instance: ctx.instance,
        mint: ctx.mint,
        withdraw_epoch,
        state,
        admin_is_tee_signer: ctx.admin_is_tee_signer,
    })
}

/// Arm (or disarm) the per-`(instance, mint)` per-epoch SETTLEMENT cap on a
/// Solana Midrib instance: the most `settle_batch` may credit to accounts (the
/// sum of its positive deltas) in that mint per epoch, across all batches.
/// Signed with `wallet` and submitted directly, like
/// [`set_withdraw_epoch_cap`].
///
/// Without it one TEE-signed batch could reassign every balance of a mint to a
/// single account; the withdrawal cap only bounds vouchers. `cap` is in the
/// mint's BASE units and `0` means unlimited (matching EVM's
/// `MidribV3.setSettleEpochCap`). Same tumbling ~1h window as the withdrawal
/// cap. The on-chain tally runs while uncapped, so a cap armed mid-epoch counts
/// what already settled; the program writes only `cap`, never the tally.
///
/// Size it above the venue's legitimate hourly settled volume in the mint: a
/// batch that would exceed it is refused on chain, and the arborter holds that
/// mint's settlement until it fits (the epoch rolls over or the cap is raised).
pub async fn set_settle_epoch_cap(
    config: &GetConfigResponse,
    network: &str,
    token_symbol: &str,
    cap: u64,
    wallet: &Wallet,
) -> Result<SetSettleEpochCapOutcome> {
    let ctx = CapContext::prepare(
        config,
        network,
        token_symbol,
        wallet,
        "settlement",
        "`MidribV3.setSettleEpochCap(token, cap)` (read back with \
         `settleEpochCap(token)`), called with the operator-admin key",
    )
    .await?;
    let (settle_epoch, _) =
        crate::solana::derive_settle_epoch_pda(&ctx.instance, &ctx.mint, &ctx.program_id);

    let before = crate::solana::client::fetch_settle_epoch(
        &ctx.rpc_url,
        &ctx.instance,
        &ctx.mint,
        &ctx.program_id,
    )
    .await
    .unwrap_or(None);
    match before {
        Some(state) => tracing::info!(
            "current settlement cap for {token_symbol}: {} (epoch {}, {} base units \
             already credited this epoch)",
            state.cap,
            state.epoch,
            state.settled
        ),
        None => tracing::info!(
            "no SettleEpoch account yet for {token_symbol} — cap is unlimited; this \
             call creates the account (rent paid by {})",
            ctx.operator_admin
        ),
    }

    let ix = crate::solana::set_settle_epoch_cap_ix(
        &ctx.program_id,
        &ctx.instance,
        &ctx.mint,
        &ctx.operator_admin,
        cap,
    )?;
    let signature =
        crate::solana::client::submit_user_signed(&ctx.rpc_url, ctx.keypair, ix).await?;

    let state = crate::solana::client::fetch_settle_epoch(
        &ctx.rpc_url,
        &ctx.instance,
        &ctx.mint,
        &ctx.program_id,
    )
    .await
    .unwrap_or(None);

    Ok(SetSettleEpochCapOutcome {
        signature,
        program_id: ctx.program_id,
        instance: ctx.instance,
        mint: ctx.mint,
        settle_epoch,
        state,
        admin_is_tee_signer: ctx.admin_is_tee_signer,
    })
}

/// Everything an operator-admin cap instruction needs, resolved and
/// pre-flighted: the chain is Solana, the token exists, the wallet IS the
/// instance's `operator_admin`, and it can pay the fee plus PDA rent.
struct CapContext<'a> {
    keypair: &'a solana_sdk::signature::Keypair,
    operator_admin: Pubkey,
    program_id: Pubkey,
    instance: Pubkey,
    mint: Pubkey,
    rpc_url: String,
    admin_is_tee_signer: bool,
}

impl<'a> CapContext<'a> {
    async fn prepare(
        config: &GetConfigResponse,
        network: &str,
        token_symbol: &str,
        wallet: &'a Wallet,
        what: &str,
        evm_equivalent: &str,
    ) -> Result<Self> {
        let chain = config
            .get_chain(network)
            .ok_or_else(|| eyre!("Chain '{}' not found in server configuration", network))?;
        if !chain.architecture.eq_ignore_ascii_case(ARCH_SOLANA) {
            return Err(eyre!(
                "chain '{}' has architecture '{}' — the per-epoch {} cap command \
                 targets Solana instances only. On EVM the equivalent is {}; it is \
                 gated to `operatorAdmin()`, the same stack-admin authority as \
                 Solana's `operator_admin`.",
                network,
                chain.architecture,
                what,
                evm_equivalent
            ));
        }
        let token = chain.tokens.get(token_symbol).ok_or_else(|| {
            eyre!(
                "Token '{}' not found on Solana chain '{}'. Run `config` to see \
                 available tokens.",
                token_symbol,
                network
            )
        })?;

        let keypair = wallet.as_solana().ok_or_else(|| {
            eyre!(
                "Solana chain '{}' requires an Ed25519 operator-admin keypair ({})",
                network,
                crate::wallet::OPERATOR_ADMIN_PRIVKEY_SOLANA_ENV
            )
        })?;
        let operator_admin = solana_sdk::signer::Signer::pubkey(keypair);

        let (program_id, instance) = crate::solana::client::resolve_program_and_instance(chain)?;
        let mint = Pubkey::from_str(&token.address)
            .map_err(|e| eyre!("invalid Solana mint '{}': {}", token.address, e))?;
        let rpc_url = crate::chain_client::chain_rpc_url(chain)?;

        // Pre-flight 1: the signer must BE the instance's operator_admin. The
        // program's check fails with a bare `Unauthorized` custom error, which
        // reads as an RPC failure rather than "you used the wrong key".
        let authorities =
            crate::solana::client::fetch_instance_authorities(&rpc_url, &instance).await?;
        if authorities.operator_admin != operator_admin {
            return Err(eyre!(
                "{} holds {}, but instance {} has operator_admin {} — the program \
                 would reject this with `Unauthorized`. Load the instance's \
                 operator-admin keypair.",
                crate::wallet::OPERATOR_ADMIN_PRIVKEY_SOLANA_ENV,
                operator_admin,
                instance,
                authorities.operator_admin
            ));
        }
        // The program refuses this pairing at `create_instance` and
        // `set_operator_admin`, so seeing it means an instance created before
        // that check.
        let admin_is_tee_signer = authorities.operator_admin == authorities.signer;
        if admin_is_tee_signer {
            tracing::warn!(
                instance = %instance,
                "instance operator_admin == its TEE signer ({}) — an instance created \
                 before the program refused that pairing. The cap you are arming \
                 bounds bugs and operator error, but a compromised signer can raise \
                 it again. Rotate to a distinct operator-admin key \
                 (`set_operator_admin`).",
                authorities.signer
            );
        }

        // Pre-flight 2: fees + PDA rent. The cap instructions are
        // `init_if_needed` with `payer = operator_admin`.
        {
            use solana_client::nonblocking::rpc_client::RpcClient;
            let rpc = RpcClient::new(rpc_url.clone());
            let lamports = rpc.get_balance(&operator_admin).await.unwrap_or(0);
            if lamports < MIN_SOL_LAMPORTS {
                return Err(eyre!(
                    "insufficient SOL: operator-admin {operator_admin} has {lamports} \
                     lamports, need >= {MIN_SOL_LAMPORTS} to cover the fee plus rent \
                     for the epoch-cap PDA this instruction may create."
                ));
            }
        }

        Ok(Self {
            keypair,
            operator_admin,
            program_id,
            instance,
            mint,
            rpc_url,
            admin_is_tee_signer,
        })
    }
}
