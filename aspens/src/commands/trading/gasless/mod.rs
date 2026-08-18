//! Resolve an order's budget and derive its canonical id.
//!
//! Stateless — no gRPC, no arborter round-trip, no chain RPC. Pure data.
//!
//! Under the optimistic shadow ledger, order entry never touches the chain:
//! the arborter authenticates the order via the **outer envelope** signature
//! (`aspens::evm::sign_send_order_envelope`) alone. The legacy gasless
//! on-chain-lock signing (EVM EIP-712 `GaslessCrossChainOrder` / Solana
//! `OpenForSignedPayload`) is gone with the on-chain order machinery, so this
//! helper no longer signs a lock or dispatches per chain architecture.
//!
//! # Nothing here goes on the wire any more
//!
//! The id this module derives is **not sent**. The `OrderAuthorization`
//! message that used to carry it — and `SendOrderRequest.authorization` with
//! it — was deleted: the arborter now derives the id itself from the signed
//! `Order`, so a caller cannot choose one. What is derived here is the
//! caller's own copy, for tracking a fill it has not yet seen an id for, and
//! it matches the server's only while every input to the recipe is one the
//! server can read off the message it verified. `nonce` is the input that had
//! to move: it is now `Order.nonce`, signed, and it is passed in here rather
//! than minted here so that one value reaches both places. See
//! [`crate::commands::trading::send_order`]'s `prepare_order`, which binds it
//! once.
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
//! # Where the id's token strings come from
//!
//! **`Order.market_id`, cut into its four `::`-separated segments — never the
//! `tokens` table.** The id hashes each token as its address STRING, so the
//! spelling is the value, and `market_id` is the one spelling that is inside
//! the signed message and is therefore what the server hashes. The private
//! `parse_market_id` below carries the full argument, including which of the
//! id's other inputs are NOT a second source and why.
//!
//! # Usage sketch
//!
//! ```ignore
//! use aspens::commands::trading::gasless::{build_order_commitment, client_nonce};
//!
//! let nonce = client_nonce()?;
//! let commitment = build_order_commitment(
//!     &config, market, side, &wallet, &quantity_raw, price_raw.as_deref(),
//!     quote_budget_raw.as_deref(), nonce,
//! )?;
//! let order = Order { /* ... */ nonce, ..Default::default() };
//! ```
//!
//! See also `aspens::orders::derive_order_id`.

#![cfg(feature = "client")]

use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{Result, eyre};

use crate::commands::config::config_pb::{Chain, GetConfigResponse, Market};
use crate::orders::derive_order_id;
use crate::wallet::{CurveType, Wallet};

/// What [`build_order_commitment`] produces: the caller's own copy of the
/// canonical order id, the budget it was derived over, and the nonce that went
/// into it.
///
/// None of it is sent to the arborter — `OrderAuthorization` is gone, field
/// numbers and all, and the arborter derives the id and the budget from the
/// signed `Order`. These are the caller's numbers, kept so it can recognise
/// its own order and so the FCE direct-action JSON (which still declares
/// `orderId` / `amountIn` for a not-yet-resynced adapter) has something to
/// fill in. See [`crate::commands::trading::fce_actions`].
#[derive(Debug, Clone)]
pub struct OrderCommitment {
    /// The canonical 32-byte order id, `0x`-prefixed hex — the same value the
    /// arborter derives from the signed `Order`, provided `nonce` below is the
    /// one that order carries.
    pub order_id: String,
    /// The order's budget — how much of the asset it GIVES it commits — in
    /// that token's native base units (NOT pair decimals). Quote for a bid,
    /// base for an ask; for a market bid it is the stated `quote_budget`
    /// verbatim.
    pub input_amount: u128,
    /// The nonce hashed into `order_id`. **Must equal `Order.nonce` on the
    /// order that gets signed** — it is echoed back here so a caller can bind
    /// the two rather than mint the value twice.
    pub nonce: u64,
}

