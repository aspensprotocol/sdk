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

/// The RECIPE, pinned over synthetic arguments.
///
/// This says the eight fields are concatenated and hashed the way the arborter
/// concatenates and hashes them. It says NOTHING about how the SDK assembles
/// those eight from an order — which side is origin, which token is the input,
/// what scale each amount is in — because the arguments here are placeholders
/// handed straight to the primitive. `order_id_composition_parity` below covers
/// the assembly; keep both.
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

/// The COMPOSITION, pinned against the vector the arborter and the browser SDK
/// both pin — driven through the real assembly path.
///
/// The snapshot above cannot catch a client that hashes the base token where
/// the quote belongs, resolves the origin chain from the wrong side of the
/// market, normalises an amount to pair decimals instead of the token's, or
/// reads a token address out of the `tokens` table instead of `market_id`.
/// Every one of those produces a well-formed id that no venue issues, on an
/// order the venue accepts without complaint. So this goes through
/// `build_order_commitment` — the function a caller actually uses — and lets it
/// pick all eight arguments itself.
#[cfg(feature = "client")]
mod order_id_composition_parity {
    use std::collections::HashMap;

    use aspens::Wallet;
    use aspens::commands::config::config_pb::{
        Chain, Configuration, GetConfigResponse, Market, RpcEndpoint, Token, TradeContract,
    };
    use aspens::commands::trading::gasless::build_order_commitment;

    /// **The cross-repo constant. Do not change it here alone.**
    ///
    /// The same limit BID is pinned to this digest in
    /// `arborter/app/server/src/handlers/order_id.rs`
    /// (`sdk_parity_known_good_vector`) and in `terminal-ui`'s
    /// `packages/sdk-typescript/src/order-commitment.test.ts`. Three
    /// independent implementations agree on it; changing one is changing the
    /// wire contract, and the other two must move in the same breath or every
    /// order placed by the odd one out is refused with nothing but an id to
    /// diagnose it by.
    const KNOWN_GOOD_ORDER_ID: &str =
        "0xc6d8846de54b5b1b2513fa5fb8b535ea48bbe2835c17187e6564443aaa774f2d";

    // The fixture's three decimal scales, mutually distinct on purpose: with
    // base == quote == pair every denomination collapses to one integer and
    // the vector stops telling a correct normalisation from a wrong one.
    const PAIR: i32 = 12;
    const BASE: u32 = 18;
    const QUOTE: u32 = 6;

    // A bid locks on the QUOTE chain and receives on the base chain, so the
    // quote chain is the ORIGIN. Two different ids, so a transposition shows.
    const BASE_CHAIN_ID: u32 = 990_001;
    const QUOTE_CHAIN_ID: u32 = 990_002;

    /// The token addresses as `market_id` spells them — the exact substrings
    /// the arborter cuts and hashes.
    const BASE_ADDR: &str = "0x00000000000000000000000000000000000000b1";
    const QUOTE_ADDR: &str = "0x00000000000000000000000000000000000000c1";
    /// The same two addresses as the `tokens` table spells them. Different
    /// strings, deliberately: the id hashes the address TEXT, so a fixture
    /// where both sources agree cannot tell the right source from the wrong
    /// one — and reading `tokens` is exactly the bug this pins shut.
    const BASE_ADDR_IN_TOKENS_TABLE: &str = "0x00000000000000000000000000000000000000B1";
    const QUOTE_ADDR_IN_TOKENS_TABLE: &str = "0x00000000000000000000000000000000000000C1";

    /// Anvil key #0, whose address is
    /// `0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266` — the address the arborter
    /// vector hashes. A dev key, not a secret.
    const TEST_EVM_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    const NONCE: u64 = 1_723_000_000_000;
    /// 1.5 base, in the market's 12 pair decimals.
    const QUANTITY: &str = "1500000000000";
    /// 2.0 quote per base, same 12 pair decimals.
    const PRICE: &str = "2000000000000";

    fn token(symbol: &str, address: &str, decimals: u32) -> (String, Token) {
        (
            symbol.to_string(),
            Token {
                name: symbol.to_string(),
                symbol: symbol.to_string(),
                address: address.to_string(),
                token_id: None,
                decimals,
            },
        )
    }

    fn chain(network: &str, chain_id: u32, tokens: HashMap<String, Token>) -> Chain {
        Chain {
            architecture: "evm".into(),
            canonical_name: network.into(),
            network: network.into(),
            chain_id,
            instance_signer_address: "0x0000000000000000000000000000000000000001".into(),
            explorer_url: None,
            rpcs: vec![RpcEndpoint {
                label: "primary".into(),
                url: "http://localhost".into(),
                enabled: true,
                ..Default::default()
            }],
            factory_address: "0xfactory".into(),
            trade_contract: Some(TradeContract {
                contract_id: None,
                address: "0xtradecontract".into(),
            }),
            tokens,
            finality: 0,
            finality_confirmations: 0,
        }
    }

