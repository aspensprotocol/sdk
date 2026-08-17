//! Build the `OrderAuthorization` proto payload for a `SendOrderRequest`.
//!
//! Stateless — no gRPC, no arborter round-trip, no chain RPC. Pure data.
//!
//! Under the optimistic shadow ledger, order entry never touches the chain:
//! the arborter authenticates the order via the **outer envelope** signature
//! (`aspens::evm::sign_send_order_envelope`) and consumes exactly one field
//! from this payload — the canonical `order_id`
//! (`aspens::orders::derive_order_id`). The legacy gasless on-chain-lock
//! signing (EVM EIP-712 `GaslessCrossChainOrder` / Solana
//! `OpenForSignedPayload`) is gone with the on-chain order machinery, so this
//! helper no longer signs a lock or dispatches per chain architecture.
//!
//! # The one rule
//!
//! **An order commits a budget, denominated in the asset it gives.** That
//! budget is `OrderCommitment::input_amount` here, and it is what the id is
//! derived over:
//!
//! | order      | gives | budget                 | derivable from (quantity, price)? |
//! |------------|-------|------------------------|-----------------------------------|
//! | limit BID  | quote | `quantity x price`     | yes                               |
//! | limit ASK  | base  | `quantity`             | yes                               |
//! | market ASK | base  | `quantity`             | yes                               |
//! | market BID | quote | `Order.quote_budget`   | **no** — it must be stated        |
//!
//! `quote_budget` is REQUIRED for a market BID and REJECTED on the other three
//! cells, matching the arborter's `quote_budget_for_cell`. It rides inside
//! `Order`, so the envelope signature covers it.
//!
//! # Usage sketch
//!
//! ```ignore
//! use aspens::commands::trading::gasless::build_gasless_authorization;
//!
//! let commitment = build_gasless_authorization(
//!     &config, market, side, &wallet, &quantity_raw, price_raw.as_deref(),
//!     quote_budget_raw.as_deref(),
//! )?;
//! request.authorization = Some(commitment.authorization);
//! ```
//!
//! See also `aspens::orders::derive_order_id`.

#![cfg(feature = "client")]

use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{Result, eyre};

use crate::commands::config::config_pb::{Chain, GetConfigResponse, Market};
use crate::orders::derive_order_id;
use crate::wallet::{CurveType, Wallet};

use super::send_order::arborter_pb::OrderAuthorization;

/// What [`build_gasless_authorization`] produces: the wire payload plus the
/// budget it was derived over.
///
/// `input_amount` is **not** on the gRPC wire — `OrderAuthorization.amount_in`
/// was deleted, field number and all, because it sat in a sibling of `Order`
/// that the envelope signature never covered. It is still the id's
/// `input_amount`, and the FCE direct-action JSON still carries it (see
/// [`crate::commands::trading::fce_actions`]), so it is returned rather than
/// recomputed by callers that need it.
#[derive(Debug, Clone)]
pub struct OrderCommitment {
    /// The payload for `SendOrderRequest.authorization`.
    pub authorization: OrderAuthorization,
    /// The order's budget — how much of the asset it GIVES it commits — in
    /// that token's native base units (NOT pair decimals). Quote for a bid,
    /// base for an ask; for a market bid it is the stated `quote_budget`
    /// verbatim.
    pub input_amount: u128,
}

