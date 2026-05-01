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
use crate::orders::{derive_order_id, GaslessLockParams};
use crate::wallet::{CurveType, Wallet};

use super::send_order::arborter_pb::GaslessAuthorization;

/// EVM architecture tag stored on `Chain.architecture` by the arborter.
/// Kept local — only consumed here.
const ARCH_EVM: &str = "EVM";
const ARCH_SOLANA: &str = "Solana";

/// Hard deadlines for the EVM gasless order:
/// - `OPEN_DEADLINE` = now + 1h — matches arborter's legacy recipe in
///   `chain-evm::lock_for_order` so external tooling expecting the same
///   horizons keeps working.
/// - `FILL_DEADLINE` = now + 24h.
const EVM_OPEN_DEADLINE_SECS: u64 = 3_600;
const EVM_FILL_DEADLINE_SECS: u64 = 86_400;

/// Solana slot buffer — how far ahead the arborter must land the tx.
/// 100 slots ≈ 40s which is comfortably larger than the bursty RPC
/// + confirmation round-trip.
const SOLANA_DEADLINE_SLOT_BUFFER: u64 = 100;

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
    if arch.eq_ignore_ascii_case(ARCH_EVM) {
        build_evm(
            origin_chain,
            destination_chain,
            wallet,
            &input_token_address,
            &output_token_address,
            amount_in,
            amount_out,
            nonce,
            order_id_hex,
        )
        .await
    } else if arch.eq_ignore_ascii_case(ARCH_SOLANA) {
        build_solana(
            origin_chain,
            destination_chain,
            wallet,
            &input_token_address,
            &output_token_address,
            amount_in,
            amount_out,
            nonce,
            order_id_bytes,
            order_id_hex,
        )
        .await
    } else {
        Err(eyre!(
            "gasless auth not implemented for chain architecture {arch:?}"
        ))
    }
}

struct OrderResolution<'a> {
    origin_chain: &'a Chain,
    destination_chain: &'a Chain,
    input_token_address: String,
    output_token_address: String,
    amount_in: u128,
    amount_out: u128,
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

    // amount_in: what the user locks (quantity * price for Bid; raw quantity for Ask).
    // amount_out: what the user expects on the destination side.
    //
    // For Bid: user pays quote = quantity * price. Receives quantity base.
    // For Ask: user pays base = quantity. Receives quantity * price quote.
    let quantity: u128 = quantity_raw
        .parse()
        .map_err(|e| eyre!("quantity_raw {quantity_raw:?} is not a u128: {e}"))?;
    let price: Option<u128> = price_raw
        .map(|s| {
            s.parse::<u128>()
                .map_err(|e| eyre!("price_raw {s:?} is not a u128: {e}"))
        })
        .transpose()?;
    let pair_decimals = market.pair_decimals as u32;
    let price_scale = 10u128
        .checked_pow(pair_decimals)
        .ok_or_else(|| eyre!("pair_decimals {pair_decimals} overflows u128 scale"))?;

    let (amount_in, amount_out) = match (side, price) {
        (1, Some(p)) => (
            // Bid limit: pay quantity*price quote, receive quantity base
            quantity
                .checked_mul(p)
                .ok_or_else(|| eyre!("amount_in overflow"))?
                / price_scale,
            quantity,
        ),
        (2, Some(p)) => (
            quantity,
            // Ask limit: pay quantity base, receive quantity*price quote
            quantity
                .checked_mul(p)
                .ok_or_else(|| eyre!("amount_out overflow"))?
                / price_scale,
        ),
        (_, None) => {
            // Market order — no price at sign time. Use quantity as both
            // amount_in and amount_out as the user's sticker-price
            // representation; the real fill may differ but that's the
            // arborter's concern, not the gasless signature's.
            (quantity, quantity)
        }
        _ => unreachable!(),
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

#[cfg(feature = "evm")]
#[allow(clippy::too_many_arguments)]
async fn build_evm(
    origin_chain: &Chain,
    destination_chain: &Chain,
    wallet: &Wallet,
    input_token_address: &str,
    output_token_address: &str,
    amount_in: u128,
    amount_out: u128,
    nonce: u64,
    order_id_hex: String,
) -> Result<GaslessAuthorization> {
    use alloy_primitives::Address;

    let now = unix_secs()?;
    let open_deadline = now + EVM_OPEN_DEADLINE_SECS;
    let fill_deadline = now + EVM_FILL_DEADLINE_SECS;

    let depositor = wallet.address();
    let dest_chain_id = destination_chain.chain_id.to_string();
    let params = GaslessLockParams {
        depositor_address: &depositor,
        token_contract: input_token_address,
        token_contract_destination_chain: output_token_address,
        destination_chain_id: &dest_chain_id,
        amount_in,
        amount_out,
        order_id: &order_id_hex,
        deadline: fill_deadline,
        nonce,
        open_deadline,
        user_signature: &[],
    };

    let arborter: Address = origin_chain
        .instance_signer_address
        .parse()
        .map_err(|e| eyre!("invalid instance_signer_address on origin chain: {e}"))?;
    let origin_settler: Address = origin_chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre!("origin chain has no trade_contract configured"))?
        .address
        .parse()
        .map_err(|e| eyre!("invalid trade_contract.address on origin chain: {e}"))?;
    let digest = crate::evm::gasless_lock_signing_hash(
        &params,
        arborter,
        origin_settler,
        origin_chain.chain_id as u64,
    )?;

    // `Wallet::sign_message` on EVM applies EIP-191, which is what
    // MidribV2._verifyOrder wraps the digest with before ecrecover.
    // `sign_hash` / `sign_eip712_digest` would NOT wrap and would be
    // rejected as INVALID_SIGNER on-chain.
    let sig = wallet.sign_message(digest.as_slice()).await?;

    if sig.len() != 65 {
        return Err(eyre!(
            "EVM gasless signature must be 65 bytes (r||s||v); got {}",
            sig.len()
        ));
    }

    Ok(GaslessAuthorization {
        user_signature: sig,
        deadline: fill_deadline,
        order_id: order_id_hex,
        nonce,
        open_deadline,
    })
}