    fn fixture() -> (GetConfigResponse, Market) {
        let market = Market {
            name: "BASE/QUOTE".into(),
            base_chain_network: "base-net".into(),
            quote_chain_network: "quote-net".into(),
            base_chain_token_symbol: "BASE".into(),
            quote_chain_token_symbol: "QUOTE".into(),
            base_chain_token_decimals: BASE as i32,
            quote_chain_token_decimals: QUOTE as i32,
            pair_decimals: PAIR,
            market_id: format!("base-net::{BASE_ADDR}::quote-net::{QUOTE_ADDR}"),
        };
        let config = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![
                    chain(
                        "base-net",
                        BASE_CHAIN_ID,
                        HashMap::from([token("BASE", BASE_ADDR_IN_TOKENS_TABLE, BASE)]),
                    ),
                    chain(
                        "quote-net",
                        QUOTE_CHAIN_ID,
                        HashMap::from([token("QUOTE", QUOTE_ADDR_IN_TOKENS_TABLE, QUOTE)]),
                    ),
                ],
                markets: vec![market.clone()],
            }),
        };
        (config, market)
    }

    fn commitment(market: &Market) -> aspens::commands::trading::gasless::OrderCommitment {
        let (config, _) = fixture();
        let wallet = Wallet::from_evm_hex(TEST_EVM_KEY).expect("anvil key #0 parses");
        build_order_commitment(
            &config,
            market,
            1, // SIDE_BID
            &wallet,
            QUANTITY,
            Some(PRICE),
            None, // a limit bid derives its budget; a stated one is refused
            NONCE,
        )
        .expect("the fixture order resolves")
    }

    /// Guard the guards. Each premise the vector rests on, asserted before it
    /// is relied on — a fixture that quietly loses one of these keeps passing
    /// while testing less.
    #[test]
    fn the_fixture_can_distinguish_what_it_claims_to() {
        assert_ne!(PAIR as u32, BASE);
        assert_ne!(PAIR as u32, QUOTE);
        assert_ne!(BASE, QUOTE);
        assert_ne!(BASE_CHAIN_ID, QUOTE_CHAIN_ID);
        assert_ne!(BASE_ADDR, QUOTE_ADDR);
        // The two sources hold the same address in two spellings. Equal
        // strings would make this vector blind to which one gets hashed.
        assert_ne!(BASE_ADDR, BASE_ADDR_IN_TOKENS_TABLE);
        assert_ne!(QUOTE_ADDR, QUOTE_ADDR_IN_TOKENS_TABLE);
        assert!(BASE_ADDR.eq_ignore_ascii_case(BASE_ADDR_IN_TOKENS_TABLE));
        assert!(QUOTE_ADDR.eq_ignore_ascii_case(QUOTE_ADDR_IN_TOKENS_TABLE));
        assert_eq!(
            Wallet::from_evm_hex(TEST_EVM_KEY).unwrap().address(),
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
            "the vector is bound to this signer's address"
        );
    }

    /// The budget the id is hashed over, written out — so a scaling change is
    /// caught here and not only as an opaque digest mismatch below.
    #[test]
    fn the_budget_is_the_product_in_quote_units() {
        let (_, market) = fixture();
        assert_eq!(
            commitment(&market).input_amount,
            3_000_000,
            "1.5 base x 2.0 = 3.0 quote, at the QUOTE token's 6 decimals — not \
             at pair's 12 and not at base's 18"
        );
    }

    /// The vector itself.
    #[test]
    fn the_composed_order_id_matches_the_cross_repo_vector() {
        let (_, market) = fixture();
        assert_eq!(
            commitment(&market).order_id,
            KNOWN_GOOD_ORDER_ID,
            "the SDK composed a different id than the arborter and terminal-ui \
             derive for this order. Do not update this constant alone — see its \
             doc comment."
        );
    }

    /// And the token strings came out of `market_id`. Re-spell them there —
    /// changing nothing else, and leaving the `tokens` rows alone — and the id
    /// must move. A client reading `Token.address` would return the vector for
    /// both markets, which is precisely the drift that cannot be seen from the
    /// wire.
    #[test]
    fn the_id_follows_the_market_id_spelling_not_the_tokens_row() {
        let (_, market) = fixture();
        let mut recased = market.clone();
        recased.market_id = format!(
            "base-net::{BASE_ADDR_IN_TOKENS_TABLE}::quote-net::{QUOTE_ADDR_IN_TOKENS_TABLE}"
        );
        assert_ne!(
            commitment(&recased).order_id,
            commitment(&market).order_id,
            "re-spelling market_id must move the id — if it does not, the token \
             strings are being read from somewhere else"
        );
        assert_ne!(commitment(&recased).order_id, KNOWN_GOOD_ORDER_ID);
    }
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
    use aspens::commands::trading::arborter_pb::Order;
    use prost::Message;

    const QUANTITY: &str = "100000000";
    const MARKET_ID: &str = "base-net::0xbase::quote-net::0xquote";
    const BASE_ACCOUNT: &str = "0xbase0000000000000000000000000000000000ba";
    const QUOTE_ACCOUNT: &str = "0xquote000000000000000000000000000000000q";
    /// 7.5 quote at 6 decimals — the quote token's OWN base units.
    const BUDGET: &str = "7500000";

    /// A nonce that needs a multi-byte varint (six bytes), so a truncating or
    /// single-byte encoding fails rather than coincides. Realistic, too: it is
    /// millis-since-epoch, which is what `gasless::client_nonce` produces.
    const NONCE: u64 = 1_700_000_000_042;

    /// A market BID: side BID, no price. The one cell that must state a
    /// budget, and so the only order that puts field 11 on the wire.
    fn market_bid(quote_budget: Option<&str>) -> Order {
        market_bid_with_nonce(quote_budget, 0)
    }

    fn market_bid_with_nonce(quote_budget: Option<&str>, nonce: u64) -> Order {
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
            nonce,
        }
    }

    fn encode(order: &Order) -> Vec<u8> {
        let mut buf = Vec::new();
        order
            .encode(&mut buf)
            .expect("encoding an Order cannot fail");
        buf
    }

    /// One varint (wire type 0) field, hand-encoded — base-128, low group
    /// first, continuation bit on every byte but the last. Written out rather
    /// than borrowed from prost so the expectation is independent of the code
    /// under test.
    fn varint_field(field_number: u8, mut value: u64) -> Vec<u8> {
        let mut out = vec![field_number << 3];
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
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

    /// `nonce` was appended as field 12, varint. The arborter now hashes it
    /// into the canonical order id, reading it off these very bytes — so if
    /// the two sides ever disagree about its number or wire type, the id the
    /// client tracks and the id the venue issues diverge for an order whose
    /// signature verifies fine. Hand-built expectation, same rationale as the
    /// budget above.
    #[test]
    fn set_nonce_encodes_as_field_12_varint() {
        let mut want = expected_market_bid_bytes(Some(BUDGET));
        want.extend(varint_field(12, NONCE));
        assert_eq!(encode(&market_bid_with_nonce(Some(BUDGET), NONCE)), want);

        // And it decodes back to the same u64 — a truncation to u32 or i32
        // would survive the append check on a small value but not this one.
        let decoded =
            Order::decode(&*encode(&market_bid_with_nonce(Some(BUDGET), NONCE))).expect("decodes");
        assert_eq!(decoded.nonce, NONCE);
    }

    /// nonce = 0 emits nothing, so the FCE direct-action path — whose wire
    /// has no nonce field, and whose adapter therefore rebuilds `Order` with
    /// the default — signs bytes the arborter can reproduce. It also means
    /// every signature cut before field 12 existed is still valid.
    #[test]
    fn unset_nonce_emits_nothing() {
        assert_eq!(
            encode(&market_bid_with_nonce(Some(BUDGET), 0)),
            expected_market_bid_bytes(Some(BUDGET)),
        );
    }
}

