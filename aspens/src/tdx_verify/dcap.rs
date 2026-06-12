//! DCAP-backed `QuoteVerifier` using the pure-Rust `dcap-qvl` crate. Behind the
//! `dcap` feature so the lean signing build pulls none of it.
//!
//! This is the "claim 1" backend: it checks the TD Quote's ECDSA chain to the
//! Intel SGX Root CA and its TCB status against operator-supplied collateral,
//! then maps the verified TD report onto a `VerifiedQuote` for the rest of the
//! `verify_attestation` pipeline (measurement policy + REPORTDATA).

use super::{QuoteVerifier, VerifiedQuote, VerifyError};
use dcap_qvl::QuoteCollateralV3;
use dcap_qvl::verify::QuoteVerifier as DcapVerifier;

/// A [`QuoteVerifier`] backed by `dcap-qvl`, verifying against Intel's production
/// root CA.
///
/// **Collateral** (TCB info, QE identity, PCK CRL + issuer chains) is supplied by
/// the caller — fetch it from Intel PCS or an operator-run PCCS out of band. This
/// adapter pulls no HTTP machinery, preserving the SDK's lean-build invariant.
///
/// `now_secs` is the verification timestamp (Unix seconds) used for TCB / cert
/// expiry checks — pass the current time when verifying live.
pub struct DcapQuoteVerifier {
    collateral: QuoteCollateralV3,
    now_secs: u64,
    accepted_tcb: Vec<String>,
    allow_debug: bool,
}

impl DcapQuoteVerifier {
    /// New verifier. By default only an `UpToDate` TCB status is accepted and
    /// debug-mode TDs are rejected — the safe defaults for production.
    pub fn new(collateral: QuoteCollateralV3, now_secs: u64) -> Self {
        Self {
            collateral,
            now_secs,
            accepted_tcb: vec!["UpToDate".to_string()],
            allow_debug: false,
        }
    }

    /// Override the set of acceptable TCB status strings (e.g. to also allow
    /// `"SWHardeningNeeded"`). `OutOfDate` / `Revoked` must be treated as failures
    /// per policy — only allow-list statuses you have explicitly accepted.
    pub fn accept_tcb_statuses(mut self, statuses: Vec<String>) -> Self {
        self.accepted_tcb = statuses;
        self
    }

    /// Allow debug-mode TDs (TDX `TUD.DEBUG`). Default: rejected.
    pub fn allow_debug(mut self, allow: bool) -> Self {
        self.allow_debug = allow;
        self
    }
}

impl QuoteVerifier for DcapQuoteVerifier {
    fn verify_quote(&self, raw_quote: &[u8]) -> Result<VerifiedQuote, VerifyError> {
        // ECDSA chain to the Intel SGX Root CA + QE/PCK checks (claim 1).
        let report = DcapVerifier::new_prod()
            .allow_debug(self.allow_debug)
            .verify(raw_quote, &self.collateral, self.now_secs)
            .map_err(|e| VerifyError::QuoteVerification(format!("{e:?}")))?;

        // TCB status policy — reject OutOfDate / Revoked / ConfigurationNeeded /…
        // unless the operator explicitly allow-listed the status.
        if !self.accepted_tcb.iter().any(|s| s == &report.status) {
            return Err(VerifyError::QuoteVerification(format!(
                "unacceptable TCB status {:?} (advisories: {:?})",
                report.status, report.advisory_ids
            )));
        }

        // Require a TDX TD report (TD 1.0 / 1.5), not an SGX enclave report.
        let td = report.report.as_td10().ok_or_else(|| {
            VerifyError::QuoteVerification("quote is not a TDX TD report".to_string())
        })?;

        Ok(VerifiedQuote {
            mr_td: td.mr_td,
            rt_mr: [td.rt_mr0, td.rt_mr1, td.rt_mr2, td.rt_mr3],
            mr_seam: td.mr_seam,
            mr_signer_seam: td.mr_signer_seam,
            td_attributes: td.td_attributes,
            xfam: td.xfam,
            report_data: td.report_data,
        })
    }
}
