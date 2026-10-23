//! Tron primitives: the display-edge address form, the TRON message prefix,
//! and hand-encoded transactions.
//!
//! A Tron account is the same 20 bytes as an Ethereum one, so a Tron wallet is
//! an EVM [`Wallet`]. Inside the venue the account is lowercase `0x…` hex,
//! exactly as on EVM; the base58 `T…` form exists only where a person reads or
//! types it (`tron_to_hex`, `hex_to_tron`) and never goes on the wire.
//!
//! These helpers are ports of the arborter's `chain-tron` crate and share one
//! vector fixture with it (`tests/fixtures/tron_vectors.json`): one byte of
//! drift recovers a different signer or a different txID, and nothing on the
//! wire says which field moved.

// Sending native Tron transactions over the node's HTTP API (documented in
// the module itself). Needs the `client` feature.
#[cfg(feature = "client")]
pub mod client;

use alloy_primitives::{Address, B256, keccak256};
use alloy_signer::Signer;
use eyre::{Result, eyre};
use sha2::{Digest, Sha256};

use crate::wallet::Wallet;

/// The first byte of a Tron address: `0x41` followed by the 20-byte account.
pub const TRON_ADDRESS_PREFIX: u8 = 0x41;

/// How long after the reference head a transaction stays valid, in
/// milliseconds. Tron allows up to 24 hours; a short window bounds how long
/// an unseen transaction can still land.
pub const EXPIRATION_MS: i64 = 60_000;

/// `type.googleapis.com/` + the fully qualified message name, as every Tron
/// client writes it into `Any.type_url`.
const TRIGGER_SMART_CONTRACT_TYPE_URL: &str = "type.googleapis.com/protocol.TriggerSmartContract";

/// `Transaction.Contract.ContractType.TriggerSmartContract` (Tron.proto).
const CONTRACT_TYPE_TRIGGER_SMART_CONTRACT: u64 = 31;

/// The prefix Tron wallets put before a message, ahead of its decimal length.
const MESSAGE_PREFIX: &str = "\x19TRON Signed Message:\n";

// -- Address edge ----------------------------------------------------------

fn checksum(payload: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(payload);
    let second = Sha256::digest(first);
    [second[0], second[1], second[2], second[3]]
}

/// Convert a base58check `T…` address to the venue's lowercase `0x` hex form.
///
/// Errors unless the string is base58, decodes to 25 bytes, carries the `0x41`
/// Tron prefix, and has a valid double-sha256 checksum.
pub fn tron_to_hex(address: &str) -> Result<String> {
    let raw = bs58::decode(address)
        .into_vec()
        .map_err(|e| eyre!("'{address}' is not base58: {e}"))?;
    if raw.len() != 25 {
        return Err(eyre!(
            "'{address}' decodes to {} bytes, a Tron address is 25",
            raw.len()
        ));
    }
    let (payload, check) = raw.split_at(21);
    if payload[0] != TRON_ADDRESS_PREFIX {
        return Err(eyre!(
            "'{address}' has version byte 0x{:02x}, a Tron address starts with 0x41",
            payload[0]
        ));
    }
    if checksum(payload) != check {
        return Err(eyre!("'{address}' has a bad checksum"));
    }
    Ok(format!("0x{}", hex::encode(&payload[1..])))
}

/// Convert a `0x`-prefixed 20-byte hex account (any case) to its base58check
/// `T…` form, for display.
pub fn hex_to_tron(account: &str) -> Result<String> {
    let body = account
        .strip_prefix("0x")
        .ok_or_else(|| eyre!("'{account}' must carry the 0x prefix"))?;
    let bytes = hex::decode(body).map_err(|e| eyre!("'{account}' is not hex: {e}"))?;
    if bytes.len() != 20 {
        return Err(eyre!(
            "'{account}' is {} bytes, an account is 20",
            bytes.len()
        ));
    }
    let mut payload = Vec::with_capacity(25);
    payload.push(TRON_ADDRESS_PREFIX);
    payload.extend_from_slice(&bytes);
    let check = checksum(&payload);
    payload.extend_from_slice(&check);
    Ok(bs58::encode(payload).into_string())
}

