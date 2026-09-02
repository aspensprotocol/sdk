//! Stateful Solana helpers — RPC submission and on-chain state reads.
//!
//! Gated behind the `client` feature. Pure instruction builders and PDA
//! derivations live in the parent `solana` module and are available to any
//! consumer that enables the `solana` feature alone.

use eyre::{Result, eyre};
use solana_sdk::{
    instruction::Instruction, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use std::str::FromStr;

use crate::commands::config::config_pb::Chain;
use crate::solana::derive_user_balance_pda;

/// Resolve `(program_id, instance)` from a chain config entry. Both must be
/// configured for trade-program instructions to be built.
///
/// Lives here rather than in the pure module because it reads from the
/// proto-generated `Chain` struct — a `client`-feature type.
pub fn resolve_program_and_instance(chain: &Chain) -> Result<(Pubkey, Pubkey)> {
    if chain.factory_address.is_empty() {
        return Err(eyre!(
            "chain {} has no factory_address (the Solana program id)",
            chain.network
        ));
    }
    let program_id = Pubkey::from_str(&chain.factory_address).map_err(|e| {
        eyre!(
            "invalid Solana program id '{}': {}",
            chain.factory_address,
            e
        )
    })?;

    let instance_str = chain
        .trade_contract
        .as_ref()
        .map(|tc| tc.address.clone())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            eyre!(
                "Solana chain '{}' has no trade_contract.address (instance PDA) configured",
                chain.network
            )
        })?;
    let instance = Pubkey::from_str(&instance_str)
        .map_err(|e| eyre!("invalid Solana instance address '{}': {}", instance_str, e))?;

    Ok((program_id, instance))
}

/// Submit a single Midrib instruction signed by `user_keypair`. Fetches a
/// recent blockhash, builds and signs the transaction, then awaits
/// confirmation.
pub async fn submit_user_signed(
    rpc_url: &str,
    user_keypair: &Keypair,
    ix: Instruction,
) -> Result<String> {
    submit_user_signed_multi(rpc_url, user_keypair, &[ix]).await
}

/// Like [`submit_user_signed`] but for a multi-instruction transaction (e.g. an
/// Ed25519Program verify ix paired with the Midrib ix that introspects it, as in
/// the withdrawal-voucher flow). `user_keypair` is the sole signer + fee payer.
pub async fn submit_user_signed_multi(
    rpc_url: &str,
    user_keypair: &Keypair,
    ixs: &[Instruction],
) -> Result<String> {
    use solana_client::nonblocking::rpc_client::RpcClient;
    let client = RpcClient::new(rpc_url.to_string());
    let blockhash = client
        .get_latest_blockhash()
        .await
        .map_err(|e| eyre!("get_latest_blockhash: {}", e))?;
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&user_keypair.pubkey()),
        &[user_keypair],
        blockhash,
    );
    let sig = client
        .send_and_confirm_transaction(&tx)
        .await
        .map_err(|e| eyre!("send_and_confirm_transaction: {}", e))?;
    Ok(sig.to_string())
}

/// The two authority keys carried on a `TradingInstance` account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceAuthorities {
    /// The TEE/arborter key that authorizes settlement batches and withdrawal
    /// vouchers.
    pub signer: Pubkey,
    /// The stack-admin key gating `set_operator_fee` / `set_operator_admin` /
    /// `set_withdraw_epoch_cap`. Equal to `signer` in the default deploy shape,
    /// in which case the withdrawal cap contains bugs but not a compromised TEE.
    pub operator_admin: Pubkey,
}

/// Read the `signer` + `operator_admin` keys off a `TradingInstance` account.
///
/// Used as a pre-flight before submitting an operator-admin instruction: the
/// program rejects a wrong signer with a bare `Unauthorized` custom error, so
/// checking here turns that into an actionable message (and costs no fee).
pub async fn fetch_instance_authorities(
    rpc_url: &str,
    instance: &Pubkey,
) -> Result<InstanceAuthorities> {
    use solana_client::nonblocking::rpc_client::RpcClient;
    let client = RpcClient::new(rpc_url.to_string());
    let acc = client
        .get_account(instance)
        .await
        .map_err(|e| eyre!("get_account (TradingInstance {}): {}", instance, e))?;

    // Layout (after the 8-byte Anchor discriminator), borsh-packed:
    //   factory(32) signer(32) maintenance_address(32) maintenance_bps(u16)
    //   operator_address(32) operator_bps(u16) operator_admin(32) bump(u8)
    //   instance_id(u64)
    const SIGNER_OFFSET: usize = 8 + 32;
    const OPERATOR_ADMIN_OFFSET: usize = 8 + 32 + 32 + 32 + 2 + 32 + 2;
    if acc.data.len() < OPERATOR_ADMIN_OFFSET + 32 {
        return Err(eyre!(
            "TradingInstance account {} too small: {} bytes",
            instance,
            acc.data.len()
        ));
    }
    let read = |offset: usize| -> Result<Pubkey> {
        let bytes: [u8; 32] = acc.data[offset..offset + 32]
            .try_into()
            .map_err(|_| eyre!("TradingInstance account data layout error at {}", offset))?;
        Ok(Pubkey::new_from_array(bytes))
    };
    Ok(InstanceAuthorities {
        signer: read(SIGNER_OFFSET)?,
        operator_admin: read(OPERATOR_ADMIN_OFFSET)?,
    })
}

