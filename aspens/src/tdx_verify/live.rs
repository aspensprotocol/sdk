//! Live end-to-end attestation verification against a running Aspens stack.
//! Behind `client` + `dcap-fetch`: fetch the attestation over gRPC, fetch its
//! DCAP collateral, and run the fail-closed `verify_attestation` pipeline.

use super::collateral::fetch_collateral;
use super::dcap::DcapQuoteVerifier;
use super::reportdata::CurveTag;
use super::{
    ExpectedReportData, MeasurementPolicy, VerifiedQuote, VerifyError, verify_attestation,
};

/// Operator policy + expectations for a live verification.
#[derive(Clone)]
pub struct LiveVerifyParams {
    /// PCCS / Intel PCS base URL for collateral
    /// (see [`super::collateral::INTEL_PCS_URL`]).
    pub pccs_url: String,
    /// Pinned measurements (claim 2). Pin MRTD + the RTMRs in production — a valid
    /// signature over *some* TD is not enough.
    pub policy: MeasurementPolicy,
    /// Expected tx pubkeys the quote must bind (claim 3). **Operator-known and
    /// out-of-band** — must NOT be fetched from the attested stack, which would be
    /// circular. Raw pubkey bytes (65-byte secp256k1 / 32-byte Ed25519), matching
    /// what the signer derives and records in its manifest.
    pub expected_pubkeys: Vec<(CurveTag, Vec<u8>)>,
    /// Expected running image digest(s) the signer self-reports (often empty).
    pub expected_image_digests: Vec<u8>,
    /// Acceptable TCB statuses (default policy: only `"UpToDate"`).
    pub accepted_tcb: Vec<String>,
    /// Verification timestamp (Unix seconds) for TCB / cert-expiry checks.
    pub now_secs: u64,
}

/// Fetch the attestation from `stack_url` (binding `nonce` as `report_data`),
/// fetch its collateral, and verify fail-closed. Returns the verified quote body
/// on success.
///
/// `nonce` is the freshness / anti-replay value the verifier chooses; it is bound
/// into REPORTDATA on the signer side and recomputed here, so a recorded quote
/// from a previous challenge won't verify.
pub async fn verify_signer_attestation(
    stack_url: String,
    nonce: Vec<u8>,
    params: LiveVerifyParams,
) -> Result<VerifiedQuote, VerifyError> {
    // 1. Fetch the attestation, binding our nonce as report_data.
    let response = crate::commands::config::get_attestation(stack_url, Some(nonce.clone()))
        .await
        .map_err(|e| VerifyError::Transport(format!("{e:?}")))?;
    let report = response.report.ok_or_else(|| {
        VerifyError::Transport("stack returned no attestation report".to_string())
    })?;
    let raw_quote = report.raw_quote;
    if raw_quote.is_empty() {
        return Err(VerifyError::EmptyQuote);
    }

    // 2. Fetch the DCAP collateral for this quote (TCB info / QE identity / CRLs).
    let collateral = fetch_collateral(&params.pccs_url, &raw_quote).await?;

    // 3. Verify, fail-closed: DCAP + TCB -> measurement policy -> REPORTDATA.
    let verifier = DcapQuoteVerifier::new(collateral, params.now_secs)
        .accept_tcb_statuses(params.accepted_tcb);
    let expected = ExpectedReportData {
        pubkeys: params.expected_pubkeys,
        image_digests: params.expected_image_digests,
        report_data: nonce,
    };
    verify_attestation(&raw_quote, &verifier, &params.policy, &expected)
}
