//! Chain-agnostic order primitives.
//!
//! `derive_order_id` is the single reference recipe for producing the 32-byte
//! order id that the client and arborter MUST hash identically.
//! `parse_destination_token_bytes32` is the shared cross-chain-token decoder
//! (parity-pinned against the arborter).

use eyre::{Result, eyre};
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
/// Both EVM and Solana clients MUST use this exact derivation.
///
/// **What checks it: the arborter, on every order.** It runs this recipe
/// itself (`chain_traits::market::derive_order_id`) over the `Order` it
/// verified the signature against, and uses the result throughout match,
/// settle and cancel. A caller no longer supplies an id — the
/// `OrderAuthorization` message that carried one was deleted — so what a
/// client derives here is its own copy, useful only insofar as it agrees.
///
/// Which is why it must not drift, and why the drift is nasty: every input is
/// a field of the signed order (`client_nonce` is `Order.nonce`, added for
/// exactly this reason), so a client hashing anything else still sends a
/// perfectly valid, perfectly accepted order — it simply tracks an id the
/// venue never issued, and the id is the key the ledger and `settleBatch`
/// agree a fill belongs to. Nothing on the wire reports the split.
///
/// Note what the recipe hashes — `input_amount` is the order's BUDGET, in the
/// asset it gives (a market bid's is its stated `quote_budget`), and
/// `output_amount` is zero for a market order, which has no price to expect
/// anything with. Note also what it hashes FIRST: the caller's own pubkey, so
/// an order signed by one wallet cannot derive to another's id — pre-claiming
/// someone else's id stops being expressible rather than being defended
/// against.
// The argument list mirrors arborter's hashing recipe one-to-one; bundling
// it into a struct here would just push the unpacking to every caller and
// drift more easily from the arborter side. Kept flat on purpose.
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
    if let Ok(raw) = bs58::decode(trimmed).into_vec()
        && raw.len() == 32
    {
        let mut out = [0u8; 32];
        out.copy_from_slice(&raw);
        return Ok(out);
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

/// Validate that `address` is usable as an `Order` account (settlement)
/// address on a chain of `architecture`.
///
/// The two `Order` account addresses are the per-chain settlement
/// addresses: the venue credits fill proceeds to these exact strings, only
/// the collateral-side one is authenticated by the envelope signature, and
/// funds credited to an address are withdrawable only by the holder of
/// that address's key. So a malformed or mistyped address here is stranded
/// funds — validate before signing, not after the venue has credited it.
///
/// The rules mirror what the arborter enforces at `SendOrder` entry:
/// - Solana architecture: base58 decoding to exactly 32 bytes.
/// - Everything else (EVM): a `0x`-prefixed 20-byte hex string. The prefix
///   is required — the venue's ledger only canonicalizes `0x`-prefixed
///   addresses, so an unprefixed spelling would key a separate balance.
///
/// Plus one stricter, client-only rule: a MIXED-CASE EVM address claims an
/// EIP-55 checksum, and is refused when that checksum is wrong. The venue
/// deliberately accepts any casing (the string sits inside the signature
/// and is echoed byte-verbatim), so catching the typo is this function's
/// job or nobody's.
pub fn validate_settle_address(architecture: &str, address: &str) -> Result<()> {
    if architecture.eq_ignore_ascii_case("solana") {
        #[cfg(feature = "solana")]
        return match bs58::decode(address).into_vec() {
            Ok(raw) if raw.len() == 32 => Ok(()),
            Ok(raw) => Err(eyre!(
                "'{address}' base58-decodes to {} bytes, expected exactly 32",
                raw.len()
            )),
            Err(e) => Err(eyre!("'{address}' is not a valid base58 pubkey: {e}")),
        };
        #[cfg(not(feature = "solana"))]
        return Err(eyre!(
            "cannot validate Solana address '{address}': the `solana` feature is not compiled in"
        ));
    }

    let Some(body) = address.strip_prefix("0x") else {
        return Err(eyre!(
            "EVM address '{address}' must carry the 0x prefix (the venue's ledger only \
             canonicalizes 0x-prefixed addresses, so an unprefixed spelling would key a \
             separate balance)"
        ));
    };
    if body.len() != 40 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(eyre!(
            "EVM address '{address}' must be exactly 20 bytes of hex after the 0x prefix"
        ));
    }
    let has_lower = body.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = body.bytes().any(|b| b.is_ascii_uppercase());
    if has_lower && has_upper {
        alloy_primitives::Address::parse_checksummed(address, None).map_err(|_| {
            eyre!(
                "EVM address '{address}' is mixed-case but fails its EIP-55 checksum — \
                 likely a typo; paste the address exactly, or all-lowercase to skip the check"
            )
        })?;
    }
    Ok(())
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

    // ----- validate_settle_address ---------------------------------------

    /// A genuinely EIP-55-checksummed address, COMPUTED rather than
    /// hand-typed: a hand-typed "checksummed" fixture can silently be
    /// wrong-cased (or its deliberately-broken twin accidentally right),
    /// and every assertion built on it then tests the opposite of what it
    /// says. The underlying address is the EIP-55 spec's own example, so
    /// its checksum form is known to be mixed-case.
    fn checksummed() -> String {
        let addr = "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed"
            .parse::<alloy_primitives::Address>()
            .unwrap()
            .to_checksum(None);
        // The casing the whole module leans on (from the EIP-55 spec).
        assert_eq!(addr, "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed");
        addr
    }

    #[test]
    fn settle_address_accepts_a_checksummed_evm_address() {
        assert!(validate_settle_address("evm", &checksummed()).is_ok());
    }

    /// All-lowercase carries no checksum information; the venue accepts it
    /// and the ledger canonicalizes to lowercase anyway.
    #[test]
    fn settle_address_accepts_a_lowercase_evm_address() {
        assert!(validate_settle_address("evm", &checksummed().to_ascii_lowercase()).is_ok());
    }

    /// Mixed case CLAIMS an EIP-55 checksum, so a wrong one is a typo —
    /// exactly what client-side validation exists to catch before the venue
    /// (which deliberately doesn't enforce checksums) credits the typo.
    #[test]
    fn settle_address_rejects_a_bad_evm_checksum() {
        // Swap the case of the first `aA` pair; the result is still
        // mixed-case (later letters keep their casing) but no longer the
        // checksum.
        let good = checksummed();
        let bad = good.replacen("aA", "Aa", 1);
        assert_ne!(good, bad, "the swap must change something");
        assert_ne!(
            bad,
            bad.to_ascii_lowercase(),
            "the broken fixture must stay mixed-case, or it lands on the \
             no-checksum-claimed path and is accepted for the wrong reason"
        );
        let err = validate_settle_address("evm", &bad).unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
    }

    /// The arborter refuses unprefixed hex (the ledger only canonicalizes
    /// `0x`-prefixed strings), so the client must too.
    #[test]
    fn settle_address_rejects_unprefixed_hex_on_evm() {
        assert!(validate_settle_address("evm", &checksummed()[2..]).is_err());
    }

    #[test]
    fn settle_address_rejects_wrong_length_or_garbage_on_evm() {
        assert!(validate_settle_address("evm", "0xAbCdEf01").is_err());
        assert!(
            validate_settle_address("evm", "0xnot-hex-at-all-000000000000000000000000").is_err()
        );
        assert!(validate_settle_address("evm", "").is_err());
    }

    #[cfg(feature = "solana")]
    #[test]
    fn settle_address_accepts_a_32_byte_base58_pubkey() {
        let pk = bs58::encode([7u8; 32]).into_string();
        assert!(validate_settle_address("solana", &pk).is_ok());
        // Architecture matching is case-insensitive, like `chain_curve`.
        assert!(validate_settle_address("Solana", &pk).is_ok());
    }

    #[cfg(feature = "solana")]
    #[test]
    fn settle_address_rejects_non_pubkeys_on_solana() {
        // Wrong decoded length.
        assert!(validate_settle_address("solana", "abc").is_err());
        // Not base58 at all: `0` is outside the alphabet, so an EVM address
        // on a Solana leg fails loudly rather than being reinterpreted.
        assert!(validate_settle_address("solana", &checksummed()).is_err());
        assert!(validate_settle_address("solana", "").is_err());
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
