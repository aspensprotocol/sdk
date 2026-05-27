//! Build the `GaslessAuthorization` proto payload for a `SendOrderRequest`.
//!
//! Stateless — no gRPC, no arborter round-trip. Pure data + one optional
//! RPC (Solana: `getSlot`) to compute the chain-specific deadline.
//!
//! Cross-checked against arborter's `GaslessLockParams` /
//! `GaslessAuthorization` — every field the arborter's
//! `lock_for_order_gasless` consumes is produced here with the right
//! semantics (EVM vs Solana).
//!
//! # Usage sketch
//!
//! ```ignore
//! use aspens::commands::trading::gasless::build_gasless_authorization;
//!
//! let gasless = build_gasless_authorization(
//!     &config, market, side, &wallet, &quantity_raw, price_raw.as_deref(),
//! ).await?;
//! request.gasless = Some(gasless);
//! ```
//!
//! See also `aspens::orders::derive_order_id`,
//! `aspens::evm::gasless_lock_signing_hash`,
//! `aspens::solana::gasless_lock_signing_message`.
//!
//! NOTE: this helper is behind the default `client` feature because it
//! reaches for `solana-client` to read the current slot when the origin
//! chain is Solana. Lean-signing consumers that build their own
//! `GaslessAuthorization` can use the lower-level helpers directly.

#![cfg(feature = "client")]

use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{eyre, Result};

use crate::commands::config::config_pb::{Chain, GetConfigResponse, Market};
use crate::orders::derive_order_id;
use crate::wallet::{CurveType, Wallet};

use super::send_order::arborter_pb::GaslessAuthorization;

mod evm;
mod solana;

use evm::build_evm;
use solana::build_solana;

/// EVM architecture tag stored on `Chain.architecture` by the arborter.
/// Kept local — only consumed here.
const ARCH_EVM: &str = "EVM";
const ARCH_SOLANA: &str = "Solana";

/// Hard deadlines for the EVM gasless order:
/// - `OPEN_DEADLINE` = now + 1h — matches arborter's legacy recipe in
///   `chain-evm::lock_for_order` so external tooling expecting the same
///   horizons keeps working.
/// - `FILL_DEADLINE` = now + 24h.
pub(super) const EVM_OPEN_DEADLINE_SECS: u64 = 3_600;
pub(super) const EVM_FILL_DEADLINE_SECS: u64 = 86_400;

/// Solana slot buffer — how far ahead the arborter must land the tx.
/// 100 slots ≈ 40s which is comfortably larger than the bursty RPC
/// + confirmation round-trip.
pub(super) const SOLANA_DEADLINE_SLOT_BUFFER: u64 = 100;