/// The per-`(instance, mint)` withdrawal rate-limit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawEpochState {
    /// Epoch index (`slot / 9_000`) the running total belongs to.
    pub epoch: u64,
    /// Base units already withdrawn within `epoch`.
    pub withdrawn: u64,
    /// The ceiling; `0` = unlimited.
    pub cap: u64,
}

/// Fetch the `WithdrawEpoch` PDA state for `(instance, mint)`. Returns `None`
/// if the account does not exist — the shipped state for every mint until a cap
/// is armed or the first voucher withdrawal lands, and equivalent to an
/// unlimited cap with a zero running total.
pub async fn fetch_withdraw_epoch(
    rpc_url: &str,
    instance: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> Result<Option<WithdrawEpochState>> {
    use solana_client::nonblocking::rpc_client::RpcClient;
    let client = RpcClient::new(rpc_url.to_string());
    let (pda, _) = crate::solana::derive_withdraw_epoch_pda(instance, mint, program_id);
    let response = client
        .get_account_with_commitment(&pda, client.commitment())
        .await
        .map_err(|e| eyre!("get_account (WithdrawEpoch PDA): {}", e))?;
    let Some(acc) = response.value else {
        return Ok(None);
    };

    // Layout (after the 8-byte Anchor discriminator):
    //   instance(32) mint(32) epoch(u64) withdrawn(u64) cap(u64) bump(u8)
    const EPOCH_OFFSET: usize = 8 + 32 + 32;
    const WITHDRAWN_OFFSET: usize = EPOCH_OFFSET + 8;
    const CAP_OFFSET: usize = WITHDRAWN_OFFSET + 8;
    if acc.data.len() < CAP_OFFSET + 8 {
        return Err(eyre!(
            "WithdrawEpoch account too small: {} bytes",
            acc.data.len()
        ));
    }
    let read_u64 = |offset: usize| -> Result<u64> {
        let bytes: [u8; 8] = acc.data[offset..offset + 8]
            .try_into()
            .map_err(|_| eyre!("WithdrawEpoch account data layout error at {}", offset))?;
        Ok(u64::from_le_bytes(bytes))
    };
    Ok(Some(WithdrawEpochState {
        epoch: read_u64(EPOCH_OFFSET)?,
        withdrawn: read_u64(WITHDRAWN_OFFSET)?,
        cap: read_u64(CAP_OFFSET)?,
    }))
}

/// Fetch on-chain `(deposited, locked)` from the UserBalance PDA. Returns
/// `(0, 0)` if the account does not exist (user has never deposited on this
/// instance/mint).
pub async fn fetch_user_balance(
    rpc_url: &str,
    instance: &Pubkey,
    user: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> Result<(u64, u64)> {
    use solana_client::nonblocking::rpc_client::RpcClient;
    let client = RpcClient::new(rpc_url.to_string());
    let (pda, _) = derive_user_balance_pda(instance, user, mint, program_id);
    let response = client
        .get_account_with_commitment(&pda, client.commitment())
        .await
        .map_err(|e| eyre!("get_account (UserBalance PDA): {}", e))?;

    let Some(acc) = response.value else {
        // Account missing is normal for first-time users; do not confuse with RPC failure.
        return Ok((0, 0));
    };

    // Layout (after 8-byte Anchor discriminator):
    //   instance: Pubkey (32)
    //   user:     Pubkey (32)
    //   mint:     Pubkey (32)
    //   deposited: u64 LE (8)  ← offset 8 + 32*3 = 104
    //   locked:    u64 LE (8)  ← offset 112
    //   bump:      u8 (1)
    const DEPOSITED_OFFSET: usize = 8 + 32 + 32 + 32;
    const LOCKED_OFFSET: usize = DEPOSITED_OFFSET + 8;
    if acc.data.len() < LOCKED_OFFSET + 8 {
        return Err(eyre!(
            "UserBalance account too small: {} bytes",
            acc.data.len()
        ));
    }
    let deposited_bytes: [u8; 8] = acc.data[DEPOSITED_OFFSET..DEPOSITED_OFFSET + 8]
        .try_into()
        .map_err(|_| eyre!("UserBalance account data layout error (deposited)"))?;
    let locked_bytes: [u8; 8] = acc.data[LOCKED_OFFSET..LOCKED_OFFSET + 8]
        .try_into()
        .map_err(|_| eyre!("UserBalance account data layout error (locked)"))?;
    Ok((
        u64::from_le_bytes(deposited_bytes),
        u64::from_le_bytes(locked_bytes),
    ))
}
