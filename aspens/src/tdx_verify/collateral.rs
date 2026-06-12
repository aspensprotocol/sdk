//! DCAP collateral fetching and loading. Behind the `dcap-fetch` feature.
//!
//! Verifying a TD Quote needs collateral the quote doesn't carry: the TCB info,
//! the QE identity, and the PCK CRL (keyed by the quote's FMSPC). dcap-qvl ships a
//! fetcher, but it's locked behind its `report` feature, which forces reqwest 0.13
//! + `hickory-dns` (an unresolvable prerelease `hickory-resolver`). So we drive the
//! PCS/PCCS v4 REST API ourselves over the SDK's own rustls reqwest 0.12 and
//! assemble a [`QuoteCollateralV3`] directly — reusing dcap-qvl's *un*-gated cert
//! parsing ([`ParsedCert`]) for the PCK extension + CA classification, and a small
//! FMSPC extension walk mirrored from dcap-qvl's `utils::find_extension`.
//!
//! For air-gapped / offline use, [`collateral_from_json`] loads operator-provided
//! collateral instead of fetching.
//!
//! ## Scope
//! - Supports quotes whose certification data embeds the PCK cert chain
//!   (cert-data **type 5** — what configfs-tsm / our signer produce). Encrypted-PPID
//!   quotes (type 2/3) need a platform-registration PCK fetch; use `--collateral`.
//! - Root-CA-CRL fetch is implemented for a **PCCS** (the standard deployment, e.g.
//!   Phala's). Pointing directly at Intel PCS for the root CRL is unsupported; use a
//!   PCCS or `--collateral`.

use super::VerifyError;
use dcap_qvl::QuoteCollateralV3;
use dcap_qvl::config::{Config, ParsedCert, PckCa, X509Codec};
use dcap_qvl::configs::DefaultConfig;
use dcap_qvl::oids;
use dcap_qvl::quote::Quote;

/// Intel Provisioning Certification Service (the authoritative source). Direct use
/// is rate-limited and needs a subscription for some endpoints — prefer a PCCS.
pub const INTEL_PCS_URL: &str = "https://api.trustedservices.intel.com";
/// Phala's public PCCS mirror — a convenient default for fetching collateral.
pub const PHALA_PCCS_URL: &str = "https://pccs.phala.network";

/// DCAP cert-data type for an embedded PCK certificate chain (PEM). Other types
/// (2/3, encrypted PPID) require a platform-registration fetch we don't do here.
const CERT_TYPE_PCK_CHAIN: u16 = 5;

fn collateral_err(msg: impl core::fmt::Display) -> VerifyError {
    VerifyError::Collateral(msg.to_string())
}

// --- PCS/PCCS endpoint URLs (mirrors dcap-qvl's `collateral::PcsEndpoints`) ------

struct Endpoints {
    base: String,
    tee: &'static str,
    fmspc: String,
    ca: String,
}

impl Endpoints {
    fn new(base_url: &str, for_sgx: bool, fmspc: String, ca: &str) -> Self {
        let tee = if for_sgx { "sgx" } else { "tdx" };
        let base = base_url
            .trim_end_matches('/')
            .trim_end_matches("/sgx/certification/v4")
            .trim_end_matches("/tdx/certification/v4")
            .to_owned();
        Self {
            base,
            tee,
            fmspc,
            ca: ca.to_owned(),
        }
    }

    fn is_pcs(&self) -> bool {
        self.base.starts_with(INTEL_PCS_URL)
    }

    fn mk(&self, tee: &str, path: &str) -> String {
        format!("{}/{}/certification/v4/{}", self.base, tee, path)
    }

    fn pckcrl(&self) -> String {
        self.mk("sgx", &format!("pckcrl?ca={}&encoding=der", self.ca))
    }

    fn rootcacrl(&self) -> String {
        self.mk("sgx", "rootcacrl")
    }

    fn tcb(&self) -> String {
        self.mk(self.tee, &format!("tcb?fmspc={}", self.fmspc))
    }

    fn qe_identity(&self) -> String {
        self.mk(self.tee, "qe/identity?update=standard")
    }
}

// --- FMSPC + CA extraction ------------------------------------------------------

