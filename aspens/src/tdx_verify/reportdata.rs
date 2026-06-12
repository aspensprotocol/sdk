//! Relying-party reconstruction of the signer's attestation `REPORTDATA` and the
//! pubkey manifest it binds, so a verifier can recompute the value and require it
//! to equal a *verified* TD Quote's REPORTDATA (ATTESTATION_QUOTE_DESIGN.md
//! §4.4/§4.7).
//!
//! This MUST stay byte-for-byte identical to the signer's producer side:
//! `signer/src/handlers/reportdata.rs` (the SHA-512 assembly) and
//! `signer/src/handlers/keymanifest.rs` (the canonical manifest). The domain
//! constants and curve tags below are copied from there; the unit tests pin the
//! exact wire bytes so a future divergence on either side is caught.

use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeSet;

/// Versioned domain prefix for REPORTDATA assembly (signer `reportdata::DOMAIN`).
pub const REPORTDATA_DOMAIN: &[u8] = b"aspens-signer/reportdata/v1";

/// Versioned domain prefix for the pubkey manifest (signer
/// `keymanifest::MANIFEST_DOMAIN`).
pub const MANIFEST_DOMAIN: &[u8] = b"aspens-signer/pubkey-manifest/v1";

/// One-byte curve discriminator recorded alongside each pubkey in the manifest
/// (signer `handlers::CURVE_TAG_*`). Values are stable and independent of any
/// proto enum numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum CurveTag {
    Secp256k1 = 0x01,
    Ed25519 = 0x02,
}

impl CurveTag {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Serialize a set of `(curve_tag, pubkey)` entries to the canonical manifest
/// bytes the signer's `KeyRegistry::manifest_bytes` produces:
///
/// ```text
/// MANIFEST_DOMAIN ‖ u32_be(count) ‖
///   for each (curve_tag, pubkey) in ascending sorted order:
///     curve_tag(1) ‖ u32_be(len(pubkey)) ‖ pubkey
/// ```
///
/// Entries are sorted and de-duplicated (the signer uses a `BTreeSet<(u8,
/// Vec<u8>)>`), and empty pubkeys are dropped — so the output is independent of
/// the order the operator lists the expected keys in.
pub fn manifest_bytes(entries: &[(CurveTag, Vec<u8>)]) -> Vec<u8> {
    let set: BTreeSet<(u8, Vec<u8>)> = entries
        .iter()
        .filter(|(_, pk)| !pk.is_empty())
        .map(|(tag, pk)| (tag.as_u8(), pk.clone()))
        .collect();

    let mut out = Vec::with_capacity(MANIFEST_DOMAIN.len() + 4 + set.len() * 70);
    out.extend_from_slice(MANIFEST_DOMAIN);
    out.extend_from_slice(&(set.len() as u32).to_be_bytes());
    for (tag, pk) in &set {
        out.push(*tag);
        out.extend_from_slice(&(pk.len() as u32).to_be_bytes());
        out.extend_from_slice(pk);
    }
    out
}

/// Reconstruct the 64-byte REPORTDATA from its three pre-hashed inputs:
///
/// `REPORTDATA = SHA-512( DOMAIN ‖ SHA256(pubkey_manifest) ‖ SHA256(image_digests) ‖ SHA256(report_data) )`
pub fn reconstruct_reportdata(
    pubkey_manifest: &[u8],
    image_digests: &[u8],
    report_data: &[u8],
) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(REPORTDATA_DOMAIN);
    h.update(Sha256::digest(pubkey_manifest));
    h.update(Sha256::digest(image_digests));
    h.update(Sha256::digest(report_data));
    let out = h.finalize();
    let mut rd = [0u8; 64];
    rd.copy_from_slice(&out);
    rd
}

