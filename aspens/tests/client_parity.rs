//! Snapshot parity tests for client-side signing payloads.
//!
//! Under the optimistic ledger the only client-derived values that must match
//! the arborter byte-for-byte are the canonical `order_id`
//! (`aspens::orders::derive_order_id`) and the EIP-712 domain constants. The
//! legacy gasless on-chain-lock signing parity (EVM `GaslessCrossChainOrder`
//! EIP-712 digest, Solana `OpenForSignedPayload` borsh layout) was retired with
//! the on-chain order machinery — order authentication is now the outer
//! envelope signature, covered by `aspens::evm::sign_send_order_envelope`'s
//! own round-trip test.
//!
//! If an arborter refactor changes the order-id recipe or the domain
//! constants, **regenerate these snapshots from arborter's code** rather than
//! silently updating — the whole point is to catch accidental divergence.

#![cfg(all(feature = "evm", feature = "solana"))]

use aspens::orders::derive_order_id;

// -- EVM domain constants -------------------------------------------------

#[test]
fn evm_eip712_constants_are_pinned() {
    // Any change here must be mirrored on the Solidity side (MidribV3's
    // `_domainNameAndVersion`) and in the arborter's `MIDRIB_EIP712_NAME` /
    // `MIDRIB_EIP712_VERSION`. MidribV3 bumped the domain version to "3".
    assert_eq!(aspens::evm::MIDRIB_EIP712_NAME, "Midrib");
    assert_eq!(aspens::evm::MIDRIB_EIP712_VERSION, "3");
}

// -- chain-agnostic order id ---------------------------------------------

#[test]
fn derive_order_id_snapshot() {
    // The single reference hash — must match arborter's
    // `chain_traits::market::derive_order_id` exactly.
    let id = derive_order_id(
        &[0xAAu8; 32],
        42,
        501,
        8453,
        b"InputMintPubkey32BytesRepresentat",
        b"0xOutputTokenAddressEvmLower4321",
        1_000_000,
        2_000_000,
    );
    let expected_hex = "642e8b1deac921a7ddc00254b847ed1eb90169b1d3a70a34b541b66617b63843";
    assert_eq!(hex::encode(id), expected_hex);
}
