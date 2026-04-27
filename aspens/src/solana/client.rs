//! Stateful Solana helpers — RPC submission and on-chain state reads.
//!
//! Gated behind the `client` feature. Pure instruction builders and PDA
//! derivations live in the parent `solana` module and are available to any
//! consumer that enables the `solana` feature alone.

use eyre::{eyre, Result};
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
    // Program id is in `factory_address`; the existing scaffold also accepted
    // `trade_contract.contract_id`, so fall back to it for compatibility.
    let program_str = if !chain.factory_address.is_empty() {
        chain.factory_address.clone()
    } else {
        chain
            .trade_contract
            .as_ref()
            .and_then(|tc| tc.contract_id.clone())
            .ok_or_else(|| {
                eyre!(
                    "Solana chain '{}' has no factory_address / trade_contract.contract_id (program id) configured",
                    chain.network
                )
            })?
    };
    let program_id = Pubkey::from_str(&program_str)
        .map_err(|e| eyre!("invalid Solana program id '{}': {}", program_str, e))?;

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
    use solana_client::nonblocking::rpc_client::RpcClient;
    let client = RpcClient::new(rpc_url.to_string());
    let blockhash = client
        .get_latest_blockhash()
        .await
        .map_err(|e| eyre!("get_latest_blockhash: {}", e))?;
    let tx = Transaction::new_signed_with_payer(
        &[ix],
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
    Ok((u64::from_le_bytes(deposited_bytes), u64::from_le_bytes(locked_bytes)))
}
