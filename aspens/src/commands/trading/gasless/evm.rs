//! EVM branch of [`super::build_gasless_authorization`].
//!
//! Produces an EIP-712 digest via [`crate::evm::gasless_lock_signing_hash`]
//! and has the wallet sign it via `Wallet::sign_message` (which applies
//! the EIP-191 wrap MidribV2's `_verifyOrder` expects).
//!
//! Split out of the `gasless` module so the dispatcher in `mod.rs` stays
//! small enough to scan top-to-bottom.

use eyre::{eyre, Result};

use crate::orders::GaslessLockParams;

use super::super::send_order::arborter_pb::GaslessAuthorization;
use super::{unix_secs, GaslessBuildArgs, EVM_FILL_DEADLINE_SECS, EVM_OPEN_DEADLINE_SECS};

#[cfg(feature = "evm")]
pub(super) async fn build_evm(args: GaslessBuildArgs<'_>) -> Result<GaslessAuthorization> {
    use alloy_primitives::Address;

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

    let now = unix_secs()?;
    let open_deadline = now + EVM_OPEN_DEADLINE_SECS;
    let fill_deadline = now + EVM_FILL_DEADLINE_SECS;

    let depositor = wallet.address();
    let dest_chain_id = destination_chain.chain_id.to_string();
    let params = GaslessLockParams {
        depositor_address: &depositor,
        token_contract: input_token_address,
        token_contract_destination_chain: output_token_address,
        destination_chain_id: &dest_chain_id,
        amount_in,
        amount_out,
        order_id: &order_id_hex,
        deadline: fill_deadline,
        nonce,
        open_deadline,
        user_signature: &[],
    };

    let arborter: Address = origin_chain
        .instance_signer_address
        .parse()
        .map_err(|e| eyre!("invalid instance_signer_address on origin chain: {e}"))?;
    let origin_settler: Address = origin_chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre!("origin chain has no trade_contract configured"))?
        .address
        .parse()
        .map_err(|e| eyre!("invalid trade_contract.address on origin chain: {e}"))?;
    let digest = crate::evm::gasless_lock_signing_hash(
        &params,
        arborter,
        origin_settler,
        origin_chain.chain_id as u64,
    )?;

    // `Wallet::sign_message` on EVM applies EIP-191, which is what
    // MidribV2._verifyOrder wraps the digest with before ecrecover.
    // `sign_hash` / `sign_eip712_digest` would NOT wrap and would be
    // rejected as INVALID_SIGNER on-chain.
    let sig = wallet.sign_message(digest.as_slice()).await?;

    if sig.len() != 65 {
        return Err(eyre!(
            "EVM gasless signature must be 65 bytes (r||s||v); got {}",
            sig.len()
        ));
    }

    Ok(GaslessAuthorization {
        user_signature: sig,
        deadline: fill_deadline,
        order_id: order_id_hex,
        nonce,
        open_deadline,
        // Echo the user-signed amounts to the arborter so it can build
        // the on-chain GaslessLockParams with identical values. The
        // contract hashes these into the EIP-712 digest and ecrecover's
        // against `order.user`; any divergence between SDK-signed and
        // arborter-rebuilt amounts surfaces as `INVALID_SIGNER`.
        amount_in: amount_in.to_string(),
        amount_out: amount_out.to_string(),
    })
}

#[cfg(not(feature = "evm"))]
pub(super) async fn build_evm(_args: GaslessBuildArgs<'_>) -> Result<GaslessAuthorization> {
    Err(eyre!(
        "EVM gasless authorization requires the `evm` feature of the aspens crate"
    ))
}