// -- Cancel wire encoding: what the cancel signature is taken over --------

/// The arborter verifies a cancel's `signature_hash` against
/// `order_to_cancel.encode(&buf)` — the prost encoding of `OrderToCancel`.
/// Pin the bytes by field number so a renumber or a re-added field (e.g. a
/// resurrected `token_address`) changes this literal instead of silently
/// changing every client's signature.
#[cfg(feature = "client")]
mod cancel_wire {
    use aspens::commands::trading::arborter_pb::{OrderToCancel, Side};
    use prost::Message;

    /// The cancel signature is over the prost encoding of `OrderToCancel`.
    /// Values are distinct per field on purpose.
    #[test]
    fn order_to_cancel_encoding_snapshot() {
        let msg = OrderToCancel {
            market_id: "a::0x1::b::0x2".to_string(),
            side: Side::Ask as i32,
            order_id: (1u8..=32).collect(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        // field 1 (string, tag 0x0a) len 14 | field 2 (varint, tag 0x10) = 2 |
        // field 3 (bytes, tag 0x1a) len 32
        let expected = {
            let mut v = vec![0x0a, 14];
            v.extend_from_slice(b"a::0x1::b::0x2");
            v.extend_from_slice(&[0x10, 0x02, 0x1a, 32]);
            v.extend(1u8..=32);
            v
        };
        assert_eq!(buf, expected);
    }
}