/// Read the FMSPC (hex) and CA type (`"processor"` / `"platform"`) from the PCK
/// leaf certificate. Uses dcap-qvl's [`ParsedCert`] for the cert parse + CA
/// classification and the Intel SGX extension lookup; the FMSPC sub-field is then
/// pulled with [`find_extension`] (mirrors `dcap_qvl::utils::get_fmspc`).
fn extract_fmspc_and_ca(pck_pem: &str) -> Result<(String, &'static str), VerifyError> {
    let pems = pem::parse_many(pck_pem.as_bytes())
        .map_err(|e| collateral_err(format!("parsing PCK PEM chain: {e}")))?;
    let leaf = pems
        .first()
        .ok_or_else(|| collateral_err("PCK certificate chain is empty"))?;

    let parsed = <DefaultConfig as Config>::X509::from_der(leaf.contents())
        .map_err(|e| collateral_err(format!("decoding PCK leaf certificate: {e}")))?;

    let sgx_ext = parsed
        .extension(oids::SGX_EXTENSION.as_bytes())
        .map_err(|e| collateral_err(format!("reading Intel SGX extension: {e}")))?
        .ok_or_else(|| collateral_err("Intel SGX extension not found in PCK certificate"))?;

    let fmspc = find_extension(&[oids::FMSPC.as_bytes()], &sgx_ext)?;
    if fmspc.len() != 6 {
        return Err(collateral_err(format!(
            "FMSPC length {} (expected 6)",
            fmspc.len()
        )));
    }

    let ca = parsed.pck_ca().unwrap_or(PckCa::Processor).as_id_str();

    Ok((hex::encode_upper(&fmspc), ca))
}

/// Walk a DER `SEQUENCE OF SEQUENCE { OID, value }` along `path` and return the
/// final value's raw bytes. Faithful mirror of `dcap_qvl::utils::find_extension`
/// (kept byte-compatible so FMSPC extraction matches the upstream verifier).
fn find_extension(path: &[&[u8]], raw: &[u8]) -> Result<Vec<u8>, VerifyError> {
    use asn1_der::DerObject;
    use asn1_der::typed::{DerDecodable, Sequence};

    fn der<T>(r: Result<T, asn1_der::Asn1DerError>) -> Result<T, VerifyError> {
        r.map_err(|e| collateral_err(format!("DER decode: {e}")))
    }

    let mut obj = der(DerObject::decode(raw))?;
    for oid in path {
        let seq = der(Sequence::load(obj))?;
        let mut next = None;
        for i in 0..seq.len() {
            let entry = der(Sequence::load(der(seq.get(i))?))?;
            let name = der(entry.get(0))?;
            let value = der(entry.get(1))?;
            if name.value() == *oid {
                next = Some(value);
                break;
            }
        }
        obj = next.ok_or_else(|| collateral_err("OID not found in Intel SGX extension"))?;
    }
    Ok(obj.value().to_vec())
}

// --- HTTP plumbing --------------------------------------------------------------

async fn http_get(
    client: &reqwest::Client,
    url: &str,
) -> Result<(Vec<u8>, reqwest::header::HeaderMap), VerifyError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| VerifyError::Transport(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(collateral_err(format!("GET {url}: HTTP {}", resp.status())));
    }
    let headers = resp.headers().clone();
    let body = resp
        .bytes()
        .await
        .map_err(|e| VerifyError::Transport(format!("reading {url}: {e}")))?
        .to_vec();
    Ok((body, headers))
}

/// Read a header value and URL-decode it (PCS returns issuer chains percent-encoded).
fn header(headers: &reqwest::header::HeaderMap, name: &str) -> Result<String, VerifyError> {
    let value = headers
        .get(name)
        .ok_or_else(|| collateral_err(format!("response missing header {name}")))?
        .to_str()
        .map_err(|e| collateral_err(format!("header {name} not valid text: {e}")))?;
    Ok(urlencoding::decode(value)
        .map_err(|e| collateral_err(format!("decoding header {name}: {e}")))?
        .into_owned())
}

#[derive(serde::Deserialize)]
struct TcbInfoResponse {
    #[serde(rename = "tcbInfo")]
    tcb_info: serde_json::Value,
    signature: String,
}

#[derive(serde::Deserialize)]
struct QeIdentityResponse {
    #[serde(rename = "enclaveIdentity")]
    enclave_identity: serde_json::Value,
    signature: String,
}

// --- Public API -----------------------------------------------------------------

