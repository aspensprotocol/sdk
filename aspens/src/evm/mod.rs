//! EVM client-side helpers for the Midrib optimistic-ledger order flow.
//!
//! Ported from `arborter/app/chain-evm/src/market.rs`. Under the optimistic
//! shadow ledger, orders never lock on-chain — the only thing a client signs
//! for order entry is the **outer envelope** over the encoded `SendOrderRequest`
//! (the counterpart to the arborter's `is_signature_valid`). The legacy gasless
//! on-chain-lock signing (EIP-712 `GaslessCrossChainOrder` / Permit2) is gone
//! with MidribV2's order machinery.
//!
//! # Typical usage
//!
//! ```ignore
//! use aspens::evm::sign_send_order_envelope;
//!
//! // Outer envelope for the gRPC SendOrderRequest:
//! let envelope_sig = sign_send_order_envelope(&wallet, &encoded_order).await?;
//! ```

use alloy_primitives::{B256, keccak256};
use eyre::Result;

/// RPC-enabled (`#[sol(rpc)]`) bindings for MidribV3 + IERC20. Pulls
/// `alloy-contract`; only available with the `client` feature.
#[cfg(feature = "client")]
pub mod rpc;

// -- EIP-712 domain -------------------------------------------------------

/// EIP-712 domain name used by Midrib. Must match the Solidity constant so
/// client-side digests equal the contract's verification.
pub const MIDRIB_EIP712_NAME: &str = "Midrib";
/// EIP-712 domain version used by MidribV3 (bumped from "2" with the rename).
pub const MIDRIB_EIP712_VERSION: &str = "3";

// -- Native-asset sentinel -------------------------------------------------

/// The sentinel "token address" keying the chain's native asset (ETH/FLR) in
/// MidribV3 (`MidribV3.NATIVE`). A token configured with this address is the
/// native asset: deposit via the payable `depositNative` (no ERC-20 approve),
/// withdraw via the same voucher flow (paid as raw value).
pub const NATIVE_TOKEN_SENTINEL: &str = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE";

/// `true` if `addr` is the native-asset sentinel (hex is case-insensitive).
pub fn is_native_token(addr: &str) -> bool {
    addr.eq_ignore_ascii_case(NATIVE_TOKEN_SENTINEL)
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
        // recovery silently — snapshot the values. MidribV3 bumped the
        // domain version to "3".
        assert_eq!(MIDRIB_EIP712_NAME, "Midrib");
        assert_eq!(MIDRIB_EIP712_VERSION, "3");
    }

    #[test]
    fn native_sentinel_matches_contract_constant() {
        // Locked to MidribV3.NATIVE. All-`e` nibbles, so the lowercase form is
        // exactly 40 `e`s — pin both the value and the case-insensitive match.
        assert_eq!(
            NATIVE_TOKEN_SENTINEL.to_ascii_lowercase(),
            format!("0x{}", "e".repeat(40))
        );
        assert!(is_native_token(NATIVE_TOKEN_SENTINEL));
        assert!(is_native_token(
            "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
        ));
        assert!(!is_native_token(
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEe0"
        ));
    }

    #[test]
    fn envelope_digest_matches_eip191() {
        // EIP-191 personal_sign prefix on an empty message.
        let digest = envelope_signing_digest(b"");
        let expected = keccak256(b"\x19Ethereum Signed Message:\n0");
        assert_eq!(digest, expected);
    }

    #[test]
    fn envelope_digest_length_prefix_is_byte_count() {
        // The EIP-191 prefix encodes the byte length of the message, not
        // its character count. Cover a few sizes so a future refactor
        // can't silently switch to e.g. char_indices().
        for msg in [b"a".as_slice(), b"hello", &[0u8; 32], &[0xffu8; 256]] {
            let digest = envelope_signing_digest(msg);
            let mut buf = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
            buf.extend_from_slice(msg);
            assert_eq!(digest, keccak256(&buf), "len={}", msg.len());
        }
    }

    #[tokio::test]
    async fn sign_send_order_envelope_round_trips_to_signer_address() {
        // Contract with arborter: SDK signs the encoded order with
        // EIP-191; arborter's `verify_secp256k1`
        // (arborter/app/onchain/src/verify.rs) calls
        // `Signature::recover_address_from_msg(message)` and compares to
        // the trader address. A drift in the prefix or hashing here
        // silently rejects every order — this is the single most
        // important EVM signing test in the SDK.
        use alloy_primitives::Signature;
        use std::str::FromStr;

        // Anvil test key #0.
        let wallet = crate::wallet::Wallet::from_evm_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        let signer_address = alloy_primitives::Address::from_str(&wallet.address()).unwrap();

        // A representative encoded-order payload — exact bytes don't
        // matter for this test, only that round-trip recovery succeeds.
        let encoded_order = b"\x00\x01encoded-send-order-payload";
        let sig_bytes = sign_send_order_envelope(&wallet, encoded_order)
            .await
            .unwrap();

        // Arborter's strict length check (verify.rs:63).
        assert_eq!(
            sig_bytes.len(),
            65,
            "sign_send_order_envelope must return r||s||v = 65 bytes; arborter rejects anything else"
        );

        let sig = Signature::try_from(sig_bytes.as_slice()).unwrap();
        let recovered = sig
            .recover_address_from_msg(encoded_order.as_slice())
            .unwrap();
        assert_eq!(
            recovered, signer_address,
            "recovered address must match the signing wallet — this is exactly what arborter checks"
        );
    }
}