/// Convenience: reconstruct REPORTDATA directly from the expected tx pubkeys, the
/// expected image digests, and the caller-supplied `report_data` (nonce / external
/// state). Equivalent to `reconstruct_reportdata(&manifest_bytes(pubkeys), …)`.
pub fn expected_reportdata(
    pubkeys: &[(CurveTag, Vec<u8>)],
    image_digests: &[u8],
    report_data: &[u8],
) -> [u8; 64] {
    reconstruct_reportdata(&manifest_bytes(pubkeys), image_digests, report_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_empty_is_domain_plus_zero_count() {
        // Locks the exact wire bytes for the empty manifest.
        let mut expected = Vec::new();
        expected.extend_from_slice(MANIFEST_DOMAIN);
        expected.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(manifest_bytes(&[]), expected);
    }

    #[test]
    fn manifest_one_key_exact_bytes() {
        // tag(0x01) ‖ u32_be(4) ‖ "key!" — pins the per-entry framing.
        let pk = b"key!".to_vec();
        let mut expected = Vec::new();
        expected.extend_from_slice(MANIFEST_DOMAIN);
        expected.extend_from_slice(&1u32.to_be_bytes()); // count
        expected.push(0x01); // CurveTag::Secp256k1
        expected.extend_from_slice(&4u32.to_be_bytes()); // len
        expected.extend_from_slice(&pk);
        assert_eq!(manifest_bytes(&[(CurveTag::Secp256k1, pk)]), expected);
    }

    #[test]
    fn manifest_is_order_independent_and_dedups() {
        let a = (CurveTag::Secp256k1, b"aaaa".to_vec());
        let b = (CurveTag::Ed25519, b"bbbb".to_vec());
        let one = manifest_bytes(&[a.clone(), b.clone()]);
        let two = manifest_bytes(&[b.clone(), a.clone(), a.clone()]); // reversed + dup
        assert_eq!(one, two);
        // count is 2, not 3 (dup collapsed).
        assert_eq!(
            &two[MANIFEST_DOMAIN.len()..MANIFEST_DOMAIN.len() + 4],
            &[0, 0, 0, 2]
        );
    }

    #[test]
    fn manifest_distinguishes_curve_tag() {
        // Same bytes under different curve tags are distinct entries.
        let m = manifest_bytes(&[
            (CurveTag::Secp256k1, b"same".to_vec()),
            (CurveTag::Ed25519, b"same".to_vec()),
        ]);
        assert_eq!(
            &m[MANIFEST_DOMAIN.len()..MANIFEST_DOMAIN.len() + 4],
            &[0, 0, 0, 2]
        );
    }

    #[test]
    fn manifest_skips_empty_pubkeys() {
        assert_eq!(
            manifest_bytes(&[(CurveTag::Ed25519, Vec::new())]),
            manifest_bytes(&[])
        );
    }

    #[test]
    fn reportdata_matches_independent_sha_assembly() {
        // Independently recompute the SHA-512 over the documented preimage and
        // confirm the helper assembles the four parts in the right order.
        let manifest = b"manifest-bytes";
        let images = b"img-digests";
        let rdata = b"nonce";
        let got = reconstruct_reportdata(manifest, images, rdata);

        let mut h = Sha512::new();
        h.update(REPORTDATA_DOMAIN);
        h.update(Sha256::digest(manifest));
        h.update(Sha256::digest(images));
        h.update(Sha256::digest(rdata));
        let want: [u8; 64] = h.finalize().into();
        assert_eq!(got, want);
    }

    #[test]
    fn reportdata_is_sensitive_to_each_input_and_deterministic() {
        let base = expected_reportdata(&[(CurveTag::Secp256k1, b"k".to_vec())], b"img", b"n");
        assert_eq!(
            base,
            expected_reportdata(&[(CurveTag::Secp256k1, b"k".to_vec())], b"img", b"n")
        );
        assert_ne!(
            base,
            expected_reportdata(&[(CurveTag::Secp256k1, b"K".to_vec())], b"img", b"n")
        );
        assert_ne!(
            base,
            expected_reportdata(&[(CurveTag::Secp256k1, b"k".to_vec())], b"IMG", b"n")
        );
        assert_ne!(
            base,
            expected_reportdata(&[(CurveTag::Secp256k1, b"k".to_vec())], b"img", b"N")
        );
    }
}
