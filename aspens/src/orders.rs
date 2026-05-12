//! Chain-agnostic order primitives.
//!
//! `derive_order_id` is the single reference recipe for producing the 32-byte
//! order id that the client and arborter MUST hash identically. `GaslessLockParams`
//! is the shared input struct fed to chain-specific signing helpers
//! (`aspens::evm::gasless_lock_signing_hash`, `aspens::solana::gasless_lock_signing_message`).

use eyre::{eyre, Result};
use sha2::{Digest, Sha256};

/// Derive the canonical 32-byte order id.
///
/// Hash layout (all little-endian where applicable):
/// ```text
/// sha256(
///     user_pubkey || client_nonce || origin_chain_id || destination_chain_id ||
///     input_token || output_token || input_amount || output_amount
/// )
/// ```
///
/// Both EVM and Solana clients MUST use this exact derivation — the arborter
/// rehashes with the same recipe and will reject orders whose id doesn't match.
#[allow(clippy::too_many_arguments)]
pub fn derive_order_id(
    user_pubkey: &[u8],
    client_nonce: u64,
    origin_chain_id: u64,
    destination_chain_id: u64,
    input_token: &[u8],
    output_token: &[u8],
    input_amount: u128,
    output_amount: u128,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(user_pubkey);
    h.update(client_nonce.to_le_bytes());
    h.update(origin_chain_id.to_le_bytes());
    h.update(destination_chain_id.to_le_bytes());
    h.update(input_token);
    h.update(output_token);
    h.update(input_amount.to_le_bytes());
    h.update(output_amount.to_le_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// Shared input struct fed to chain-specific signing helpers.
///
/// Fields are chain-specific where noted; harmless-but-ignored defaults are
/// fine for the other chain. A client constructs one of these and passes it
/// to either `aspens::evm::gasless_lock_signing_hash` or
/// `aspens::solana::gasless_lock_signing_message`.
#[derive(Debug, Clone)]
pub struct GaslessLockParams<'a> {
    /// User funding the lock — hex address on EVM, base58 pubkey on Solana.
    pub depositor_address: &'a str,
    /// Address / mint of the token being deposited on the origin chain.
    pub token_contract: &'a str,
    /// Address / mint of the token the user expects to receive on the
    /// destination chain.
    pub token_contract_destination_chain: &'a str,
    /// Chain id of the destination chain (decimal string).
    pub destination_chain_id: &'a str,
    /// Amount of `token_contract` being deposited, in that token's
    /// native base units (NOT pair_decimals). The on-chain EIP-712 /
    /// Ed25519 digest is computed over this exact integer; the
    /// arborter and contract recompute identically only when they
    /// receive the same value. Callers feeding values from the
    /// matching engine's pair-decimal representation must normalise
    /// first via `gasless::normalize` (private) or its public mirror
    /// `chain_traits::convert_decimals::normalize_decimals` (arborter).
    pub amount_in: u128,
    /// Amount of `token_contract_destination_chain` the user expects
    /// out, in that token's native base units. Same scale convention
    /// as `amount_in` above.
    pub amount_out: u128,
    /// Opaque order id — typically a 32-byte hex string. On Solana this
    /// is the key under which the `Order` PDA is `init`-ed; on EVM it's
    /// the intent id. Chains that want to derive it internally may
    /// accept an empty string.
    pub order_id: &'a str,
    /// Chain-specific absolute deadline:
    ///   * Solana: slot number.
    ///   * EVM:    unix-seconds `fillDeadline` stamped on the GaslessCrossChainOrder.
    pub deadline: u64,
    /// Permit2 / EIP-712 nonce, embedded in the EVM `PermitSingle`. The user's
    /// signature is computed over the exact struct that includes this nonce,
    /// so the arborter must pass it through verbatim. Ignored by Solana
    /// (the `Order` PDA's `init` serves as the single-use nonce).
    pub nonce: u64,
    /// EVM-only `openDeadline` field on `GaslessCrossChainOrder` (unix
    /// seconds). Ignored by Solana.
    pub open_deadline: u64,
    /// User-produced signature. 64 bytes Ed25519 on Solana; 65 bytes
    /// ECDSA on EVM; length and semantics are chain-specific.
    pub user_signature: &'a [u8],
}

/// Decode a cross-chain destination token identifier into a 32-byte slot.
///
/// The EVM `OrderData.outputToken` field is `bytes32`, sized to fit any
/// 32-byte-or-less token id natively. Inputs:
/// - `0x`-prefixed hex (case-insensitive). Up to 32 bytes (64 hex chars);
///   shorter inputs (e.g. a 20-byte EVM address) are LEFT-padded with
///   zeros to match `bytes32(uint256(uint160(addr)))` casts on-chain.
/// - bare hex (no `0x` prefix), same rules as above.
/// - base58 32-byte pubkey (Solana mints, etc.); must decode to exactly
///   32 bytes. Requires the `solana` feature.
///
/// ## Hex vs. base58 disambiguation
///
/// The base58 alphabet `[1-9A-HJ-NP-Za-km-z]` overlaps with hex at
/// `[1-9a-fA-F]`. A string composed entirely of that intersection is
/// syntactically valid as either — for example the Solana System Program
/// pubkey `"11111111111111111111111111111111"` (32 chars of `'1'`) is
/// both valid base58 (decoding to 32 zero bytes) and valid hex (decoding
/// to 16 bytes of `0x11`).
///
/// To handle these without surprising Solana callers, an input *without*
/// the `0x` prefix is tried as base58 first; we accept it only if base58
/// decodes to **exactly 32 bytes**. Other base58 lengths (16-byte
/// vanity addresses, short pubkeys, etc.) fall through to the hex path,
/// which preserves backwards compatibility for bare-hex EVM addresses.
/// A `0x` prefix forces the hex path unconditionally.
///
/// Errors on inputs that decode to >32 bytes or are otherwise unparseable.
///
/// **Parity:** mirrors
/// `arborter::chain_traits::market::parse_destination_token_bytes32` exactly.
/// Any change here must be mirrored there. Pinned by snapshot tests in
/// `tests/client_parity.rs`.
pub fn parse_destination_token_bytes32(token: &str) -> Result<[u8; 32]> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(eyre!("empty destination token"));
    }

    // `0x` prefix forces hex — base58 can never start with `0x` anyway
    // (`0` is not in the base58 alphabet).
    if let Some(hex_body) = trimmed.strip_prefix("0x") {
        return decode_hex_to_bytes32(hex_body, trimmed);
    }

    // Unprefixed input: prefer a successful 32-byte base58 decode. This
    // is the only way to disambiguate inputs that are valid as both
    // (e.g. the 32-char all-`'1'` Solana System Program pubkey).
    #[cfg(feature = "solana")]
    if let Ok(raw) = bs58::decode(trimmed).into_vec() {
        if raw.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&raw);
            return Ok(out);
        }
    }

    // Not a 32-byte base58 (or `solana` feature off). Fall back to hex.
    if !trimmed.is_empty() && trimmed.len() <= 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        return decode_hex_to_bytes32(trimmed, trimmed);
    }

    #[cfg(feature = "solana")]
    {
        Err(eyre!(
            "destination token '{}' is neither a 32-byte base58 pubkey nor a valid \
             hex string of ≤32 bytes",
            trimmed
        ))
    }

    #[cfg(not(feature = "solana"))]
    Err(eyre!(
        "non-hex destination token '{}' requires the `solana` feature",
        trimmed
    ))
}