/// Fetch the DCAP collateral for `raw_quote` from the given PCCS base URL (e.g.
/// [`PHALA_PCCS_URL`] or an operator-run PCCS). Reads the FMSPC + PCK chain from
/// the quote, then fetches the PCK CRL, TCB info, QE identity, and root CA CRL.
pub async fn fetch_collateral(
    pccs_url: &str,
    raw_quote: &[u8],
) -> Result<QuoteCollateralV3, VerifyError> {
    let quote =
        Quote::parse(raw_quote).map_err(|e| collateral_err(format!("parsing quote: {e}")))?;

    // We only handle the embedded PCK-chain case (cert-data type 5). Encrypted-PPID
    // quotes need a platform-registration fetch we deliberately don't implement.
    let cert_type = quote.inner_cert_type();
    if cert_type != CERT_TYPE_PCK_CHAIN {
        return Err(collateral_err(format!(
            "quote certification-data type {cert_type} (not an embedded PCK chain) is not \
             supported in-process; supply collateral with --collateral"
        )));
    }
    let pck_chain = String::from_utf8_lossy(quote.inner_cert_data()).into_owned();
    let (fmspc, ca) = extract_fmspc_and_ca(&pck_chain)?;

    let endpoints = Endpoints::new(pccs_url, quote.header.is_sgx(), fmspc, ca);
    if endpoints.is_pcs() {
        return Err(collateral_err(
            "fetching the root CA CRL directly from Intel PCS is unsupported; point --pccs-url at \
             a PCCS (e.g. https://pccs.phala.network) or supply --collateral",
        ));
    }
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| collateral_err(format!("building HTTP client: {e}")))?;

    // PCK CRL (DER) + its issuer chain.
    let (pck_crl, pckcrl_headers) = http_get(&client, &endpoints.pckcrl()).await?;
    let pck_crl_issuer_chain = header(&pckcrl_headers, "SGX-PCK-CRL-Issuer-Chain")?;

    // TCB info (the issuer-chain header name differs between PCS and some PCCS).
    let (tcb_body, tcb_headers) = http_get(&client, &endpoints.tcb()).await?;
    let tcb_info_issuer_chain = header(&tcb_headers, "SGX-TCB-Info-Issuer-Chain")
        .or_else(|_| header(&tcb_headers, "TCB-Info-Issuer-Chain"))?;
    let raw_tcb_info = String::from_utf8(tcb_body)
        .map_err(|e| collateral_err(format!("TCB info not UTF-8: {e}")))?;

    // QE identity.
    let (qe_body, qe_headers) = http_get(&client, &endpoints.qe_identity()).await?;
    let qe_identity_issuer_chain = header(&qe_headers, "SGX-Enclave-Identity-Issuer-Chain")?;
    let raw_qe_identity = String::from_utf8(qe_body)
        .map_err(|e| collateral_err(format!("QE identity not UTF-8: {e}")))?;

    // Root CA CRL — PCCS serves it hex-encoded at /sgx/certification/v4/rootcacrl.
    let (rootcrl_body, _) = http_get(&client, &endpoints.rootcacrl()).await?;
    let rootcrl_hex = core::str::from_utf8(&rootcrl_body)
        .map_err(|e| collateral_err(format!("root CA CRL not UTF-8 hex: {e}")))?;
    let root_ca_crl = hex::decode(rootcrl_hex.trim())
        .map_err(|e| collateral_err(format!("decoding root CA CRL hex: {e}")))?;

    // Unwrap the JSON envelopes into the (body, signature) pair verify() expects.
    let tcb: TcbInfoResponse = serde_json::from_str(&raw_tcb_info)
        .map_err(|e| collateral_err(format!("TCB info JSON: {e}")))?;
    let tcb_info_signature = hex::decode(&tcb.signature)
        .map_err(|e| collateral_err(format!("TCB info signature hex: {e}")))?;

    let qe: QeIdentityResponse = serde_json::from_str(&raw_qe_identity)
        .map_err(|e| collateral_err(format!("QE identity JSON: {e}")))?;
    let qe_identity_signature = hex::decode(&qe.signature)
        .map_err(|e| collateral_err(format!("QE identity signature hex: {e}")))?;

    Ok(QuoteCollateralV3 {
        pck_crl_issuer_chain,
        root_ca_crl,
        pck_crl,
        tcb_info_issuer_chain,
        tcb_info: tcb.tcb_info.to_string(),
        tcb_info_signature,
        qe_identity_issuer_chain,
        qe_identity: qe.enclave_identity.to_string(),
        qe_identity_signature,
        pck_certificate_chain: Some(pck_chain),
    })
}