#[cfg(not(feature = "evm"))]
#[allow(clippy::too_many_arguments)]
async fn build_evm(
    _: &Chain,
    _: &Chain,
    _: &Wallet,
    _: &str,
    _: &str,
    _: u128,
    _: u128,
    _: u64,
    _: String,
) -> Result<GaslessAuthorization> {
    Err(eyre!(
        "EVM gasless authorization requires the `evm` feature of the aspens crate"
    ))
}

#[cfg(feature = "solana")]
#[allow(clippy::too_many_arguments)]
async fn build_solana(
    origin_chain: &Chain,
    destination_chain: &Chain,
    wallet: &Wallet,
    input_token_address: &str,
    output_token_address: &str,
    amount_in: u128,
    amount_out: u128,
    nonce: u64,
    order_id_bytes: [u8; 32],
    order_id_hex: String,
) -> Result<GaslessAuthorization> {
    use crate::solana::{gasless_lock_signing_message, OpenOrderArgs};
    use solana_sdk::pubkey::Pubkey;

    // Deadline = current_slot + buffer. Fetches once from origin chain's RPC.
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(origin_chain.rpc_url.clone());
    let current_slot = rpc
        .get_slot()
        .await
        .map_err(|e| eyre!("solana get_slot: {e}"))?;
    let deadline = current_slot + SOLANA_DEADLINE_SLOT_BUFFER;

    let instance_pda: Pubkey = origin_chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre!("origin chain has no trade_contract configured"))?
        .address
        .parse()
        .map_err(|e| eyre!("invalid trade_contract.address on origin chain: {e}"))?;
    let user_pubkey: Pubkey = wallet.address().parse().map_err(|e| {
        eyre!(
            "wallet address {:?} not a valid Solana pubkey: {e}",
            wallet.address()
        )
    })?;
    let input_token: Pubkey = input_token_address
        .parse()
        .map_err(|e| eyre!("input token {input_token_address:?} not a Solana pubkey: {e}"))?;

    // For EVM destination tokens (0x-prefixed 20-byte hex), the address
    // won't be a 32-byte Solana pubkey. Left-pad into a 32-byte slot so
    // it fits OpenOrderArgs::output_token. Arborter-side unpacks by
    // convention (low-order 20 bytes = EVM addr).
    let output_token_bytes = parse_cross_chain_token_into_32(output_token_address)?;

    let amount_in_u64 = u64::try_from(amount_in)
        .map_err(|_| eyre!("Solana amount_in {amount_in} exceeds u64::MAX"))?;
    let amount_out_u64 = u64::try_from(amount_out)
        .map_err(|_| eyre!("Solana amount_out {amount_out} exceeds u64::MAX"))?;

    let order = OpenOrderArgs {
        order_id: order_id_bytes,
        origin_chain_id: origin_chain.chain_id as u64,
        destination_chain_id: destination_chain.chain_id as u64,
        input_token,
        input_amount: amount_in_u64,
        output_token: output_token_bytes,
        output_amount: amount_out_u64,
    };
    let message = gasless_lock_signing_message(&instance_pda, &user_pubkey, deadline, &order)?;

    // Wallet::sign_message on Solana → raw Ed25519 sign, 64 bytes.
    let sig = wallet.sign_message(&message).await?;
    if sig.len() != 64 {
        return Err(eyre!(
            "Solana gasless signature must be 64 bytes (Ed25519); got {}",
            sig.len()
        ));
    }

    // Placate the unused-var lints on both paths.
    let _ = nonce;

    Ok(GaslessAuthorization {
        user_signature: sig,
        deadline,
        order_id: order_id_hex,
        nonce: 0,
        open_deadline: 0,
    })
}

#[cfg(not(feature = "solana"))]
#[allow(clippy::too_many_arguments)]
async fn build_solana(
    _: &Chain,
    _: &Chain,
    _: &Wallet,
    _: &str,
    _: &str,
    _: u128,
    _: u128,
    _: u64,
    _: [u8; 32],
    _: String,
) -> Result<GaslessAuthorization> {
    Err(eyre!(
        "Solana gasless authorization requires the `solana` feature of the aspens crate"
    ))
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

fn parse_cross_chain_token_into_32(addr: &str) -> Result<[u8; 32]> {
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

fn unix_secs() -> Result<u64> {
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
}