/// Build an `OrderAuthorization` for the given order.
///
/// Resolves the order's chains/tokens/budget and derives the canonical
/// `order_id`. The returned payload carries the single field the arborter
/// still consumes, `order_id`; the budget it was derived over comes back
/// alongside it in [`OrderCommitment::input_amount`]. Order authentication is
/// via the outer envelope signature, not a per-order on-chain lock signature.
///
/// `quote_budget_raw` is the market-BID budget in the QUOTE token's native
/// base units (decimal `u128`, the same string that goes into
/// `Order.quote_budget`). It is required for a market BID and rejected on
/// every other cell — see the module header.
pub fn build_gasless_authorization(
    config: &GetConfigResponse,
    market: &Market,
    side: i32,
    wallet: &Wallet,
    quantity_raw: &str,
    price_raw: Option<&str>,
    quote_budget_raw: Option<&str>,
) -> Result<OrderCommitment> {
    let OrderResolution {
        origin_chain,
        destination_chain,
        input_token_address,
        output_token_address,
        amount_in,
        amount_out,
    } = resolve_order(
        config,
        market,
        side,
        quantity_raw,
        price_raw,
        quote_budget_raw,
    )?;

    // Client nonce: millis-since-epoch. Folded into `derive_order_id` purely
    // to keep the derived id unique across a wallet's orders (millis gives
    // 1000× collision headroom over a unix-seconds scheme).
    let nonce = unix_millis()?;

    let order_id_bytes = derive_order_id(
        wallet_pubkey_bytes(wallet).as_slice(),
        nonce,
        origin_chain.chain_id as u64,
        destination_chain.chain_id as u64,
        input_token_address.as_bytes(),
        output_token_address.as_bytes(),
        amount_in,
        amount_out,
    );
    let order_id_hex = format!("0x{}", hex::encode(order_id_bytes));

    Ok(OrderCommitment {
        authorization: OrderAuthorization {
            order_id: order_id_hex,
        },
        input_amount: amount_in,
    })
}

#[cfg_attr(test, derive(Debug))]
struct OrderResolution<'a> {
    origin_chain: &'a Chain,
    destination_chain: &'a Chain,
    input_token_address: String,
    output_token_address: String,
    /// The order's budget, in the origin chain's input-token native base
    /// units (e.g. 1_000_000 for 1 USDC at 6 decimals). NOT pair decimals.
    /// This is the number `derive_order_id` hashes as `input_amount`, so
    /// SDK and arborter must agree on the scale. See `normalize` below for
    /// the conversion from the matching-engine's pair-decimal representation
    /// — a market bid needs none, because `quote_budget` arrives already
    /// denominated in the quote token's own units.
    amount_in: u128,
    /// Expected output amount in the destination chain's output-token
    /// native base units. Same scale convention as `amount_in`. **Zero for a
    /// market order**: with no price there is no honest expected output, and
    /// zero is how "unknown at signing time" is encoded here.
    amount_out: u128,
}

/// Convert an integer expressed in `from_decimals` to `to_decimals`.
///
/// - `from == to` → identity.
/// - `from >  to` → divide by `10^(from-to)` (truncation toward zero).
/// - `from <  to` → multiply by `10^(to-from)`, with overflow check.
///
/// Mirrors `arborter/app/chain-traits/src/convert_decimals.rs::normalize_decimals`
/// so the SDK and arborter agree on the scale of every amount that flows
/// through the EIP-712 / Ed25519 digest.
fn normalize(amount: u128, from_decimals: u32, to_decimals: u32) -> Result<u128> {
    use std::cmp::Ordering;
    match from_decimals.cmp(&to_decimals) {
        Ordering::Equal => Ok(amount),
        Ordering::Greater => {
            let scale = 10u128
                .checked_pow(from_decimals - to_decimals)
                .ok_or_else(|| {
                    eyre!(
                        "normalize scale 10^{} overflows u128",
                        from_decimals - to_decimals
                    )
                })?;
            Ok(amount / scale)
        }
        Ordering::Less => {
            let scale = 10u128
                .checked_pow(to_decimals - from_decimals)
                .ok_or_else(|| {
                    eyre!(
                        "normalize scale 10^{} overflows u128",
                        to_decimals - from_decimals
                    )
                })?;
            amount.checked_mul(scale).ok_or_else(|| {
                eyre!(
                    "normalize: {amount} * 10^{} overflows u128",
                    to_decimals - from_decimals
                )
            })
        }
    }
}

