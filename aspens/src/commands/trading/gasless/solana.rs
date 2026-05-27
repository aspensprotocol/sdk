//! Solana branch of [`super::build_gasless_authorization`].
//!
//! Produces the borsh-encoded `OpenForSignedPayload` via
//! [`crate::solana::gasless_lock_signing_message`] and has the wallet
//! Ed25519-sign it. The deadline is `current_slot + buffer` — fetched
//! once from the origin chain's RPC.
//!
//! Split out of the `gasless` module so the dispatcher in `mod.rs` stays
//! small enough to scan top-to-bottom.

use eyre::{eyre, Result};

use super::super::send_order::arborter_pb::GaslessAuthorization;
use super::{parse_cross_chain_token_into_32, GaslessBuildArgs, SOLANA_DEADLINE_SLOT_BUFFER};

#[cfg(feature = "solana")]
pub(super) async fn build_solana(
    args: GaslessBuildArgs<'_>,
    order_id_bytes: [u8; 32],
) -> Result<GaslessAuthorization> {
    use crate::solana::{gasless_lock_signing_message, OpenOrderArgs};
    use solana_sdk::pubkey::Pubkey;

    let GaslessBuildArgs {
        origin_chain,
        destination_chain,
        wallet,
        input_token_address,
        output_token_address,
        amount_in,
        amount_out,
        nonce,
        order_id_hex,
    } = args;

    // Deadline = current_slot + buffer. Fetches once from origin chain's RPC.
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(origin_chain.rpc_url.clone());
    let current_slot = rpc
        .get_slot()
        .await
        .map_err(|e| eyre!("solana get_slot: {e}"))?;
    let deadline = current_slot + SOLANA_DEADLINE_SLOT_BUFFER;

    let instance_pda: Pubkey = origin_chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre!("origin chain has no trade_contract configured"))?
        .address
        .parse()
        .map_err(|e| eyre!("invalid trade_contract.address on origin chain: {e}"))?;
    let user_pubkey: Pubkey = wallet.address().parse().map_err(|e| {
        eyre!(
            "wallet address {:?} not a valid Solana pubkey: {e}",
            wallet.address()
        )
    })?;
    let input_token: Pubkey = input_token_address
        .parse()
        .map_err(|e| eyre!("input token {input_token_address:?} not a Solana pubkey: {e}"))?;

    // For EVM destination tokens (0x-prefixed 20-byte hex), the address
    // won't be a 32-byte Solana pubkey. Left-pad into a 32-byte slot so
    // it fits OpenOrderArgs::output_token. Arborter-side unpacks by
    // convention (low-order 20 bytes = EVM addr).
    let output_token_bytes = parse_cross_chain_token_into_32(output_token_address)?;

    let amount_in_u64 = u64::try_from(amount_in)
        .map_err(|_| eyre!("Solana amount_in {amount_in} exceeds u64::MAX"))?;
    let amount_out_u64 = u64::try_from(amount_out)
        .map_err(|_| eyre!("Solana amount_out {amount_out} exceeds u64::MAX"))?;

    let order = OpenOrderArgs {
        order_id: order_id_bytes,
        origin_chain_id: origin_chain.chain_id as u64,
        destination_chain_id: destination_chain.chain_id as u64,
        input_token,
        input_amount: amount_in_u64,
        output_token: output_token_bytes,
        output_amount: amount_out_u64,
    };
    let message = gasless_lock_signing_message(&instance_pda, &user_pubkey, deadline, &order)?;

    // Wallet::sign_message on Solana → raw Ed25519 sign, 64 bytes.
    let sig = wallet.sign_message(&message).await?;
    if sig.len() != 64 {
        return Err(eyre!(
            "Solana gasless signature must be 64 bytes (Ed25519); got {}",
            sig.len()
        ));
    }

    // Placate the unused-var lints on both paths.
    let _ = nonce;

    Ok(GaslessAuthorization {
        user_signature: sig,
        deadline,
        order_id: order_id_hex,
        nonce: 0,
        open_deadline: 0,
        // Same semantics as the EVM path: send the exact integers the
        // user signed inside the borsh `OpenForSignedPayload`. The
        // arborter rebuilds the open_for ix from `auth.amount_in` so
        // its OpenOrderArgs match the user's signed message byte-for-byte
        // and the Ed25519Program precompile accepts the signature.
        amount_in: amount_in.to_string(),
        amount_out: amount_out.to_string(),
    })
}

#[cfg(not(feature = "solana"))]
pub(super) async fn build_solana(
    _args: GaslessBuildArgs<'_>,
    _order_id_bytes: [u8; 32],
) -> Result<GaslessAuthorization> {
    Err(eyre!(
        "Solana gasless authorization requires the `solana` feature of the aspens crate"
    ))
}
