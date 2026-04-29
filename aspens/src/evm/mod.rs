//! EVM client-side helpers for the Midrib V2 cross-chain order flow.
//!
//! Ported from `arborter/app/chain-evm/src/market.rs` — keep the
//! [`MIDRIB_EIP712_NAME`] / [`MIDRIB_EIP712_VERSION`] constants and the
//! [`gasless_lock_signing_hash`] recipe in lock-step with that source so
//! client- and arborter-side signatures match.
//!
//! # Typical usage
//!
//! ```ignore
//! use aspens::orders::GaslessLockParams;
//! use aspens::evm::{gasless_lock_signing_hash, sign_send_order_envelope};
//!
//! let params = GaslessLockParams { /* ... */ };
//! let digest = gasless_lock_signing_hash(&params, arborter, settler, chain_id)?;
//! let permit_sig = wallet.sign_eip712_digest(digest).await?;
//!
//! // Outer envelope for the gRPC SendOrderRequest:
//! let envelope_sig = sign_send_order_envelope(&wallet, &encoded_order).await?;
//! ```

use alloy_primitives::{keccak256, Address, Bytes, FixedBytes, Uint, B256, U160, U256};
use alloy_sol_types::{eip712_domain, sol, SolStruct, SolValue};
use eyre::Result;
use std::str::FromStr;

use crate::orders::GaslessLockParams;

// -- sol! bindings --------------------------------------------------------
//
// Mirror the arborter invocations so both sides build identical types. The
// JSON artifacts and the MidribDataTypes.sol file are copied into
// `aspens/artifacts/` so these macros can resolve them at compile time.

// Note: no `#[sol(rpc)]` — the stateless signing module only needs struct
// types, and `rpc` would pull in `alloy-contract` (an RPC/provider
// dep). The commands/trading module has its own `#[sol(rpc)]` invocations
// behind the `client` feature for actual on-chain calls.

sol!(
    #[derive(Debug)]
    #[allow(missing_docs)]
    MidribV2,
    "artifacts/MidribV2.json"
);

sol!(
    #[derive(Debug)]
    #[allow(missing_docs)]
    IAllowanceTransfer,
    "artifacts/IAllowanceTransfer.json"
);

#[allow(missing_docs)]
mod data_types {
    alloy_sol_types::sol!(
        #[sol(abi)]
        "artifacts/MidribDataTypes.sol"
    );
}
pub use data_types::*;

// -- EIP-712 domain -------------------------------------------------------

/// EIP-712 domain name used by MidribV2. Must match the Solidity constant so
/// client-side digests equal the contract's `lock_for_order_gasless` check.
pub const MIDRIB_EIP712_NAME: &str = "Midrib";
/// EIP-712 domain version used by MidribV2.
pub const MIDRIB_EIP712_VERSION: &str = "2";

// -- Gasless order builder + hasher --------------------------------------

/// Build a `MidribV2::GaslessCrossChainOrder` from the chain-agnostic
/// [`GaslessLockParams`]. Pure struct construction — no RPC.
pub fn build_gasless_cross_chain_order(
    params: &GaslessLockParams<'_>,
    arborter_address: Address,
    origin_settler: Address,
    origin_chain_id: u64,
) -> Result<MidribV2::GaslessCrossChainOrder> {
    if params.deadline == 0 || params.open_deadline == 0 {
        return Err(eyre::eyre!(
            "EVM gasless order requires non-zero deadline (fillDeadline) and open_deadline"
        ));
    }
    if u32::try_from(params.deadline).is_err() || u32::try_from(params.open_deadline).is_err() {
        return Err(eyre::eyre!(
            "EVM gasless deadlines must fit in u32 (contract field width)"
        ));
    }

    let amount_in = U160::from(params.amount_in);
    let amount_out = U160::from(params.amount_out);
    let action = MidribDataTypes::IntentAction::LOCK;

    let permit2_single = IAllowanceTransfer::PermitSingle {
        details: IAllowanceTransfer::PermitDetails {
            token: params.token_contract.parse()?,
            amount: amount_in,
            expiration: Uint::<48, 1>::from(0),
            nonce: Uint::<48, 1>::from(params.nonce),
        },
        spender: arborter_address,
        sigDeadline: U256::ZERO,
    };

    let order_data = MidribDataTypes::OrderData {
        // The on-chain field is `bytes32`: it accepts both EVM addresses
        // (left-padded to 32 bytes) and 32-byte non-EVM token ids
        // (Solana mints, etc.) without losing information.
        outputToken: FixedBytes::<32>::from(crate::orders::parse_destination_token_bytes32(
            params.token_contract_destination_chain,
        )?),
        outputAmount: amount_out,
        inputAmount: amount_in,
        recipient: params.depositor_address.parse()?,
        destinationChainId: U256::from_str(params.destination_chain_id)?,
        exclusiveRelayer: arborter_address,
        message: Bytes::new(),
    };
    let encoded_order_data = Bytes::from((action, permit2_single, order_data).abi_encode_params());

    Ok(MidribV2::GaslessCrossChainOrder {
        originSettler: origin_settler,
        user: params.depositor_address.parse()?,
        nonce: U256::from(params.nonce),
        originChainId: U256::from(origin_chain_id),
        openDeadline: params.open_deadline as u32,
        fillDeadline: params.deadline as u32,
        orderDataType: FixedBytes::<32>::from_slice(&[0u8; 32]),
        orderData: encoded_order_data,
    })
}