/// Which cell of the order table this is, and therefore whether
/// `Order.quote_budget` is required, forbidden, and what it means.
///
/// Mirrors the arborter's `quote_budget_for_cell` (`handlers/send_order.rs`):
/// a market BID must state its budget, every other cell derives one and must
/// not state it. Rejecting here saves a round-trip and gives the same answer
/// the arborter would.
///
/// Returns the stated budget in the quote token's native base units, `None`
/// for the three deriving cells.
fn quote_budget_for_cell(
    side: i32,
    price_raw: Option<&str>,
    stated: Option<&str>,
) -> Result<Option<u128>> {
    let is_market_bid = side == 1 && price_raw.is_none();
    match (is_market_bid, stated) {
        (true, None) => Err(eyre!(
            "a market BID (side BID, no price) must set Order.quote_budget: it gives \
             quote and has no price to size that with, so nothing else bounds what it \
             may spend. Pass the maximum quote you are prepared to spend — or pass a \
             price, making it a limit bid"
        )),
        (true, Some(budget)) => {
            let parsed = budget
                .parse::<u128>()
                .map_err(|e| eyre!("Order.quote_budget {budget:?} is not a u128 decimal: {e}"))?;
            if parsed == 0 {
                return Err(eyre!(
                    "Order.quote_budget must be greater than zero: a market BID with \
                     nothing to spend can buy nothing"
                ));
            }
            Ok(Some(parsed))
        }
        (false, None) => Ok(None),
        (false, Some(_)) if side == 1 => Err(eyre!(
            "Order.quote_budget is only for a MARKET bid. A limit bid's budget is \
             derived — it is `quantity x price`, already signed — so drop the budget, \
             or drop the price to make this a market bid sized in quote"
        )),
        (false, Some(_)) => Err(eyre!(
            "Order.quote_budget is only for a MARKET bid. An ASK gives base, and its \
             budget IS its `quantity`, so drop the budget"
        )),
    }
}