/// Build a `GaslessAuthorization` for the given order.
///
/// Dispatches on the **origin** chain's architecture — Bid → quote
/// chain, Ask → base chain (matches arborter's handler convention:
/// locked funds live on the chain the user is paying FROM).
///
/// * EVM: produces an EIP-712 digest via `aspens::evm::gasless_lock_signing_hash`
///   and has the wallet sign it via `Wallet::sign_message` (which applies
///   the EIP-191 wrap MidribV2's `_verifyOrder` expects).
/// * Solana: produces the borsh-encoded `OpenForSignedPayload` via
///   `aspens::solana::gasless_lock_signing_message` and has the wallet
///   Ed25519-sign it.
pub async fn build_gasless_authorization(
    config: &GetConfigResponse,
    market: &Market,
    side: i32,
    wallet: &Wallet,
    quantity_raw: &str,
    price_raw: Option<&str>,
) -> Result<GaslessAuthorization> {
    let OrderResolution {
        origin_chain,
        destination_chain,
        input_token_address,
        output_token_address,
        amount_in,
        amount_out,
    } = resolve_order(config, market, side, quantity_raw, price_raw)?;

    // Client nonce: millis-since-epoch. Millis (not seconds) gives 1000×
    // collision headroom over the old unix-seconds scheme arborter used
    // — two clients issuing in the same second would otherwise have
    // produced the same Permit2 nonce and one would reject as replay.
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

    let arch = origin_chain.architecture.as_str();
    let args = GaslessBuildArgs {
        origin_chain,
        destination_chain,
        wallet,
        input_token_address: &input_token_address,
        output_token_address: &output_token_address,
        amount_in,
        amount_out,
        nonce,
        order_id_hex,
    };
    if arch.eq_ignore_ascii_case(ARCH_EVM) {
        build_evm(args).await
    } else if arch.eq_ignore_ascii_case(ARCH_SOLANA) {
        build_solana(args, order_id_bytes).await
    } else {
        Err(eyre!(
            "gasless auth not implemented for chain architecture {arch:?}"
        ))
    }
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
            // `amount_in` — any placeholder (e.g. `quantity` as a
            // base-amount stand-in for a quote lock) diverges from
            // whatever the arborter ends up locking and the contract
            // rejects the order with `INVALID_SIGNER`. Force the user
            // to commit explicit slippage via a buy-limit / sell-limit
            // at a price ceiling / floor they're willing to accept.
            return Err(eyre!(
                "gasless cross-chain orders require a limit price — \
                 market orders cannot pre-commit a lock amount the on-chain \
                 verifier will recompute identically. Use buy-limit / \
                 sell-limit with a slippage-capped price (e.g. price ≥ best \
                 ask × (1 + slippage) for a buy)."
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

/// Bundle of arguments shared by `build_evm` (in the `evm` submodule)
/// and `build_solana` (in the `solana` submodule). Kept as a struct so
/// the dispatcher in [`build_gasless_authorization`] doesn't have to
/// pass nine separate arguments through (and trip clippy's
/// `too_many_arguments`).
pub(super) struct GaslessBuildArgs<'a> {
    pub(super) origin_chain: &'a Chain,
    pub(super) destination_chain: &'a Chain,
    pub(super) wallet: &'a Wallet,
    pub(super) input_token_address: &'a str,
    pub(super) output_token_address: &'a str,
    pub(super) amount_in: u128,
    pub(super) amount_out: u128,
    pub(super) nonce: u64,
    pub(super) order_id_hex: String,
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

pub(super) fn parse_cross_chain_token_into_32(addr: &str) -> Result<[u8; 32]> {
    // Solana mint — 32-byte base58 pubkey.
    if let Ok(bytes) = bs58::decode(addr).into_vec() {
        if bytes.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            return Ok(out);
        }
    }
    // EVM address — 20 bytes, left-pad.
    let trimmed = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(trimmed)
        .map_err(|e| eyre!("token {addr:?} is neither base58 nor 0x-hex: {e}"))?;
    if bytes.len() > 32 {
        return Err(eyre!(
            "token {addr:?} is {} bytes — too large for 32-byte output_token slot",
            bytes.len()
        ));
    }
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}

pub(super) fn unix_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| eyre!("system clock before epoch: {e}"))?
        .as_secs())
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

    #[test]
    fn cross_chain_token_evm_pads_to_32() {
        let addr = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"; // USDC mainnet
        let bytes = parse_cross_chain_token_into_32(addr).unwrap();
        assert_eq!(&bytes[..12], &[0u8; 12]);
        assert_eq!(&bytes[12..], &hex::decode(&addr[2..]).unwrap()[..]);
    }

    #[test]
    fn cross_chain_token_solana_fills_32() {
        // 32-byte base58 pubkey
        let pk = "Ed25519SigVerify111111111111111111111111111";
        let bytes = parse_cross_chain_token_into_32(pk).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn cross_chain_token_rejects_oversize() {
        // >32 bytes of hex
        let too_big = format!("0x{}", "aa".repeat(33));
        assert!(parse_cross_chain_token_into_32(&too_big).is_err());
    }

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
