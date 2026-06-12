//! TDX attestation verification — the relying-party side of
//! `ATTESTATION_QUOTE_DESIGN.md` §4.7.
//!
//! The signer produces a single Intel-signed **TD Quote** whose REPORTDATA binds
//! its tx-pubkey manifest, the running image digests, and caller-supplied data
//! (see [`reportdata`]). A relying party verifies it in three steps, fail-closed:
//!
//! 1. **Genuine TEE (claim 1):** DCAP/QVL-verify the quote's ECDSA chain to the
//!    Intel SGX Root CA and its TCB status — done by a [`QuoteVerifier`].
//! 2. **Measurement policy (claim 2):** pin expected `MRTD`/`RTMR[..]`/seam
//!    measurements against operator values ([`MeasurementPolicy`]). A valid
//!    signature over *some* TD is not enough.
//! 3. **REPORTDATA (claims 2+3 + freshness):** recompute
//!    `SHA-512(DOMAIN ‖ H(pubkey_manifest) ‖ H(images) ‖ H(report_data))` from the
//!    *expected* values and require it to equal the verified quote's REPORTDATA.
//!
//! The DCAP step ([`QuoteVerifier`]) is pluggable. A pure-Rust backend
//! ([`dcap::DcapQuoteVerifier`], `dcap-qvl`) ships behind the `dcap` feature; the
//! reconstruction, measurement policy, and pipeline are always built and
//! host-tested. The remaining integration step is end-to-end validation against a
//! real quote + collateral (hardware), plus the operator's collateral source
//! (Intel PCS / PCCS) and measurement-policy config.

pub mod reportdata;

/// DCAP quote-verification backend (`QuoteVerifier` impl). Requires the `dcap`
/// feature; the rest of this module (reconstruction + pipeline) is always built.
#[cfg(feature = "dcap")]
pub mod dcap;

use reportdata::{CurveTag, expected_reportdata};
use std::fmt;

/// TDX measurement register width (SHA-384): MRTD, RTMR[0..3], MRSEAM, etc.
pub const MEASUREMENT_LEN: usize = 48;
/// A 48-byte TDX measurement.
pub type Measurement = [u8; MEASUREMENT_LEN];

/// The verified contents of a TD Quote, as returned by a [`QuoteVerifier`] after
/// it has checked the ECDSA chain to the Intel SGX Root CA and the TCB status.
#[derive(Clone, Debug)]
pub struct VerifiedQuote {
    pub mr_td: Measurement,
    pub rt_mr: [Measurement; 4],
    pub mr_seam: Measurement,
    pub mr_signer_seam: Measurement,
    pub td_attributes: [u8; 8],
    pub xfam: [u8; 8],
    /// The 64-byte REPORTDATA the TD bound into the quote.
    pub report_data: [u8; 64],
}

/// Operator-pinned expected measurements (claim 2). `None` = not pinned (skipped);
/// a pinned value must match the quote exactly. Pinning `MRTD` and the `RTMR`s is
/// effectively mandatory — without it, any genuine TDX TD would pass (design §4.7
/// step 3).
#[derive(Clone, Default)]
pub struct MeasurementPolicy {
    pub mr_td: Option<Measurement>,
    pub rt_mr: [Option<Measurement>; 4],
    pub mr_seam: Option<Measurement>,
    pub mr_signer_seam: Option<Measurement>,
    pub td_attributes: Option<[u8; 8]>,
    pub xfam: Option<[u8; 8]>,
}

impl MeasurementPolicy {
    fn check(&self, q: &VerifiedQuote) -> Result<(), VerifyError> {
        fn pin<const N: usize>(
            field: &'static str,
            want: &Option<[u8; N]>,
            got: &[u8; N],
        ) -> Result<(), VerifyError> {
            match want {
                Some(w) if w != got => Err(VerifyError::MeasurementMismatch(field)),
                _ => Ok(()),
            }
        }
        pin("mr_td", &self.mr_td, &q.mr_td)?;
        pin("rt_mr0", &self.rt_mr[0], &q.rt_mr[0])?;
        pin("rt_mr1", &self.rt_mr[1], &q.rt_mr[1])?;
        pin("rt_mr2", &self.rt_mr[2], &q.rt_mr[2])?;
        pin("rt_mr3", &self.rt_mr[3], &q.rt_mr[3])?;
        pin("mr_seam", &self.mr_seam, &q.mr_seam)?;
        pin("mr_signer_seam", &self.mr_signer_seam, &q.mr_signer_seam)?;
        pin("td_attributes", &self.td_attributes, &q.td_attributes)?;
        pin("xfam", &self.xfam, &q.xfam)?;
        Ok(())
    }
}