fn resolve_order<'a>(
    config: &'a GetConfigResponse,
    market: &Market,
    side: i32,
    quantity_raw: &str,
    price_raw: Option<&str>,
    quote_budget_raw: Option<&str>,
) -> Result<OrderResolution<'a>> {
    // Arborter handler convention: Bid = buying base, locks on quote
    // chain. Ask = selling base, locks on base chain.
    let (origin_net, origin_sym, dest_net, dest_sym) = match side {
        1 => (
            &market.quote_chain_network,
            &market.quote_chain_token_symbol,
            &market.base_chain_network,
            &market.base_chain_token_symbol,
        ),
        2 => (
            &market.base_chain_network,
            &market.base_chain_token_symbol,
            &market.quote_chain_network,
            &market.quote_chain_token_symbol,
        ),
        other => {
            return Err(eyre!(
                "unsupported side {other} — expected 1 (Bid) or 2 (Ask)"
            ));
        }
    };

    let origin_chain = config
        .get_chain(origin_net)
        .ok_or_else(|| eyre!("origin chain {origin_net:?} not found in config"))?;
    let destination_chain = config
        .get_chain(dest_net)
        .ok_or_else(|| eyre!("destination chain {dest_net:?} not found in config"))?;
    let input_token = config
        .get_token(origin_net, origin_sym)
        .ok_or_else(|| eyre!("token {origin_sym} on {origin_net} not found"))?;
    let output_token = config
        .get_token(dest_net, dest_sym)
        .ok_or_else(|| eyre!("token {dest_sym} on {dest_net} not found"))?;
    let input_decimals = input_token.decimals;
    let output_decimals = output_token.decimals;
    let pair_decimals = market.pair_decimals as u32;

    let quantity: u128 = quantity_raw
        .parse()
        .map_err(|e| eyre!("quantity_raw {quantity_raw:?} is not a u128: {e}"))?;
    let price: Option<u128> = price_raw
        .map(|s| {
            s.parse::<u128>()
                .map_err(|e| eyre!("price_raw {s:?} is not a u128: {e}"))
        })
        .transpose()?;

    // Required for a market bid, refused everywhere else.
    let quote_budget = quote_budget_for_cell(side, price_raw, quote_budget_raw)?;

    // The matching engine works in pair-decimals throughout; quantity
    // and price arrive here as pair-decimal-scaled u128s. The order id,
    // however, is hashed over NATIVE base units. Normalise both sides of
    // the trade so the SDK hashes the same integers the arborter would.
    // For markets where pair_decimals != input_decimals or
    // != output_decimals (e.g. WFLR/USDC: pair=18, USDC=6), skipping
    // this normalisation produced digests that were N orders of
    // magnitude off and the on-chain `ecrecover` returned a nonsense
    // address.
    //
    // ONE rule across all four cells: `amount_in` is the order's budget,
    // in the asset it gives. `amount_out` is what it expects back — and a
    // market order, having no price, expects nothing quantifiable, so it
    // is zero there rather than a guess.
    let (amount_in, amount_out) = match (side, price) {
        // Limit bid: pay quote = qty * price (in input=quote decimals),
        //            receive base = qty (in output=base decimals).
        (1, Some(price)) => {
            let qty_quote_pair2 = quantity
                .checked_mul(price)
                .ok_or_else(|| eyre!("amount_in overflow: {quantity} * {price}"))?;
            (
                normalize(qty_quote_pair2, pair_decimals * 2, input_decimals)?,
                normalize(quantity, pair_decimals, output_decimals)?,
            )
        }
        // Market bid: the budget is stated, not derived, and `quote_budget`
        // is ALREADY in the quote token's native base units (the input
        // token here) — so it is taken verbatim, with no normalisation.
        // Re-scaling it would be the WFLR/USDC bug in reverse. The base
        // quantity bounds nothing on this cell and buys nothing knowable,
        // so the expected output is zero.
        (1, None) => (
            quote_budget.expect("quote_budget_for_cell requires one for a market bid"),
            0,
        ),
        // Limit ask: pay base = qty (in input=base decimals),
        //            receive quote = qty * price (in output=quote decimals).
        (2, Some(price)) => {
            let qty_quote_pair2 = quantity
                .checked_mul(price)
                .ok_or_else(|| eyre!("amount_out overflow: {quantity} * {price}"))?;
            (
                normalize(quantity, pair_decimals, input_decimals)?,
                normalize(qty_quote_pair2, pair_decimals * 2, output_decimals)?,
            )
        }
        // Market ask: gives base, and its budget IS its quantity whatever it
        // trades at — derivable, price or no price.
        (2, None) => (normalize(quantity, pair_decimals, input_decimals)?, 0),
        _ => unreachable!("side validated above"),
    };

    Ok(OrderResolution {
        origin_chain,
        destination_chain,
        input_token_address: input_token.address.clone(),
        output_token_address: output_token.address.clone(),
        amount_in,
        amount_out,
    })
}

fn wallet_pubkey_bytes(wallet: &Wallet) -> Vec<u8> {
    // EVM: 20-byte address. Solana: 32-byte Ed25519 pubkey. The
    // `derive_order_id` hash treats the pubkey as opaque bytes so both
    // chains pass their canonical form.
    match wallet.curve() {
        CurveType::Secp256k1 => {
            // 0x-prefixed 20-byte hex — strip prefix, decode.
            let s = wallet.address();
            let trimmed = s.strip_prefix("0x").unwrap_or(&s);
            hex::decode(trimmed).unwrap_or_default()
        }
        CurveType::Ed25519 => bs58::decode(wallet.address())
            .into_vec()
            .unwrap_or_default(),
    }
}

