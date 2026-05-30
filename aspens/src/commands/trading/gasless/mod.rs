//! Build the `OrderAuthorization` proto payload for a `SendOrderRequest`.
//!
//! Stateless — no gRPC, no arborter round-trip, no chain RPC. Pure data.
//!
//! Under the optimistic shadow ledger, order entry never touches the chain:
//! the arborter authenticates the order via the **outer envelope** signature
//! (`aspens::evm::sign_send_order_envelope`) and consumes only two fields from
//! this payload — the canonical `order_id` (`aspens::orders::derive_order_id`)
//! and the committed `amount_in`. The legacy gasless on-chain-lock signing
//! (EVM EIP-712 `GaslessCrossChainOrder` / Solana `OpenForSignedPayload`) is
//! gone with the on-chain order machinery, so this helper no longer signs a
//! lock or dispatches per chain architecture.
//!
//! # Usage sketch
//!
//! ```ignore
//! use aspens::commands::trading::gasless::build_gasless_authorization;
//!
//! let gasless = build_gasless_authorization(
//!     &config, market, side, &wallet, &quantity_raw, price_raw.as_deref(),
//! )?;
//! request.authorization = Some(auth);
//! ```
//!
//! See also `aspens::orders::derive_order_id`.

#![cfg(feature = "client")]

use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{eyre, Result};

use crate::commands::config::config_pb::{Chain, GetConfigResponse, Market};
use crate::orders::derive_order_id;
use crate::wallet::{CurveType, Wallet};

use super::send_order::arborter_pb::OrderAuthorization;

/// Build an `OrderAuthorization` for the given order.
///
/// Resolves the order's chains/tokens/amounts, derives the canonical
/// `order_id`, and returns a payload carrying only the fields the arborter
/// still consumes: `order_id` and `amount_in` (the committed lock amount in
/// the input token's native base units). Order authentication is via the
/// outer envelope signature, not a per-order on-chain lock signature.
pub fn build_gasless_authorization(
    config: &GetConfigResponse,
    market: &Market,
    side: i32,
    wallet: &Wallet,
    quantity_raw: &str,
    price_raw: Option<&str>,
) -> Result<OrderAuthorization> {
    let OrderResolution {
        origin_chain,
        destination_chain,
        input_token_address,
        output_token_address,
        amount_in,
        amount_out,
    } = resolve_order(config, market, side, quantity_raw, price_raw)?;

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

    Ok(OrderAuthorization {
        order_id: order_id_hex,
        amount_in: amount_in.to_string(),
    })
}

#[cfg_attr(test, derive(Debug))]
struct OrderResolution<'a> {
    origin_chain: &'a Chain,
    destination_chain: &'a Chain,
    input_token_address: String,
    output_token_address: String,
    /// Lock amount in the origin chain's input-token native base units
    /// (e.g. 1_000_000 for 1 USDC at 6 decimals). NOT pair decimals —
    /// the arborter / on-chain contract verify the user-signed digest
    /// over this exact value, so SDK and arborter must agree on the
    /// scale. See `normalize` below for the conversion from the
    /// matching-engine's pair-decimal representation.
    amount_in: u128,
    /// Expected output amount in the destination chain's output-token
    /// native base units. Same scale convention as `amount_in`.
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