/// What the relying party expects the quote's REPORTDATA to bind: the signer's tx
/// pubkeys (claim 3), the running image digests (claim 2), and the opaque
/// `report_data` the verifier supplied to `GetAttestation` (a freshness nonce
/// and/or external state). These are recomputed into the 64-byte REPORTDATA and
/// compared against the verified quote.
#[derive(Clone, Default)]
pub struct ExpectedReportData {
    /// Expected tx pubkeys (operator-known): one secp256k1 per EVM chain, one
    /// Ed25519 per Solana chain. Order-independent — the manifest is canonicalized.
    pub pubkeys: Vec<(CurveTag, Vec<u8>)>,
    /// Expected running image digest(s), exactly as the signer reads them.
    pub image_digests: Vec<u8>,
    /// The opaque bytes the verifier passed as `report_data` to `GetAttestation`.
    pub report_data: Vec<u8>,
}

/// Verifies a raw TD Quote's signature chain to the Intel SGX Root CA and its TCB
/// status, returning the parsed, verified quote body. The concrete backend is
/// pluggable (vetted DCAP crate vs Intel QVL FFI vs operator QVE).
///
/// No implementation ships yet — see the module docs (phase 2: DCAP backend).
pub trait QuoteVerifier {
    fn verify_quote(&self, raw_quote: &[u8]) -> Result<VerifiedQuote, VerifyError>;
}

/// Run the full relying-party verification, fail-closed. On success returns the
/// verified quote body (so the caller can read its measurements/TCB). Any failing
/// step — DCAP, measurement policy, or REPORTDATA — rejects the attestation.
pub fn verify_attestation(
    raw_quote: &[u8],
    verifier: &dyn QuoteVerifier,
    policy: &MeasurementPolicy,
    expected: &ExpectedReportData,
) -> Result<VerifiedQuote, VerifyError> {
    if raw_quote.is_empty() {
        // An empty raw_quote means the signer produced no quote (non-attesting).
        return Err(VerifyError::EmptyQuote);
    }

    // Claim 1 — genuine TEE: DCAP/QVL signature chain + TCB status.
    let quote = verifier.verify_quote(raw_quote)?;

    // Claim 2 — measurement policy: pin MRTD/RTMR/seam against operator values.
    policy.check(&quote)?;

    // Claims 2+3 + freshness — REPORTDATA must bind exactly the keys, images, and
    // caller data we expect.
    let expected_rd = expected_reportdata(
        &expected.pubkeys,
        &expected.image_digests,
        &expected.report_data,
    );
    if quote.report_data != expected_rd {
        return Err(VerifyError::ReportDataMismatch);
    }

    Ok(quote)
}