fn unix_millis() -> Result<u64> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| eyre!("system clock before epoch: {e}"))?
        .as_millis();
    u64::try_from(ms).map_err(|_| eyre!("unix millis overflow"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // ----- normalize() decimal-scale helper ----------------------------

    #[test]
    fn normalize_identity_when_decimals_match() {
        assert_eq!(normalize(123_456_789, 6, 6).unwrap(), 123_456_789);
        assert_eq!(normalize(0, 18, 18).unwrap(), 0);
        assert_eq!(normalize(u128::MAX, 6, 6).unwrap(), u128::MAX);
    }

    #[test]
    fn normalize_downscales_with_truncation() {
        // 1.0 USDC at 18 decimals → 1.0 USDC at 6 decimals.
        assert_eq!(
            normalize(1_000_000_000_000_000_000, 18, 6).unwrap(),
            1_000_000
        );
        // Truncation, no rounding: dust below 10^12 is lost.
        assert_eq!(normalize(999_999_999_999, 18, 6).unwrap(), 0);
        assert_eq!(normalize(1_999_999_999_999, 18, 6).unwrap(), 1);
    }

    #[test]
    fn normalize_upscales_within_u128() {
        // 1.0 USDC at 6 decimals → 1.0 at 18 decimals.
        assert_eq!(
            normalize(1_000_000, 6, 18).unwrap(),
            1_000_000_000_000_000_000
        );
    }

    #[test]
    fn normalize_upscale_overflow_errors_cleanly() {
        // 10^20 cannot fit a 10^20 multiplier.
        assert!(normalize(u128::MAX, 0, 40).is_err());
    }

    // ----- resolve_order: scale conversions ---------------------------
    //
    // These tests pin the integer values the SDK signs for representative
    // market shapes. The arborter must produce the same integers when it
    // rebuilds the order; any drift here is the WFLR/USDC class of bug
    // we hit in cross-chain testing.

    /// Shared with `send_order`'s tests: one fixture, so a market that
    /// exercises the budget rule is described once.
    pub(crate) fn config_with_market(
        base_dec: u32,
        quote_dec: u32,
        pair_dec: i32,
    ) -> (GetConfigResponse, Market) {
        use crate::commands::config::config_pb::{Chain, Configuration, Market, TradeContract};
        use std::collections::HashMap;

        let mut base_tokens = HashMap::new();
        base_tokens.insert(
            "BASE".to_string(),
            crate::commands::config::config_pb::Token {
                name: "Base".into(),
                symbol: "BASE".into(),
                address: "0xbase".into(),
                token_id: None,
                decimals: base_dec,
            },
        );
        let mut quote_tokens = HashMap::new();
        quote_tokens.insert(
            "QUOTE".to_string(),
            crate::commands::config::config_pb::Token {
                name: "Quote".into(),
                symbol: "QUOTE".into(),
                address: "0xquote".into(),
                token_id: None,
                decimals: quote_dec,
            },
        );

        let base_chain = Chain {
            architecture: "evm".into(),
            canonical_name: "base-chain".into(),
            network: "base-net".into(),
            chain_id: 1,
            instance_signer_address: "0x0000000000000000000000000000000000000001".into(),
            explorer_url: None,
            rpc_url: "http://localhost".into(),
            factory_address: "0xfactory".into(),
            trade_contract: Some(TradeContract {
                contract_id: None,
                address: "0xtradecontract".into(),
            }),
            tokens: base_tokens,
            // 0 = FINALITY_POLICY_UNSPECIFIED, which the arborter resolves to
            // FINALIZED. Deposit finality is irrelevant to this fixture.
            finality: 0,
            finality_confirmations: 0,
        };
        let quote_chain = Chain {
            architecture: "evm".into(),
            canonical_name: "quote-chain".into(),
            network: "quote-net".into(),
            chain_id: 2,
            instance_signer_address: "0x0000000000000000000000000000000000000002".into(),
            explorer_url: None,
            rpc_url: "http://localhost".into(),
            factory_address: "0xfactory".into(),
            trade_contract: Some(TradeContract {
                contract_id: None,
                address: "0xtradecontract".into(),
            }),
            tokens: quote_tokens,
            // 0 = FINALITY_POLICY_UNSPECIFIED, which the arborter resolves to
            // FINALIZED. Deposit finality is irrelevant to this fixture.
            finality: 0,
            finality_confirmations: 0,
        };

        let market = Market {
            name: "BASE/QUOTE".into(),
            base_chain_network: "base-net".into(),
            quote_chain_network: "quote-net".into(),
            base_chain_token_symbol: "BASE".into(),
            quote_chain_token_symbol: "QUOTE".into(),
            base_chain_token_decimals: base_dec as i32,
            quote_chain_token_decimals: quote_dec as i32,
            pair_decimals: pair_dec,
            market_id: "base-net::0xbase::quote-net::0xquote".into(),
        };
        let config = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![base_chain, quote_chain],
                markets: vec![market.clone()],
            }),
        };
        (config, market)
    }

    /// A market with three DIFFERENT decimal scales, so no test written
    /// against it can pass by coincidence: base 18, quote 6, pair 8. Every
    /// budget below is a different integer in each of the three, which is
    /// exactly what a same-decimals fixture cannot tell apart.
    const BASE_DEC: u32 = 18;
    const QUOTE_DEC: u32 = 6;
    const PAIR_DEC: i32 = 8;

    #[test]
    fn resolve_buy_limit_same_decimals_market() {
        // pair=quote=base=6 (USDT0/USDC class). qty 0.1 @ price 1.0:
        // quantity_pair = 100_000, price_pair = 1_000_000.
        // Bid: amount_in (quote) = qty*price normalised pair*2(=12) → 6 = 100_000.
        //      amount_out (base) = qty normalised pair(=6) → 6 = 100_000.
        let (config, market) = config_with_market(6, 6, 6);
        let r = resolve_order(&config, &market, 1, "100000", Some("1000000"), None).unwrap();
        assert_eq!(r.amount_in, 100_000);
        assert_eq!(r.amount_out, 100_000);
    }

    #[test]
    fn resolve_buy_limit_high_pair_decimals_market() {
        // pair=18, quote=6, base=18 (WFLR-on-Coston2 / USDC-on-Solana). The
        // SDK used to sign in pair_decimals (10^17 for 0.1) while the
        // arborter rebuilt in quote_token_decimals (10^5) — 12 orders of
        // magnitude off, INVALID_SIGNER. Now both sides agree on the
        // arborter's scale (token_decimals).
        let (config, market) = config_with_market(18, 6, 18);
        // qty 0.1 WFLR → quantity_pair = 0.1 * 10^18 = 10^17.
        // price 1.0 USDC/WFLR → price_pair = 10^18.
        let q = "100000000000000000"; // 10^17
        let p = "1000000000000000000"; // 10^18
        let r = resolve_order(&config, &market, 1, q, Some(p), None).unwrap();
        // amount_in (quote, USDC=6 dp): 10^17 * 10^18 = 10^35, ÷ 10^(36-6) = 10^5.
        assert_eq!(r.amount_in, 100_000);
        // amount_out (base, WFLR=18 dp): 10^17 (same scale already).
        assert_eq!(r.amount_out, 100_000_000_000_000_000);
    }

    #[test]
    fn resolve_sell_limit_mirrors_buy() {
        // pair=base=6, quote=6. Ask side flips which leg is qty vs qty*price.
        let (config, market) = config_with_market(6, 6, 6);
        let r = resolve_order(&config, &market, 2, "100000", Some("1000000"), None).unwrap();
        // amount_in (base) = qty in base_decimals = 100_000.
        // amount_out (quote) = qty*price in quote_decimals = 100_000.
        assert_eq!(r.amount_in, 100_000);
        assert_eq!(r.amount_out, 100_000);
    }

    #[test]
    fn resolve_rejects_unknown_side() {
        let (config, market) = config_with_market(6, 6, 6);
        assert!(resolve_order(&config, &market, 7, "100000", Some("1000000"), None).is_err());
    }

    // ----- the budget rule, cell by cell -------------------------------
    //
    // One rule: an order commits a budget, denominated in the asset it
    // gives. `amount_in` IS that budget, and it is what the order id is
    // hashed over. Three cells derive it from (quantity, price); the market
    // bid states it as `quote_budget`.

    /// A market bid's budget is the number the caller stated, taken
    /// VERBATIM: `quote_budget` already arrives in the quote token's own
    /// base units, the denomination the arborter's ledger reserves in.
    /// Re-scaling it by pair decimals here would be the WFLR/USDC bug in
    /// reverse — and on this fixture every wrong scaling lands on a
    /// different integer (7_500_000 vs 75_000 vs 750_000_000).
    #[test]
    fn resolve_market_bid_takes_the_stated_budget_verbatim() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        // 7.5 quote at 6 decimals. Quantity is a deliberately different
        // number (1.0 base at pair 8) so a mutant that budgets the quantity
        // instead cannot coincide with the right answer.
        let r = resolve_order(&config, &market, 1, "100000000", None, Some("7500000")).unwrap();
        assert_eq!(
            r.amount_in, 7_500_000,
            "the stated quote budget must reach the order id unscaled"
        );
        // No price, so no honest expected output — zero, not a guess.
        assert_eq!(r.amount_out, 0);
    }

    /// A market bid with no budget is unbounded: it can be filled at any ask
    /// in the book, so nothing collateralises it. The arborter refuses it
    /// (`quote_budget_for_cell`); refusing here saves the round-trip.
    #[test]
    fn resolve_market_bid_without_a_budget_is_refused() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let err = resolve_order(&config, &market, 1, "100000000", None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must set Order.quote_budget"),
            "expected the market-bid budget requirement; got: {msg}"
        );
    }

    /// Zero buys nothing and reserves nothing — same refusal the arborter
    /// makes, rather than an order that rests unfillable.
    #[test]
    fn resolve_market_bid_rejects_a_zero_or_unparseable_budget() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let err = resolve_order(&config, &market, 1, "100000000", None, Some("0")).unwrap_err();
        assert!(err.to_string().contains("greater than zero"), "got: {err}");
        let err = resolve_order(&config, &market, 1, "100000000", None, Some("7.5")).unwrap_err();
        assert!(err.to_string().contains("not a u128 decimal"), "got: {err}");
    }

    /// The other three cells derive their budget, so a stated one could only
    /// disagree with the derivation. Refused on all three — silently ignoring
    /// it would let a caller believe a cap applies that nothing enforces.
    #[test]
    fn resolve_rejects_a_budget_on_every_deriving_cell() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let budget = Some("7500000");
        for (side, quantity, price) in [
            (1, "100000000", Some("200000000")), // limit bid
            (2, "100000000", Some("200000000")), // limit ask
            (2, "100000000", None),              // market ask
        ] {
            let err = resolve_order(&config, &market, side, quantity, price, budget).unwrap_err();
            assert!(
                err.to_string().contains("only for a MARKET bid"),
                "side {side} price {price:?} should refuse a stated budget; got: {err}"
            );
        }
    }

    /// A market ask needs no price: it gives base, and its budget IS its
    /// quantity, converted to the BASE token's own units. Used to be refused
    /// outright ("requires a limit price"), which was never true of the ask
    /// side — only the bid lacked a derivable number.
    #[test]
    fn resolve_market_ask_budgets_its_quantity_in_base_units() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        // 1.0 base at pair decimals 8 → 10^18 at base decimals 18.
        let r = resolve_order(&config, &market, 2, "100000000", None, None).unwrap();
        assert_eq!(r.amount_in, 1_000_000_000_000_000_000);
        assert_eq!(r.amount_out, 0);
    }

    /// The budget is what the order id is hashed over, so two market bids
    /// that differ ONLY in budget must derive different ids. This is the
    /// property the arborter will rely on when it starts deriving the id
    /// server-side: same signed order in, same id out.
    #[test]
    fn market_bid_budget_reaches_the_order_id() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let id_for = |budget: &str| {
            let r = resolve_order(&config, &market, 1, "100000000", None, Some(budget)).unwrap();
            derive_order_id(
                &[7u8; 20],
                42, // fixed nonce: the budget is the only difference
                r.origin_chain.chain_id as u64,
                r.destination_chain.chain_id as u64,
                r.input_token_address.as_bytes(),
                r.output_token_address.as_bytes(),
                r.amount_in,
                r.amount_out,
            )
        };
        assert_ne!(id_for("7500000"), id_for("7500001"));
    }
}