/// Derive the canonical order id for the given order, and the budget it
/// commits.
///
/// Resolves the order's chains/tokens/budget and hashes them, with `nonce`,
/// into the id. Nothing here is transmitted: the arborter runs the same recipe
/// over the `Order` it verified. That is exactly why `nonce` is a parameter —
/// the value must also be set as `Order.nonce`, and minting it inside would
/// give the caller no way to sign the same one.
///
/// `quote_budget_raw` is the market-BID budget in the QUOTE token's native
/// base units (decimal `u128`, the same string that goes into
/// `Order.quote_budget`). It is required for a market BID and rejected on
/// every other cell — see the module header.
// The argument list mirrors what the id recipe consumes; see the same note on
// `crate::orders::derive_order_id`.
#[allow(clippy::too_many_arguments)]
pub fn build_order_commitment(
    config: &GetConfigResponse,
    market: &Market,
    side: i32,
    wallet: &Wallet,
    quantity_raw: &str,
    price_raw: Option<&str>,
    quote_budget_raw: Option<&str>,
    nonce: u64,
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
        order_id: order_id_hex,
        input_amount: amount_in,
        nonce,
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
pub(crate) fn normalize(amount: u128, from_decimals: u32, to_decimals: u32) -> Result<u128> {
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
    // The two token strings the id is hashed over come out of `market_id`,
    // and out of nothing else. See `parse_market_id` for why.
    let [_, base_token_address, _, quote_token_address] = parse_market_id(&market.market_id)?;

    // Arborter handler convention: Bid = buying base, locks on quote
    // chain. Ask = selling base, locks on base chain.
    let (origin_net, origin_sym, dest_net, dest_sym, input_token_address, output_token_address) =
        match side {
            1 => (
                &market.quote_chain_network,
                &market.quote_chain_token_symbol,
                &market.base_chain_network,
                &market.base_chain_token_symbol,
                quote_token_address,
                base_token_address,
            ),
            2 => (
                &market.base_chain_network,
                &market.base_chain_token_symbol,
                &market.quote_chain_network,
                &market.quote_chain_token_symbol,
                base_token_address,
                quote_token_address,
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
    // The `tokens` rows are consulted for DECIMALS only — never for the
    // address. Decimals are not a second source: the arborter reads a market's
    // token decimals through the very same `(network, symbol)` key
    // (`get_market_by_id` LEFT JOINs `tokens` on it), so both sides read one
    // column. The address is the field that has two homes, and only one of
    // them is signed.
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
        input_token_address: input_token_address.to_string(),
        output_token_address: output_token_address.to_string(),
        amount_in,
        amount_out,
    })
}

/// Split a `market_id` into `[base_network, base_token_address,
/// quote_network, quote_token_address]`.
///
/// # Why the id's token strings must come from here
///
/// The order id hashes each token as its **address STRING** — the UTF-8 bytes
/// of the text, not the decoded address — so `0xAbC…` and `0xabc…` are two
/// different preimages and therefore two different ids for one order. Which
/// spelling the client hashes is thus load-bearing, and there are two places a
/// spelling can be read from:
///
/// * `market_id`, which is a field of the signed `Order`. The arborter cuts
///   its preimage out of exactly this substring, and the browser SDK's
///   `buildOrderCommitment` does the same.
/// * the `tokens` table, served by `GetConfig` as `Token.address`. A
///   *different* row, with its own lifetime.
///
/// Those two agree only while byte-identical. Registering a market now asserts
/// they are (the arborter's `assert_token_address` refuses a `SetMarket` whose
/// stated address differs from the `tokens` row, case included), but nothing
/// re-checks a market afterwards: re-register the token under another spelling
/// and the market's id keeps the old one. Reading `Token.address` would then
/// derive an id no venue ever issues, and the failure is silent — the order is
/// accepted, the two sides track different ids, and nothing on the wire says
/// so. Reading `market_id` cannot drift, because it *is* what the server
/// hashes.
///
/// Only the addresses are contested. The two network segments are the same
/// strings as `Market.base_chain_network` / `.quote_chain_network` by
/// construction — `insert_new_market` builds the id out of those very columns
/// — so they are left where the rest of this module already reads them.
///
/// Mirrors the arborter's `trading::parse_market_id`, including that segments
/// past the fourth are ignored.
fn parse_market_id(market_id: &str) -> Result<[&str; 4]> {
    let mut segments = market_id.split("::");
    let malformed = || {
        eyre!(
            "market_id {market_id:?} is not \
             base_network::base_token_address::quote_network::quote_token_address — \
             the order id is hashed over the two token addresses in it, so there is \
             nothing to fall back to"
        )
    };
    let base_network = segments.next().ok_or_else(malformed)?;
    let base_token_address = segments.next().ok_or_else(malformed)?;
    let quote_network = segments.next().ok_or_else(malformed)?;
    let quote_token_address = segments.next().ok_or_else(malformed)?;
    Ok([
        base_network,
        base_token_address,
        quote_network,
        quote_token_address,
    ])
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

/// The SDK's choice of `Order.nonce`: millis since the Unix epoch.
///
/// Any `u64` is a legal nonce — it exists only to separate a wallet's
/// otherwise identical orders, since the id is derived from the order's
/// content plus the signer's address. Millis gives 1000x the collision
/// headroom of a unix-seconds scheme; two orders that do land on the same
/// millisecond with every other field equal derive the same id and the second
/// is refused as a replay, which is the intended behaviour of a reused nonce,
/// not a corruption.
///
/// Call it once per order and put the value in **both** `Order.nonce` and
/// [`build_order_commitment`] — see `send_order::prepare_order`.
pub fn client_nonce() -> Result<u64> {
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

    /// The base token's address as `market_id` spells it — the spelling the
    /// arborter hashes, and so the one the id must be derived over.
    pub(crate) const BASE_ADDR_IN_MARKET_ID: &str = "0xbase";
    /// The same address as the `tokens` table spells it. Deliberately a
    /// DIFFERENT string (case only, which is the divergence that actually
    /// occurs): a fixture where both sources hold one string cannot tell a
    /// correct source from a wrong one, and every token-address test in this
    /// module would pass while reading the wrong row.
    pub(crate) const BASE_ADDR_IN_TOKENS_TABLE: &str = "0xBASE";
    /// Quote side of the same split. See [`BASE_ADDR_IN_MARKET_ID`].
    pub(crate) const QUOTE_ADDR_IN_MARKET_ID: &str = "0xquote";
    /// Quote side of the same split. See [`BASE_ADDR_IN_TOKENS_TABLE`].
    pub(crate) const QUOTE_ADDR_IN_TOKENS_TABLE: &str = "0xQUOTE";

    /// Guard the guard: the two spellings must be the same address written
    /// two ways. Equal strings would make every assertion below vacuous;
    /// different addresses would make them test the wrong thing.
    #[test]
    fn the_fixture_spells_each_address_two_different_ways() {
        for (in_id, in_tokens) in [
            (BASE_ADDR_IN_MARKET_ID, BASE_ADDR_IN_TOKENS_TABLE),
            (QUOTE_ADDR_IN_MARKET_ID, QUOTE_ADDR_IN_TOKENS_TABLE),
        ] {
            assert_ne!(
                in_id, in_tokens,
                "the two sources must hold different strings or no test can \
                 distinguish them"
            );
            assert!(
                in_id.eq_ignore_ascii_case(in_tokens),
                "they must still be the same address, differing only in case"
            );
        }
        let (config, market) = config_with_market(6, 6, 6);
        assert_eq!(
            market.market_id,
            format!("base-net::{BASE_ADDR_IN_MARKET_ID}::quote-net::{QUOTE_ADDR_IN_MARKET_ID}")
        );
        assert_eq!(
            config.get_token("base-net", "BASE").unwrap().address,
            BASE_ADDR_IN_TOKENS_TABLE
        );
        assert_eq!(
            config.get_token("quote-net", "QUOTE").unwrap().address,
            QUOTE_ADDR_IN_TOKENS_TABLE
        );
    }

    /// The id hashes each token's address as a STRING, so the spelling is the
    /// value. It must come from `market_id` — part of the signed `Order`, and
    /// the exact substring the arborter cuts its own preimage from — not from
    /// the `tokens` row, which is a separate record that can be re-registered
    /// under another spelling after the market exists.
    ///
    /// Both sides, because the orientation is what decides WHICH address is
    /// the input: a bid gives quote, an ask gives base.
    #[test]
    fn the_token_strings_come_from_market_id_not_the_tokens_table() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);

        let bid = resolve_order(&config, &market, 1, "100000000", Some("200000000"), None).unwrap();
        assert_eq!(bid.input_token_address, QUOTE_ADDR_IN_MARKET_ID);
        assert_eq!(bid.output_token_address, BASE_ADDR_IN_MARKET_ID);

        let ask = resolve_order(&config, &market, 2, "100000000", Some("200000000"), None).unwrap();
        assert_eq!(ask.input_token_address, BASE_ADDR_IN_MARKET_ID);
        assert_eq!(ask.output_token_address, QUOTE_ADDR_IN_MARKET_ID);

        // Stated as a NEGATIVE too: reading the `tokens` row would produce
        // these strings instead, and hashing them yields an id the arborter
        // never issues for an order it accepts without complaint.
        assert_ne!(bid.input_token_address, QUOTE_ADDR_IN_TOKENS_TABLE);
        assert_ne!(ask.input_token_address, BASE_ADDR_IN_TOKENS_TABLE);
    }

    /// And it reaches the digest, not just the resolution: two markets that
    /// differ ONLY in how `market_id` spells a token derive different ids.
    #[test]
    fn the_market_id_spelling_reaches_the_derived_order_id() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let mut recased = market.clone();
        recased.market_id = format!(
            "base-net::{BASE_ADDR_IN_TOKENS_TABLE}::quote-net::{QUOTE_ADDR_IN_TOKENS_TABLE}"
        );

        let wallet = Wallet::from_evm_hex(TEST_EVM_KEY).unwrap();
        let id_for = |m: &Market| {
            build_order_commitment(
                &config,
                m,
                1,
                &wallet,
                "100000000",
                Some("200000000"),
                None,
                1_700_000_000_042,
            )
            .unwrap()
            .order_id
        };
        assert_ne!(
            id_for(&market),
            id_for(&recased),
            "the address STRING is the preimage, so a re-cased market_id is a \
             different id — and a client that hashed the `tokens` row instead \
             would return the same id for both"
        );
    }

    /// A `market_id` that cannot be cut into four is refused here rather than
    /// hashed into an id over an empty or partial address.
    #[test]
    fn a_short_market_id_is_refused() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        for malformed in [
            "",
            "base-net",
            "base-net::0xbase",
            "base-net::0xbase::quote-net",
        ] {
            let mut broken = market.clone();
            broken.market_id = malformed.to_string();
            let err = resolve_order(&config, &broken, 1, "100000000", Some("200000000"), None)
                .unwrap_err();
            assert!(
                err.to_string().contains("is not"),
                "market_id {malformed:?} should be refused as malformed; got: {err}"
            );
        }
    }

    /// Shared with `send_order`'s tests: one fixture, so a market that
    /// exercises the budget rule is described once.
    ///
    /// The `tokens` rows and the `market_id` spell each address DIFFERENTLY
    /// on purpose — see [`BASE_ADDR_IN_TOKENS_TABLE`].
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
                address: BASE_ADDR_IN_TOKENS_TABLE.into(),
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
                address: QUOTE_ADDR_IN_TOKENS_TABLE.into(),
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
            market_id: format!(
                "base-net::{BASE_ADDR_IN_MARKET_ID}::quote-net::{QUOTE_ADDR_IN_MARKET_ID}"
            ),
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
    /// property the arborter relies on now that it derives the id itself:
    /// same signed order in, same id out.
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

    // ----- the nonce reaches the id ------------------------------------

    /// Anvil key #0. Any fixed key does; the id just has to be stable across
    /// the two calls being compared.
    const TEST_EVM_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn commit_with_nonce(nonce: u64) -> OrderCommitment {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        let wallet = Wallet::from_evm_hex(TEST_EVM_KEY).unwrap();
        build_order_commitment(
            &config,
            &market,
            1, // limit bid
            &wallet,
            "100000000",       // 1.0 base at pair 8
            Some("200000000"), // price 2.0 at pair 8
            None,
            nonce,
        )
        .unwrap()
    }

    /// The nonce is an input to the id, so two orders identical in every
    /// other respect must derive different ids. This is what makes
    /// `Order.nonce` worth signing: the arborter reproduces the id from the
    /// message, and a caller that changed nothing but the nonce still gets a
    /// distinct order rather than a replay.
    ///
    /// Both nonces are non-zero and neither is the other's byte-reversal, so
    /// a mutant that ignores the argument, or one that hashes a constant,
    /// fails here instead of coinciding.
    #[test]
    fn the_nonce_changes_the_derived_order_id() {
        let a = commit_with_nonce(1_700_000_000_001);
        let b = commit_with_nonce(1_700_000_000_002);
        assert_ne!(
            a.order_id, b.order_id,
            "the nonce must reach derive_order_id, or a wallet's repeat orders \
             collide on one id"
        );
        // Everything else about the two orders is equal — so the id is the
        // only thing the nonce moved.
        assert_eq!(a.input_amount, b.input_amount);
    }

    /// The nonce comes back on the commitment so the caller can put the SAME
    /// value in `Order.nonce`. If it were re-minted, or echoed as a default,
    /// the client's id and the arborter's would differ with nothing on the
    /// wire to show it.
    #[test]
    fn the_commitment_echoes_the_nonce_it_hashed() {
        // Not 0 and not 1: a mutant returning either a default or a counter
        // is visible.
        let nonce = 1_700_000_000_042;
        assert_eq!(commit_with_nonce(nonce).nonce, nonce);
    }

    // ----- the id's arithmetic is a cross-repo contract ----------------

    /// A limit bid's budget is `quantity x price` normalised in ONE step,
    /// from `2 x pair_decimals` straight to the quote token's decimals — and
    /// the arborter deliberately implements that same single-floor
    /// arithmetic. The value below is therefore a cross-repo pin, not an
    /// internal detail: the id is hashed over it, and a client that computes a
    /// different integer derives an id the venue never issues, which is
    /// refused with nothing to diagnose it by.
    ///
    /// base 18 / quote 6 / pair 8 — three different scales, so a mutant that
    /// normalises to the wrong one lands on a different integer.
    #[test]
    fn limit_bid_budget_is_quantity_times_price_in_quote_units() {
        let (config, market) = config_with_market(BASE_DEC, QUOTE_DEC, PAIR_DEC);
        // qty 1.0 (10^8 at pair 8) x price 2.0 (2*10^8 at pair 8) = 2*10^16
        // at pair*2 = 16 decimals. One step to quote's 6: / 10^10 = 2_000_000.
        let r = resolve_order(&config, &market, 1, "100000000", Some("200000000"), None).unwrap();
        assert_eq!(
            r.amount_in, 2_000_000,
            "2.0 quote at 6 decimals, not 2.0 at pair or base decimals"
        );
        // Output leg: 1.0 base, pair 8 -> base 18.
        assert_eq!(r.amount_out, 1_000_000_000_000_000_000);
    }

    /// **Do not make this floor twice.** Chained division alone cannot tell
    /// the two apart — `floor(floor(x/a)/b) == floor(x/(a*b))` for positive
    /// integers — which is why every market this venue runs (quote decimals
    /// <= pair decimals) sees one answer and the question looks academic. It
    /// stops being academic the moment the second step is a MULTIPLY, i.e.
    /// when quote decimals EXCEED pair decimals: flooring first throws away
    /// digits the multiply then re-pads with zeros.
    ///
    /// The fixture below is that market (quote 8, pair 6), and the number it
    /// pins is what the SDK and the arborter both produce today. A two-floor
    /// "fix" — defensible in isolation, since the reservation sizing does
    /// floor twice — would return 123_456_800 here and break every order on
    /// such a market silently: same signed bytes, two different ids. Changing
    /// the recipe requires both repos to move together.
    #[test]
    fn limit_bid_budget_floors_once_where_that_is_observable() {
        // base 18 / quote 8 / pair 6 — quote > pair, the divergent shape.
        let (config, market) = config_with_market(18, 8, 6);
        // qty 1.234567, price 1.000001 (both at pair 6).
        // product = 1_234_568_234_567 at pair*2 = 12 decimals.
        let r = resolve_order(&config, &market, 1, "1234567", Some("1000001"), None).unwrap();
        assert_eq!(
            r.amount_in, 123_456_823,
            "one floor, 12 -> 8 decimals. Two floors (12 -> 6 -> 8) would give \
             123_456_800 — a different order id for the same signed order"
        );
    }
}