// -- TRON message signing --------------------------------------------------

/// `keccak256("\x19TRON Signed Message:\n" ‖ decimal len(text) ‖ text)`, the
/// digest tronweb's `signMessageV2` signs. `len` counts the text's bytes.
pub fn tron_message_digest(text: &str) -> B256 {
    let mut preimage = Vec::with_capacity(MESSAGE_PREFIX.len() + 8 + text.len());
    preimage.extend_from_slice(MESSAGE_PREFIX.as_bytes());
    preimage.extend_from_slice(text.len().to_string().as_bytes());
    preimage.extend_from_slice(text.as_bytes());
    keccak256(&preimage)
}

/// The digest for a protobuf message (`Order`, `OrderToCancel`): the TRON
/// prefix over the lowercase hex text, no `0x`, of the encoded bytes.
///
/// Tron wallets decode a binary message as UTF-8 lossily, so distinct binary
/// messages could share one signed text; the venue signs and verifies the hex
/// text instead.
pub fn tron_proto_message_digest(encoded: &[u8]) -> B256 {
    tron_message_digest(&hex::encode(encoded))
}

async fn sign_digest(wallet: &Wallet, digest: &B256) -> Result<[u8; 65]> {
    let signer = wallet
        .as_evm()
        .ok_or_else(|| eyre!("a Tron signature needs a secp256k1 (EVM) wallet"))?;
    let sig = signer.sign_hash(digest).await?;
    Ok(sig.as_bytes())
}

/// Sign `text` as is under the TRON message prefix (the withdraw request
/// string). Returns `r ‖ s ‖ v` with `v` = 27/28. Errors for a non-EVM wallet.
pub async fn tron_sign_text(wallet: &Wallet, text: &str) -> Result<[u8; 65]> {
    sign_digest(wallet, &tron_message_digest(text)).await
}

/// Sign an encoded protobuf message (`Order`, `OrderToCancel`) the way the
/// venue verifies it on a Tron leg: the TRON prefix over the lowercase hex
/// text of `encoded`. There is deliberately no entry point that signs the
/// raw bytes. Returns `r ‖ s ‖ v` with `v` = 27/28.
pub async fn tron_sign_proto_message(wallet: &Wallet, encoded: &[u8]) -> Result<[u8; 65]> {
    sign_digest(wallet, &tron_proto_message_digest(encoded)).await
}

// -- Transactions ----------------------------------------------------------

/// The block a transaction references (TaPoS), plus the head time its
/// expiration is counted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefBlock {
    /// Bytes 6..8 of the block number's 8-byte big-endian form.
    pub bytes: [u8; 2],
    /// Bytes 8..16 of the block ID.
    pub hash: [u8; 8],
    /// The head block's timestamp, in milliseconds.
    pub head_time_ms: i64,
}

/// The 21-byte Tron form of a 20-byte account: `0x41 ‖ account`.
pub fn tron_address_bytes(account: Address) -> [u8; 21] {
    let mut out = [0u8; 21];
    out[0] = TRON_ADDRESS_PREFIX;
    out[1..].copy_from_slice(account.as_slice());
    out
}

/// Encode `Transaction.raw` for one `TriggerSmartContract` call, stamped with
/// the current time. `expiration` is `rb.head_time_ms` + [`EXPIRATION_MS`];
/// `call_value_sun` is omitted when zero.
pub fn build_trigger(
    owner: Address,
    contract: Address,
    data: &[u8],
    call_value_sun: i64,
    fee_limit_sun: i64,
    rb: &RefBlock,
) -> Vec<u8> {
    build_trigger_at(
        owner,
        contract,
        data,
        call_value_sun,
        fee_limit_sun,
        rb,
        now_ms(),
    )
}

