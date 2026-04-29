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

#[test]
fn evm_gasless_hash_with_solana_destination_token() {
    // Pin the digest produced when the destination is a non-EVM chain whose
    // token id is a 32-byte Solana mint. Before the OrderData.outputToken
    // bytes32 widening, this combination errored at parse time
    // ("invalid string length") because the SDK tried to coerce a base58
    // mint into a 20-byte alloy `Address`. After the widening, the mint
    // bytes flow into the bytes32 slot directly and produce a deterministic
    // digest distinct from any digest reachable with an EVM address.
    //
    // If this snapshot breaks, regenerate from arborter's chain-evm
    // anvil_harness.rs with the same inputs; do NOT blindly update.
    use alloy_primitives::Address;

    let arborter: Address = "0x0000000000000000000000000000000000000aA1"
        .parse()
        .unwrap();
    let settler: Address = "0x0000000000000000000000000000000000000bB2"
        .parse()
        .unwrap();
    let depositor = "0x0000000000000000000000000000000000000cC3";
    let token_in = "0x0000000000000000000000000000000000000dD4";
    // Base58 of 32 bytes of 0x44 — same byte pattern the Solana parity
    // test uses for its `output_token_bytes`. Picked so the snapshot is
    // visually correlatable with the cross-chain side.
    let token_out_solana_mint = bs58::encode([0x44u8; 32]).into_string();

    let params = GaslessLockParams {
        depositor_address: depositor,
        token_contract: token_in,
        token_contract_destination_chain: &token_out_solana_mint,
        destination_chain_id: "501",
        amount_in: 1_000_000,
        amount_out: 2_000_000,
        order_id: "",
        deadline: 1_700_000_100,
        nonce: 42,
        open_deadline: 1_700_000_000,
        user_signature: &[],
    };

    let digest = aspens::evm::gasless_lock_signing_hash(&params, arborter, settler, 84532).unwrap();
    let expected_hex = "dfc4ba49f31772c3f4df2a95e05200db863bffe680050fc582cae3d8a9fe5c05";
    assert_eq!(
        hex::encode(digest),
        expected_hex,
        "EIP-712 digest with Solana-mint destination drifted — regenerate from arborter"
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

// -- Arborter-fixture mirrors --------------------------------------------
//
// These tests use the **exact** inputs arborter's own program/integration
// tests use, so a byte-for-byte mismatch here signals divergence between
// SDK and arborter directly. The SDK's other snapshot tests catch
// internal drift; these catch cross-repo drift.

#[test]
fn solana_message_matches_arborter_fixture() {
    // Mirrors arborter/chains/solana/programs/midrib/src/instructions.rs ::
    // `gasless_signing_message_layout_is_stable` — any change to borsh
    // layout on either side breaks one of these assertions.
    use aspens::solana::{gasless_lock_signing_message, OpenOrderArgs};
    use solana_sdk::pubkey::Pubkey;

    let instance = Pubkey::new_from_array([1u8; 32]);
    let user = Pubkey::new_from_array([2u8; 32]);
    let mint = Pubkey::new_from_array([3u8; 32]);
    let order = OpenOrderArgs {
        order_id: [4u8; 32],
        origin_chain_id: 501,
        destination_chain_id: 1,
        input_token: mint,
        input_amount: 1_000_000,
        output_token: [5u8; 32],
        output_amount: 1_000_000,
    };
    let msg = gasless_lock_signing_message(&instance, &user, 99, &order).unwrap();

    assert_eq!(msg.len(), 200, "layout diverged from arborter");
    // Positional cross-checks match the arborter-side assertions exactly.
    assert_eq!(&msg[..32], instance.as_ref());
    assert_eq!(&msg[32..64], user.as_ref());
    assert_eq!(&msg[64..72], 99u64.to_le_bytes());
    assert_eq!(&msg[72..104], &[4u8; 32]); // order.order_id
}

#[test]
fn evm_hash_matches_arborter_fixture() {
    // Mirrors arborter/app/chain-evm/src/market.rs ::
    // `gasless_order_signature_round_trips` — same params + same
    // arborter / origin_settler / chain_id → same EIP-712 digest on
    // both sides. arborter's round-trip test signs + recovers; we just
    // pin the bytes, but identical inputs land on the same hash by
    // construction of alloy's sol! + eip712_domain!.
    use alloy_primitives::{address, Address};

    let arborter: Address = address!("1111111111111111111111111111111111111111");
    let origin_settler: Address = address!("2222222222222222222222222222222222222222");
    let token = address!("3333333333333333333333333333333333333333");
    let dest_token = address!("4444444444444444444444444444444444444444");
    let depositor = address!("5555555555555555555555555555555555555555");
    let origin_chain_id: u64 = 13337;

    let depositor_s = depositor.to_string();
    let token_s = token.to_string();
    let dest_token_s = dest_token.to_string();
    let sig_placeholder = vec![0u8; 65];

    let params = GaslessLockParams {
        depositor_address: &depositor_s,
        token_contract: &token_s,
        token_contract_destination_chain: &dest_token_s,
        destination_chain_id: "1",
        amount_in: 1_000_000,
        amount_out: 1_000_000,
        order_id: "",
        deadline: 2_000_000_000,
        open_deadline: 1_999_000_000,
        nonce: 42,
        user_signature: &sig_placeholder,
    };

    let digest =
        aspens::evm::gasless_lock_signing_hash(&params, arborter, origin_settler, origin_chain_id)
            .unwrap();

    // arborter-side reference: same build_gasless_cross_chain_order
    // inputs + same eip712_domain! → same bytes. Fixed expected hash
    // below; regenerate via the arborter round-trip test if the
    // underlying sol! struct layout ever changes.
    let expected_hex = "959bb32ae0a4690b5cfcc13110bddce3ba5f1bc29301168221493ea40ab884fe";
    assert_eq!(
        hex::encode(digest),
        expected_hex,
        "EIP-712 digest diverged from arborter fixture"
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