/// Hex → left-padded `[u8; 32]`. Shared between the `0x`-prefixed and
/// bare-hex fallback paths. `display` is the original string used for
/// error messages so the operator sees what they actually passed in.
fn decode_hex_to_bytes32(hex_body: &str, display: &str) -> Result<[u8; 32]> {
    if hex_body.is_empty() {
        return Err(eyre!("empty hex body in '{}'", display));
    }
    if hex_body.len() > 64 {
        return Err(eyre!(
            "hex token '{}' has {} hex chars; max 64 (32 bytes)",
            display,
            hex_body.len()
        ));
    }
    if !hex_body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(eyre!("hex token '{}' contains non-hex characters", display));
    }
    let normalized = if hex_body.len().is_multiple_of(2) {
        hex_body.to_string()
    } else {
        format!("0{hex_body}")
    };
    let raw =
        hex::decode(&normalized).map_err(|e| eyre!("invalid hex token '{}': {}", display, e))?;
    let mut out = [0u8; 32];
    out[32 - raw.len()..].copy_from_slice(&raw);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_id_is_deterministic() {
        let a = derive_order_id(&[1; 32], 42, 1, 501, b"0xaaa", b"MintXYZ", 100, 200);
        let b = derive_order_id(&[1; 32], 42, 1, 501, b"0xaaa", b"MintXYZ", 100, 200);
        assert_eq!(a, b);
    }

    #[test]
    fn order_id_changes_with_nonce() {
        let a = derive_order_id(&[1; 32], 1, 1, 501, b"t1", b"t2", 100, 200);
        let b = derive_order_id(&[1; 32], 2, 1, 501, b"t1", b"t2", 100, 200);
        assert_ne!(a, b);
    }

    #[test]
    fn order_id_endianness_is_le() {
        // If the hash ever changes to BE we need to coordinate with arborter,
        // so pin the canonical bytes for a known input.
        let id = derive_order_id(&[], 0, 0, 0, &[], &[], 0, 0);
        // sha256 of 8*8 = 64 zero bytes (5 u64 LE zero fields + 2 u128 LE zero).
        // Total: 0 + 8 + 8 + 8 + 0 + 0 + 16 + 16 = 56 bytes of zeros.
        let mut h = Sha256::new();
        h.update([0u8; 56]);
        let mut want = [0u8; 32];
        want.copy_from_slice(&h.finalize());
        assert_eq!(id, want);
    }

    #[test]
    fn parse_hex_20_byte_address_left_pads() {
        let evm = "0x".to_string() + &"ab".repeat(20);
        let bytes = parse_destination_token_bytes32(&evm).unwrap();
        assert_eq!(&bytes[..12], &[0u8; 12]);
        assert_eq!(&bytes[12..], &[0xabu8; 20]);
    }

    #[test]
    fn parse_hex_32_byte_passes_through() {
        let h = "0x".to_string() + &"cd".repeat(32);
        let bytes = parse_destination_token_bytes32(&h).unwrap();
        assert_eq!(bytes, [0xcdu8; 32]);
    }

    #[test]
    fn parse_hex_without_0x_prefix_works() {
        let h = "ab".repeat(20);
        let bytes = parse_destination_token_bytes32(&h).unwrap();
        assert_eq!(&bytes[12..], &[0xabu8; 20]);
    }

    #[cfg(feature = "solana")]
    #[test]
    fn parse_base58_solana_pubkey() {
        let raw = [0x42u8; 32];
        let b58 = bs58::encode(raw).into_string();
        let bytes = parse_destination_token_bytes32(&b58).unwrap();
        assert_eq!(bytes, raw);
    }

    #[test]
    fn parse_rejects_too_long_hex() {
        let h = "0x".to_string() + &"ab".repeat(33);
        assert!(parse_destination_token_bytes32(&h).is_err());
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(parse_destination_token_bytes32("").is_err());
        assert!(parse_destination_token_bytes32("   ").is_err());
    }

    /// Regression: Solana's System Program / null pubkey base58-encodes as
    /// 32 `'1'` characters, which is *also* syntactically valid hex (16
    /// bytes of `0x11`). Previously the hex path won and silently
    /// truncated. The unprefixed input must decode as base58 → 32 zero
    /// bytes; the `0x`-prefixed form must still go down the hex path.
    /// Mirrors the same regression test in chain-traits.
    #[cfg(feature = "solana")]
    #[test]
    fn parse_ambiguous_base58_zero_pubkey_decodes_as_base58() {
        let zero_pubkey_base58 = bs58::encode([0u8; 32]).into_string();
        assert_eq!(zero_pubkey_base58, "11111111111111111111111111111111");

        let parsed = parse_destination_token_bytes32(&zero_pubkey_base58).unwrap();
        assert_eq!(parsed, [0u8; 32], "unprefixed 32-byte base58 wins");

        let with_prefix = format!("0x{}", zero_pubkey_base58);
        let parsed = parse_destination_token_bytes32(&with_prefix).unwrap();
        let mut expected = [0u8; 32];
        expected[16..].copy_from_slice(&[0x11u8; 16]);
        assert_eq!(parsed, expected, "0x prefix forces hex");
    }
}