/// [`build_trigger`] with an explicit `timestamp_ms`, the one input that is
/// not a function of the call: tests reproduce a captured transaction byte
/// for byte with it.
pub fn build_trigger_at(
    owner: Address,
    contract: Address,
    data: &[u8],
    call_value_sun: i64,
    fee_limit_sun: i64,
    rb: &RefBlock,
    timestamp_ms: i64,
) -> Vec<u8> {
    // TriggerSmartContract: bytes owner_address = 1; bytes contract_address = 2;
    // int64 call_value = 3; bytes data = 4.
    let mut trigger = Vec::new();
    put_bytes(1, &tron_address_bytes(owner), &mut trigger);
    put_bytes(2, &tron_address_bytes(contract), &mut trigger);
    put_int64(3, call_value_sun, &mut trigger);
    put_bytes(4, data, &mut trigger);

    // google.protobuf.Any: string type_url = 1; bytes value = 2.
    let mut any = Vec::new();
    put_bytes(1, TRIGGER_SMART_CONTRACT_TYPE_URL.as_bytes(), &mut any);
    put_bytes(2, &trigger, &mut any);

    // Transaction.Contract: ContractType type = 1; Any parameter = 2.
    let mut contract_msg = Vec::new();
    put_varint(1, CONTRACT_TYPE_TRIGGER_SMART_CONTRACT, &mut contract_msg);
    put_bytes(2, &any, &mut contract_msg);

    // Transaction.raw: bytes ref_block_bytes = 1; bytes ref_block_hash = 4;
    // int64 expiration = 8; repeated Contract contract = 11; int64 timestamp =
    // 14; int64 fee_limit = 18. Field-number order, as serializers write it.
    let mut raw = Vec::new();
    put_bytes(1, &rb.bytes, &mut raw);
    put_bytes(4, &rb.hash, &mut raw);
    put_int64(8, rb.head_time_ms + EXPIRATION_MS, &mut raw);
    put_bytes(11, &contract_msg, &mut raw);
    put_int64(14, timestamp_ms, &mut raw);
    put_int64(18, fee_limit_sun, &mut raw);
    raw
}

/// The transaction ID: `sha256(raw_data)`. This is also the digest the owner
/// signs.
pub fn txid(raw: &[u8]) -> [u8; 32] {
    Sha256::digest(raw).into()
}

/// Sign a transaction's [`txid`] with an EVM-curve wallet. Returns
/// `r ‖ s ‖ v` with `v` = 27/28.
pub async fn tron_sign_txid(wallet: &Wallet, raw: &[u8]) -> Result<[u8; 65]> {
    sign_digest(wallet, &B256::from(txid(raw))).await
}

/// The signed `Transaction`, hex-encoded for `/wallet/broadcasthex`.
pub fn signed_hex(raw: &[u8], sig: &[u8; 65]) -> String {
    // Transaction: raw raw_data = 1; repeated bytes signature = 2.
    let mut tx = Vec::with_capacity(raw.len() + sig.len() + 8);
    put_bytes(1, raw, &mut tx);
    put_bytes(2, sig, &mut tx);
    hex::encode(tx)
}

fn put_raw_varint(mut value: u64, buf: &mut Vec<u8>) {
    while value >= 0x80 {
        buf.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

fn put_key(tag: u32, wire_type: u8, buf: &mut Vec<u8>) {
    put_raw_varint(u64::from(tag) << 3 | u64::from(wire_type), buf);
}

/// A length-delimited field, omitted when empty (proto3 default).
fn put_bytes(tag: u32, value: &[u8], buf: &mut Vec<u8>) {
    if value.is_empty() {
        return;
    }
    put_key(tag, 2, buf);
    put_raw_varint(value.len() as u64, buf);
    buf.extend_from_slice(value);
}

/// A varint field, omitted when zero (proto3 default).
fn put_varint(tag: u32, value: u64, buf: &mut Vec<u8>) {
    if value == 0 {
        return;
    }
    put_key(tag, 0, buf);
    put_raw_varint(value, buf);
}

/// An `int64` field: a varint of the two's-complement value, omitted when
/// zero.
fn put_int64(tag: u32, value: i64, buf: &mut Vec<u8>) {
    put_varint(tag, value as u64, buf);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