fn resolve_order<'a>(
    config: &'a GetConfigResponse,
    market: &Market,
    side: i32,
    quantity_raw: &str,
    price_raw: Option<&str>,
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
            ))
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
    let price: u128 = match price_raw {
        Some(s) => s
            .parse::<u128>()
            .map_err(|e| eyre!("price_raw {s:?} is not a u128: {e}"))?,
        None => {
            // Gasless cross-chain orders require the user to pre-commit
            // a specific `amount_in` (the lock amount the EIP-712 /
            // Ed25519 signature binds). Market orders have no price at
            // signing time, so there's no honest value to put in
            // `amount_in` — the committed input amount the arborter
            // reserves. Force the user to commit explicit slippage via a
            // buy-limit / sell-limit at a price ceiling / floor they're
            // willing to accept.
            return Err(eyre!(
                "cross-chain orders require a limit price — a market order \
                 can't pre-commit the `amount_in` the arborter reserves. Use \
                 buy-limit / sell-limit with a slippage-capped price (e.g. \
                 price ≥ best ask × (1 + slippage) for a buy)."
            ));
        }
    };

    // The matching engine works in pair-decimals throughout; quantity
    // and price arrive here as pair-decimal-scaled u128s. The on-chain
    // lock and the EIP-712 / Ed25519 digest, however, are in the
    // token's NATIVE base units. Normalise both sides of the trade so
    // the SDK signs over the same integers the arborter and contract
    // will see. For markets where pair_decimals != input_decimals or
    // != output_decimals (e.g. WFLR/USDC: pair=18, USDC=6), skipping
    // this normalisation produced digests that were N orders of
    // magnitude off and the on-chain `ecrecover` returned a nonsense
    // address.
    let (amount_in, amount_out) = match side {
        // Bid: pay quote = qty * price (in input=quote decimals),
        //      receive base = qty (in output=base decimals).
        1 => {
            let qty_quote_pair2 = quantity
                .checked_mul(price)
                .ok_or_else(|| eyre!("amount_in overflow: {quantity} * {price}"))?;
            (
                normalize(qty_quote_pair2, pair_decimals * 2, input_decimals)?,
                normalize(quantity, pair_decimals, output_decimals)?,
            )
        }
        // Ask: pay base = qty (in input=base decimals),
        //      receive quote = qty * price (in output=quote decimals).
        2 => {
            let qty_quote_pair2 = quantity
                .checked_mul(price)
                .ok_or_else(|| eyre!("amount_out overflow: {quantity} * {price}"))?;
            (
                normalize(quantity, pair_decimals, input_decimals)?,
                normalize(qty_quote_pair2, pair_decimals * 2, output_decimals)?,
            )
        }
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
mod tests {
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

    fn config_with_market(
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
            permit2_address: "0xpermit2".into(),
            trade_contract: Some(TradeContract {
                contract_id: None,
                address: "0xtradecontract".into(),
            }),
            tokens: base_tokens,
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
            permit2_address: "0xpermit2".into(),
            trade_contract: Some(TradeContract {
                contract_id: None,
                address: "0xtradecontract".into(),
            }),
            tokens: quote_tokens,
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

    #[test]
    fn resolve_buy_limit_same_decimals_market() {
        // pair=quote=base=6 (USDT0/USDC class). qty 0.1 @ price 1.0:
        // quantity_pair = 100_000, price_pair = 1_000_000.
        // Bid: amount_in (quote) = qty*price normalised pair*2(=12) → 6 = 100_000.
        //      amount_out (base) = qty normalised pair(=6) → 6 = 100_000.
        let (config, market) = config_with_market(6, 6, 6);
        let r = resolve_order(&config, &market, 1, "100000", Some("1000000")).unwrap();
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
        let r = resolve_order(&config, &market, 1, q, Some(p)).unwrap();
        // amount_in (quote, USDC=6 dp): 10^17 * 10^18 = 10^35, ÷ 10^(36-6) = 10^5.
        assert_eq!(r.amount_in, 100_000);
        // amount_out (base, WFLR=18 dp): 10^17 (same scale already).
        assert_eq!(r.amount_out, 100_000_000_000_000_000);
    }

    #[test]
    fn resolve_sell_limit_mirrors_buy() {
        // pair=base=6, quote=6. Ask side flips which leg is qty vs qty*price.
        let (config, market) = config_with_market(6, 6, 6);
        let r = resolve_order(&config, &market, 2, "100000", Some("1000000")).unwrap();
        // amount_in (base) = qty in base_decimals = 100_000.
        // amount_out (quote) = qty*price in quote_decimals = 100_000.
        assert_eq!(r.amount_in, 100_000);
        assert_eq!(r.amount_out, 100_000);
    }

    #[test]
    fn resolve_market_order_is_rejected() {
        // The historical bug: SDK signed (quantity, quantity) as a
        // placeholder while the arborter recomputed lock amounts from
        // `last_price` (whose initial value of 1 normalises to 0 for
        // 6-decimals quote tokens). Forcing limit orders eliminates the
        // amount-divergence class entirely.
        let (config, market) = config_with_market(6, 6, 6);
        let err = resolve_order(&config, &market, 1, "100000", None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("require a limit price"),
            "expected market-order rejection message; got: {msg}"
        );
        // Sell-market too.
        let err = resolve_order(&config, &market, 2, "100000", None).unwrap_err();
        assert!(err.to_string().contains("require a limit price"));
    }

    #[test]
    fn resolve_rejects_unknown_side() {
        let (config, market) = config_with_market(6, 6, 6);
        assert!(resolve_order(&config, &market, 7, "100000", Some("1000000")).is_err());
    }
}
