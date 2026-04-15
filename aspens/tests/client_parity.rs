//! Snapshot parity tests for client-side signing payloads.
//!
//! The EVM EIP-712 digest and the Solana borsh signing message MUST match
//! exactly what `arborter/app/chain-evm` and `arborter/app/chain-solana`
//! produce — a one-bit drift in layout, domain, or constants breaks order
//! submission silently (the arborter recovers/verifies a different hash).
//!
//! These tests pin the outputs for concrete inputs. Because both sides use
//! the same `sol!` artifacts + `alloy_sol_types` (EVM) and the same borsh
//! crate + Pubkey layout (Solana), identical inputs here produce identical
//! outputs on the arborter side. If an arborter refactor changes any of the
//! hashing / constants / layouts, **regenerate these snapshots from
//! arborter's code** rather than silently updating — the whole point is to
//! catch accidental divergence.

#![cfg(all(feature = "evm", feature = "solana"))]

use aspens::orders::{derive_order_id, GaslessLockParams};

// -- EVM parity -----------------------------------------------------------

#[test]
fn evm_eip712_constants_are_pinned() {
    // Any change here must be mirrored on the Solidity side and in the
    // arborter's `MIDRIB_EIP712_NAME` / `MIDRIB_EIP712_VERSION`.
    assert_eq!(aspens::evm::MIDRIB_EIP712_NAME, "Midrib");
    assert_eq!(aspens::evm::MIDRIB_EIP712_VERSION, "2");
}

#[test]
fn evm_gasless_lock_signing_hash_snapshot() {
    use alloy_primitives::Address;

    // Deterministic input — addresses are all-zero-but-one so the snapshot
    // is readable when it fails.
    let arborter: Address = "0x0000000000000000000000000000000000000aA1"
        .parse()
        .unwrap();
    let settler: Address = "0x0000000000000000000000000000000000000bB2"
        .parse()
        .unwrap();
    let depositor = "0x0000000000000000000000000000000000000cC3";
    let token_in = "0x0000000000000000000000000000000000000dD4";
    let token_out = "0x0000000000000000000000000000000000000eE5";

    let params = GaslessLockParams {
        depositor_address: depositor,
        token_contract: token_in,
        token_contract_destination_chain: token_out,
        destination_chain_id: "8453",
        amount_in: 1_000_000,
        amount_out: 2_000_000,
        order_id: "",
        deadline: 1_700_000_100,
        nonce: 42,
        open_deadline: 1_700_000_000,
        user_signature: &[],
    };

    let digest = aspens::evm::gasless_lock_signing_hash(&params, arborter, settler, 84532).unwrap();

    // If this snapshot breaks, regenerate from arborter with the same
    // inputs; do NOT blindly update.
    let expected_hex = "df311c324f054e2b139a5b25950d372ef729a4e5c7132256ca0990170cf4fe40";
    assert_eq!(
        hex::encode(digest),
        expected_hex,
        "EIP-712 digest drifted — align with arborter::chain_evm::gasless_lock_signing_hash"
    );
}

// -- Solana parity --------------------------------------------------------

#[test]
fn solana_gasless_lock_signing_message_snapshot() {
    use aspens::solana::{gasless_lock_signing_message, OpenOrderArgs};
    use solana_sdk::pubkey::Pubkey;

    let instance = Pubkey::new_from_array([0x11; 32]);
    let user = Pubkey::new_from_array([0x22; 32]);
    let input_token = Pubkey::new_from_array([0x33; 32]);
    let output_token_bytes = [0x44u8; 32];

    let order = OpenOrderArgs {
        order_id: [0x55; 32],
        origin_chain_id: 501,
        destination_chain_id: 8453,
        input_token,
        input_amount: 1_000_000,
        output_token: output_token_bytes,
        output_amount: 2_000_000,
    };

    let bytes = gasless_lock_signing_message(&instance, &user, 1_700_000_000, &order).unwrap();

    // Layout (borsh, no length prefixes on fixed-size arrays / structs):
    //   instance (32) || user (32) || deadline (8 LE) ||
    //   order_id (32) || origin_chain_id (8 LE) || destination_chain_id (8 LE) ||
    //   input_token (32) || input_amount (8 LE) ||
    //   output_token (32) || output_amount (8 LE)
    // Total: 32+32+8 + 32+8+8+32+8+32+8 = 200 bytes.
    assert_eq!(bytes.len(), 200, "OpenForSignedPayload byte length drifted");

    let expected_hex = "1111111111111111111111111111111111111111111111111111111111111111\
         2222222222222222222222222222222222222222222222222222222222222222\
         00f1536500000000\
         5555555555555555555555555555555555555555555555555555555555555555\
         f501000000000000\
         0521000000000000\
         3333333333333333333333333333333333333333333333333333333333333333\
         40420f0000000000\
         4444444444444444444444444444444444444444444444444444444444444444\
         80841e0000000000";
    assert_eq!(
        hex::encode(&bytes),
        expected_hex,
        "borsh layout drifted — align with arborter::chain_solana::gasless_lock_signing_message"
    );
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