/// Load operator-provided collateral from JSON (the [`QuoteCollateralV3`] shape) —
/// for air-gapped / offline verification where collateral is supplied out of band
/// rather than fetched. The byte fields use `serde_bytes`, so round-tripping
/// through `serde_json` on a value produced by this crate works as-is.
pub fn collateral_from_json(json: &str) -> Result<QuoteCollateralV3, VerifyError> {
    serde_json::from_str(json).map_err(|e| collateral_err(format!("invalid collateral JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // PCK certificate chain (processor CA) from dcap-qvl's own test corpus; its
    // FMSPC is the known value 00A067110000. This is the cross-check that our
    // ParsedCert + asn1_der FMSPC walk matches the upstream verifier byte-for-byte.
    const TEST_PCK_CHAIN_PROCESSOR: &str = "-----BEGIN CERTIFICATE-----\n\
MIIEjTCCBDSgAwIBAgIVAIG3dzK3YemOubljpKvR5bm/XdjWMAoGCCqGSM49BAMC\n\
MHExIzAhBgNVBAMMGkludGVsIFNHWCBQQ0sgUHJvY2Vzc29yIENBMRowGAYDVQQK\n\
DBFJbnRlbCBDb3Jwb3JhdGlvbjEUMBIGA1UEBwwLU2FudGEgQ2xhcmExCzAJBgNV\n\
BAgMAkNBMQswCQYDVQQGEwJVUzAeFw0yMzA5MjAyMTUzNDNaFw0zMDA5MjAyMTUz\n\
NDNaMHAxIjAgBgNVBAMMGUludGVsIFNHWCBQQ0sgQ2VydGlmaWNhdGUxGjAYBgNV\n\
BAoMEUludGVsIENvcnBvcmF0aW9uMRQwEgYDVQQHDAtTYW50YSBDbGFyYTELMAkG\n\
A1UECAwCQ0ExCzAJBgNVBAYTAlVTMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n\
kgmE7N3D+RspyaCZ2YoDTLDCuh5pnvAu4crPn2uAGujq9tOgwU8/y7jttShCB603\n\
U6r+h9ayOk2nZ9jewk25lqOCAqgwggKkMB8GA1UdIwQYMBaAFNDoqtp11/kuSReY\n\
PHsUZdDV8llNMGwGA1UdHwRlMGMwYaBfoF2GW2h0dHBzOi8vYXBpLnRydXN0ZWRz\n\
ZXJ2aWNlcy5pbnRlbC5jb20vc2d4L2NlcnRpZmljYXRpb24vdjQvcGNrY3JsP2Nh\n\
PXByb2Nlc3NvciZlbmNvZGluZz1kZXIwHQYDVR0OBBYEFIW4KX263PRxYJah2Cfj\n\
AlrcvAC9MA4GA1UdDwEB/wQEAwIGwDAMBgNVHRMBAf8EAjAAMIIB1AYJKoZIhvhN\n\
AQ0BBIIBxTCCAcEwHgYKKoZIhvhNAQ0BAQQQ0E7AbU5tktyQ0K089e4t3zCCAWQG\n\
CiqGSIb4TQENAQIwggFUMBAGCyqGSIb4TQENAQIBAgELMBAGCyqGSIb4TQENAQIC\n\
AgELMBAGCyqGSIb4TQENAQIDAgECMBAGCyqGSIb4TQENAQIEAgECMBEGCyqGSIb4\n\
TQENAQIFAgIA/zAQBgsqhkiG+E0BDQECBgIBATAQBgsqhkiG+E0BDQECBwIBADAQ\n\
BgsqhkiG+E0BDQECCAIBADAQBgsqhkiG+E0BDQECCQIBADAQBgsqhkiG+E0BDQEC\n\
CgIBADAQBgsqhkiG+E0BDQECCwIBADAQBgsqhkiG+E0BDQECDAIBADAQBgsqhkiG\n\
+E0BDQECDQIBADAQBgsqhkiG+E0BDQECDgIBADAQBgsqhkiG+E0BDQECDwIBADAQ\n\
BgsqhkiG+E0BDQECEAIBADAQBgsqhkiG+E0BDQECEQIBDTAfBgsqhkiG+E0BDQEC\n\
EgQQCwsCAv8BAAAAAAAAAAAAADAQBgoqhkiG+E0BDQEDBAIAADAUBgoqhkiG+E0B\n\
DQEEBAYAoGcRAAAwDwYKKoZIhvhNAQ0BBQoBADAKBggqhkjOPQQDAgNHADBEAiBm\n\
SMZEtlQEjnZgGa192W3ArnZ3iyY6ckM/sTsXxCRmJgIgLf20tZHNw3a1b31JDSOW\n\
E6wesxoAmTeqJGRqZl621qI=\n\
-----END CERTIFICATE-----\n\
-----BEGIN CERTIFICATE-----\n\
MIICmDCCAj6gAwIBAgIVANDoqtp11/kuSReYPHsUZdDV8llNMAoGCCqGSM49BAMC\n\
MGgxGjAYBgNVBAMMEUludGVsIFNHWCBSb290IENBMRowGAYDVQQKDBFJbnRlbCBD\n\
b3Jwb3JhdGlvbjEUMBIGA1UEBwwLU2FudGEgQ2xhcmExCzAJBgNVBAgMAkNBMQsw\n\
CQYDVQQGEwJVUzAeFw0xODA1MjExMDUwMTBaFw0zMzA1MjExMDUwMTBaMHExIzAh\n\
BgNVBAMMGkludGVsIFNHWCBQQ0sgUHJvY2Vzc29yIENBMRowGAYDVQQKDBFJbnRl\n\
bCBDb3Jwb3JhdGlvbjEUMBIGA1UEBwwLU2FudGEgQ2xhcmExCzAJBgNVBAgMAkNB\n\
MQswCQYDVQQGEwJVUzBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABL9q+NMp2IOg\n\
tdl1bk/uWZ5+TGQm8aCi8z78fs+fKCQ3d+uDzXnVTAT2ZhDCifyIuJwvN3wNBp9i\n\
HBSSMJMJrBOjgbswgbgwHwYDVR0jBBgwFoAUImUM1lqdNInzg7SVUr9QGzknBqww\n\
UgYDVR0fBEswSTBHoEWgQ4ZBaHR0cHM6Ly9jZXJ0aWZpY2F0ZXMudHJ1c3RlZHNl\n\
cnZpY2VzLmludGVsLmNvbS9JbnRlbFNHWFJvb3RDQS5kZXIwHQYDVR0OBBYEFNDo\n\
qtp11/kuSReYPHsUZdDV8llNMA4GA1UdDwEB/wQEAwIBBjASBgNVHRMBAf8ECDAG\n\
AQH/AgEAMAoGCCqGSM49BAMCA0gAMEUCIQCJgTbtVqOyZ1m3jqiAXM6QYa6r5sWS\n\
4y/G7y8uIJGxdwIgRqPvBSKzzQagBLQq5s5A70pdoiaRJ8z/0uDz4NgV91k=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn fmspc_and_ca_match_known_vector() {
        let (fmspc, ca) = extract_fmspc_and_ca(TEST_PCK_CHAIN_PROCESSOR).unwrap();
        assert_eq!(fmspc, "00A067110000");
        assert_eq!(ca, "processor");
    }

    #[test]
    fn endpoint_urls_pccs() {
        let ep = Endpoints::new(
            "https://pccs.example.com/",
            false,
            "B0C06F000000".to_string(),
            "processor",
        );
        assert!(!ep.is_pcs());
        assert_eq!(
            ep.pckcrl(),
            "https://pccs.example.com/sgx/certification/v4/pckcrl?ca=processor&encoding=der"
        );
        assert_eq!(
            ep.tcb(),
            "https://pccs.example.com/tdx/certification/v4/tcb?fmspc=B0C06F000000"
        );
        assert_eq!(
            ep.qe_identity(),
            "https://pccs.example.com/tdx/certification/v4/qe/identity?update=standard"
        );
        assert_eq!(
            ep.rootcacrl(),
            "https://pccs.example.com/sgx/certification/v4/rootcacrl"
        );
    }

    #[test]
    fn endpoint_is_pcs_detects_intel() {
        let ep = Endpoints::new(INTEL_PCS_URL, false, "B0C06F000000".to_string(), "platform");
        assert!(ep.is_pcs());
        // base-url normalization strips a trailing certification path
        let ep2 = Endpoints::new(
            "https://pccs.example.com/tdx/certification/v4",
            false,
            "ABCDEF000000".to_string(),
            "platform",
        );
        assert_eq!(ep2.base, "https://pccs.example.com");
    }

    #[test]
    fn collateral_json_round_trips() {
        let c = QuoteCollateralV3 {
            pck_crl_issuer_chain: "pck-issuer".to_string(),
            root_ca_crl: vec![1, 2, 3],
            pck_crl: vec![4, 5, 6],
            tcb_info_issuer_chain: "tcb-issuer".to_string(),
            tcb_info: "{\"x\":1}".to_string(),
            tcb_info_signature: vec![7, 8],
            qe_identity_issuer_chain: "qe-issuer".to_string(),
            qe_identity: "{\"y\":2}".to_string(),
            qe_identity_signature: vec![9, 10],
            pck_certificate_chain: Some("pem".to_string()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back = collateral_from_json(&json).unwrap();
        assert_eq!(back.pck_crl_issuer_chain, c.pck_crl_issuer_chain);
        assert_eq!(back.root_ca_crl, c.root_ca_crl);
        assert_eq!(back.tcb_info, c.tcb_info);
        assert_eq!(back.qe_identity_signature, c.qe_identity_signature);
        assert_eq!(back.pck_certificate_chain, c.pck_certificate_chain);
    }
}