/// Produce the EIP-712 signing digest a wallet must sign to authorize a
/// gasless lock. This is the input to `wallet.sign_eip712_digest(...)`.
pub fn gasless_lock_signing_hash(
    params: &GaslessLockParams<'_>,
    arborter_address: Address,
    origin_settler: Address,
    origin_chain_id: u64,
) -> Result<FixedBytes<32>> {
    let order =
        build_gasless_cross_chain_order(params, arborter_address, origin_settler, origin_chain_id)?;
    let domain = eip712_domain! {
        name: MIDRIB_EIP712_NAME,
        version: MIDRIB_EIP712_VERSION,
        chain_id: origin_chain_id,
        verifying_contract: origin_settler,
    };
    Ok(order.eip712_signing_hash(&domain))
}

// -- Outer envelope signature --------------------------------------------

/// Produce the EIP-191 personal-sign digest for an encoded order payload.
///
/// This is the signing counterpart to the arborter's `is_signature_valid`
/// check over `SendOrderRequest.signature_hash`: the arborter recovers the
/// address from the same message via `eth_sign` / EIP-191, so clients must
/// sign the message with a method that applies the
/// `"\x19Ethereum Signed Message:\n<len>" || message` prefix.
///
/// `alloy::signers::Signer::sign_message` already applies this prefix, which
/// is why [`crate::wallet::Wallet::sign_message`] returns the correct
/// 65-byte envelope signature out of the box. This helper exists as the
/// authoritative reference for the exact digest that will be recovered
/// against — useful for tests, or for clients that want to verify locally
/// before submitting.
pub fn envelope_signing_digest(message: &[u8]) -> B256 {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut buf = Vec::with_capacity(prefix.len() + message.len());
    buf.extend_from_slice(prefix.as_bytes());
    buf.extend_from_slice(message);
    keccak256(&buf)
}

/// Convenience: sign the outer `SendOrderRequest` envelope for an encoded
/// order payload using the provided wallet. Returns the raw 65-byte ECDSA
/// signature (r||s||v). Errors if the wallet is not an EVM wallet.
pub async fn sign_send_order_envelope(
    wallet: &crate::wallet::Wallet,
    encoded_order: &[u8],
) -> Result<Vec<u8>> {
    wallet.sign_message(encoded_order).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eip712_constants_are_exact() {
        // Locked in by the on-chain contract. Any drift breaks signature
        // recovery silently — snapshot the values.
        assert_eq!(MIDRIB_EIP712_NAME, "Midrib");
        assert_eq!(MIDRIB_EIP712_VERSION, "2");
    }

    #[test]
    fn envelope_digest_matches_eip191() {
        // EIP-191 personal_sign prefix on an empty message.
        let digest = envelope_signing_digest(b"");
        let expected = keccak256(b"\x19Ethereum Signed Message:\n0");
        assert_eq!(digest, expected);
    }

    #[test]
    fn build_rejects_zero_deadline() {
        let params = GaslessLockParams {
            depositor_address: "0x0000000000000000000000000000000000000001",
            token_contract: "0x0000000000000000000000000000000000000002",
            token_contract_destination_chain: "0x0000000000000000000000000000000000000003",
            destination_chain_id: "1",
            amount_in: 10,
            amount_out: 10,
            order_id: "",
            deadline: 0,
            nonce: 0,
            open_deadline: 100,
            user_signature: &[],
        };
        let arb = Address::ZERO;
        assert!(build_gasless_cross_chain_order(&params, arb, arb, 1).is_err());
    }
}