/// Why an attestation was rejected. Distinct variants so a caller can tell a
/// crypto/TCB failure from a policy or REPORTDATA mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// `raw_quote` was empty — the signer produced no (verifiable) quote.
    EmptyQuote,
    /// DCAP/QVL verification failed (bad signature chain, TCB out-of-date/revoked,
    /// or the quote couldn't be parsed).
    QuoteVerification(String),
    /// A pinned measurement did not match the quote (field name).
    MeasurementMismatch(&'static str),
    /// The recomputed REPORTDATA did not equal the quote's — the quote does not
    /// bind the expected keys/images/report_data.
    ReportDataMismatch,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::EmptyQuote => {
                write!(
                    f,
                    "attestation rejected: empty raw_quote (signer produced no TD Quote)"
                )
            }
            VerifyError::QuoteVerification(e) => {
                write!(
                    f,
                    "attestation rejected: DCAP/QVL quote verification failed: {e}"
                )
            }
            VerifyError::MeasurementMismatch(field) => {
                write!(
                    f,
                    "attestation rejected: measurement {field} does not match policy"
                )
            }
            VerifyError::ReportDataMismatch => write!(
                f,
                "attestation rejected: REPORTDATA mismatch — quote does not bind the expected \
                 pubkeys/images/report_data"
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn meas(b: u8) -> Measurement {
        [b; MEASUREMENT_LEN]
    }

    /// A stub verifier returning a fixed quote body — lets us exercise the
    /// pipeline (policy + REPORTDATA) without a real DCAP backend.
    struct StubVerifier(VerifiedQuote);
    impl QuoteVerifier for StubVerifier {
        fn verify_quote(&self, _raw_quote: &[u8]) -> Result<VerifiedQuote, VerifyError> {
            Ok(self.0.clone())
        }
    }

    fn quote_with_reportdata(report_data: [u8; 64]) -> VerifiedQuote {
        VerifiedQuote {
            mr_td: meas(0x11),
            rt_mr: [meas(0x20), meas(0x21), meas(0x22), meas(0x23)],
            mr_seam: meas(0x30),
            mr_signer_seam: meas(0x31),
            td_attributes: [0u8; 8],
            xfam: [0u8; 8],
            report_data,
        }
    }

    fn expected() -> ExpectedReportData {
        ExpectedReportData {
            pubkeys: vec![(CurveTag::Secp256k1, b"pubkey-evm".to_vec())],
            image_digests: b"img".to_vec(),
            report_data: b"nonce".to_vec(),
        }
    }

    #[test]
    fn accepts_matching_quote() {
        let exp = expected();
        let rd = expected_reportdata(&exp.pubkeys, &exp.image_digests, &exp.report_data);
        let v = StubVerifier(quote_with_reportdata(rd));
        let mut policy = MeasurementPolicy::default();
        policy.mr_td = Some(meas(0x11));
        policy.rt_mr[0] = Some(meas(0x20));
        assert!(verify_attestation(b"raw", &v, &policy, &exp).is_ok());
    }

    #[test]
    fn rejects_reportdata_mismatch() {
        let exp = expected();
        // Quote binds a DIFFERENT report_data than expected.
        let wrong = expected_reportdata(&exp.pubkeys, &exp.image_digests, b"different-nonce");
        let v = StubVerifier(quote_with_reportdata(wrong));
        let err = verify_attestation(b"raw", &v, &MeasurementPolicy::default(), &exp).unwrap_err();
        assert_eq!(err, VerifyError::ReportDataMismatch);
    }

    #[test]
    fn rejects_measurement_mismatch() {
        let exp = expected();
        let rd = expected_reportdata(&exp.pubkeys, &exp.image_digests, &exp.report_data);
        let v = StubVerifier(quote_with_reportdata(rd));
        let mut policy = MeasurementPolicy::default();
        policy.mr_td = Some(meas(0xFF)); // wrong
        let err = verify_attestation(b"raw", &v, &policy, &exp).unwrap_err();
        assert_eq!(err, VerifyError::MeasurementMismatch("mr_td"));
    }

    #[test]
    fn rejects_empty_quote_before_calling_verifier() {
        struct Panicking;
        impl QuoteVerifier for Panicking {
            fn verify_quote(&self, _: &[u8]) -> Result<VerifiedQuote, VerifyError> {
                panic!("must not be called for an empty quote");
            }
        }
        let err = verify_attestation(b"", &Panicking, &MeasurementPolicy::default(), &expected())
            .unwrap_err();
        assert_eq!(err, VerifyError::EmptyQuote);
    }

    #[test]
    fn rejects_unpinned_is_allowed_but_pinned_must_match() {
        // An all-default policy pins nothing → passes the policy step (the
        // REPORTDATA + DCAP steps are what carry the weight then).
        let exp = expected();
        let rd = expected_reportdata(&exp.pubkeys, &exp.image_digests, &exp.report_data);
        let v = StubVerifier(quote_with_reportdata(rd));
        assert!(verify_attestation(b"raw", &v, &MeasurementPolicy::default(), &exp).is_ok());
    }
}
