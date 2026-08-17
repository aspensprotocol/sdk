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

// -- Order wire encoding: what the envelope signature is taken over -------

/// The arborter verifies `signature_hash` against `order.encode(&buf)` — the
/// prost encoding of `Order` — and recovers the signer from it. A one-byte
/// drift is not an error there: it recovers a DIFFERENT address and rejects
/// the order for a bad signature, saying nothing about the field that moved.
///
/// So the expected bytes below are assembled BY HAND from the proto's field
/// numbers rather than from prost, and the whole encoding is pinned as hex.
/// If the arborter's `Order` and this one ever disagree about `quote_budget`'s
/// field number, its wire type, or whether an unset one is emitted, one of
/// these fails loudly instead of the venue silently refusing market bids.
#[cfg(feature = "client")]
mod order_wire {
    use aspens::commands::trading::send_order::arborter_pb::Order;
    use prost::Message;

    const QUANTITY: &str = "100000000";
    const MARKET_ID: &str = "base-net::0xbase::quote-net::0xquote";
    const BASE_ACCOUNT: &str = "0xbase0000000000000000000000000000000000ba";
    const QUOTE_ACCOUNT: &str = "0xquote000000000000000000000000000000000q";
    /// 7.5 quote at 6 decimals — the quote token's OWN base units.
    const BUDGET: &str = "7500000";

    /// A market BID: side BID, no price. The one cell that must state a
    /// budget, and so the only order that puts field 11 on the wire.
    fn market_bid(quote_budget: Option<&str>) -> Order {
        Order {
            side: 1, // SIDE_BID
            quantity: QUANTITY.to_string(),
            price: None, // market order
            market_id: MARKET_ID.to_string(),
            base_account_address: BASE_ACCOUNT.to_string(),
            quote_account_address: QUOTE_ACCOUNT.to_string(),
            execution_type: 0,
            matching_order_ids: vec![],
            post_only: false,
            hidden: false,
            quote_budget: quote_budget.map(str::to_string),
        }
    }

    fn encode(order: &Order) -> Vec<u8> {
        let mut buf = Vec::new();
        order
            .encode(&mut buf)
            .expect("encoding an Order cannot fail");
        buf
    }

    /// One length-delimited (wire type 2) string field, hand-encoded.
    /// Every payload here is < 128 bytes, so the length is a single varint
    /// byte — deliberately, to keep the expectation readable.
    fn string_field(field_number: u8, value: &str) -> Vec<u8> {
        assert!(value.len() < 128, "fixture strings stay in one length byte");
        let mut out = vec![(field_number << 3) | 2, value.len() as u8];
        out.extend_from_slice(value.as_bytes());
        out
    }

    /// The full encoding of a market bid, built from the proto spec by hand:
    /// side (1, varint), quantity (2), market_id (4), base/quote accounts
    /// (5, 6), quote_budget (11) — with every proto3-default field
    /// (price=None, execution_type=0, no matching ids, both bools false)
    /// emitting nothing at all.
    fn expected_market_bid_bytes(quote_budget: Option<&str>) -> Vec<u8> {
        let mut want = vec![0x08, 0x01]; // field 1, varint, SIDE_BID
        want.extend(string_field(2, QUANTITY));
        want.extend(string_field(4, MARKET_ID));
        want.extend(string_field(5, BASE_ACCOUNT));
        want.extend(string_field(6, QUOTE_ACCOUNT));
        if let Some(budget) = quote_budget {
            want.extend(string_field(11, budget));
        }
        want
    }

    /// `quote_budget` was APPENDED as field 11, so an order that doesn't set
    /// it (every order but a market bid) encodes to exactly the bytes it
    /// encoded to before the field existed — every signature already in
    /// flight stays valid.
    #[test]
    fn unset_quote_budget_emits_nothing() {
        assert_eq!(encode(&market_bid(None)), expected_market_bid_bytes(None));
    }

    /// And when it IS set, it reaches the wire as field 11 — a regression
    /// that dropped it would otherwise sign an unbounded order while the
    /// caller believed a cap applied.
    #[test]
    fn set_quote_budget_encodes_as_field_11() {
        assert_eq!(
            encode(&market_bid(Some(BUDGET))),
            expected_market_bid_bytes(Some(BUDGET))
        );
        // Stated as an append, too: the budget adds bytes at the end and
        // moves nothing before it.
        let mut appended = encode(&market_bid(None));
        appended.extend(string_field(11, BUDGET));
        assert_eq!(encode(&market_bid(Some(BUDGET))), appended);
    }

    /// The exact bytes the arborter must reconstruct for this order. Pinned
    /// as hex so the arborter side can be compared against a literal rather
    /// than against another copy of the same generated code.
    #[test]
    fn market_bid_encoding_snapshot() {
        assert_eq!(
            hex::encode(encode(&market_bid(Some(BUDGET)))),
            "080112093130303030303030302224626173652d6e65743a3a3078626173653a3a71756f74652d6e65743a3a307871756f74652a2a3078626173653030303030303030303030303030303030303030303030303030303030303030303062613229307871756f7465303030303030303030303030303030303030303030303030303030303030303030715a0737353030303030"
        );
    }

    /// A budget is not a restatement of the quantity: the two are different
    /// numbers in different tokens, and swapping them would sign a spend cap
    /// nobody asked for. Pin that they occupy different fields.
    #[test]
    fn budget_and_quantity_are_distinct_fields() {
        let bytes = encode(&market_bid(Some(BUDGET)));
        let decoded = Order::decode(&*bytes).expect("round-trips");
        assert_eq!(decoded.quantity, QUANTITY);
        assert_eq!(decoded.quote_budget.as_deref(), Some(BUDGET));
    }
}
