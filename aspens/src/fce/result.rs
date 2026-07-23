//! FCE action results (design §5), pinned against `tee-node@v0.0.22`
//! `pkg/types/actions.go`.
//!
//! `ActionResult.Data` is go-ethereum `hexutil.Bytes` — i.e. `0x`-hex, NOT
//! base64. Its decoded bytes are `json.Marshal(<arborter response>)`, so
//! decoding is: hex → bytes → JSON.

use serde::{Deserialize, Serialize};

use super::wire::{hex32, hexbytes};

/// Poll response wrapper: `GET /action/result/{id}` returns this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResponse {
    pub result: ActionResult,
    #[serde(with = "hexbytes", default)]
    pub signature: Vec<u8>,
    #[serde(rename = "proxySignature", with = "hexbytes", default)]
    pub proxy_signature: Vec<u8>,
}

/// The extension's result for one action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    #[serde(with = "hex32")]
    pub id: [u8; 32],
    #[serde(rename = "submissionTag")]
    pub submission_tag: String,
    /// 1 = ok, 0 = error.
    pub status: u8,
    pub log: String,
    #[serde(rename = "opType", with = "hex32")]
    pub op_type: [u8; 32],
    #[serde(rename = "opCommand", with = "hex32")]
    pub op_command: [u8; 32],
    pub version: String,
    /// `0x`-hex of `json.Marshal(<arborter response>)` (present on status=1).
    #[serde(with = "hexbytes", default)]
    pub data: Vec<u8>,
}

impl ActionResult {
    /// True when the action succeeded (`status == 1`).
    pub fn ok(&self) -> bool {
        self.status == 1
    }

    /// Decode `data` (already hex-decoded to raw bytes) as a typed response.
    /// Errors if `data` is empty or not the expected JSON.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> eyre::Result<T> {
        if self.data.is_empty() {
            eyre::bail!(
                "action result has no data (status={}, log={})",
                self.status,
                self.log
            );
        }
        Ok(serde_json::from_slice(&self.data)?)
    }

    /// `data` as raw JSON bytes (the `json.Marshal` of the arborter response).
    pub fn data_bytes(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A result envelope as the proxy returns it — data is 0x-hex of the JSON.
    #[test]
    fn decodes_hex_data_to_json() {
        // json = {"orderId":7,"orderInBook":true,"fills":0}
        let json = br#"{"orderId":7,"orderInBook":true,"fills":0}"#;
        let envelope = serde_json::json!({
            "result": {
                "id": "0x".to_string() + &"00".repeat(32),
                "submissionTag": "submit",
                "status": 1u8,
                "log": "ok",
                "opType": "0x".to_string() + &"00".repeat(32),
                "opCommand": "0x".to_string() + &"00".repeat(32),
                "version": "0.1.0",
                "data": format!("0x{}", hex::encode(json)),
            },
            "signature": "0x",
            "proxySignature": "0x",
        });
        let resp: ActionResponse = serde_json::from_value(envelope).unwrap();
        assert!(resp.result.ok());
        let po: super::super::payloads::PlaceOrderResponse = resp.result.decode().unwrap();
        assert_eq!(po.order_id, 7);
        assert!(po.order_in_book);
    }

    #[test]
    fn failed_status_surfaces_log() {
        let envelope = serde_json::json!({
            "result": {
                "id": "0x".to_string() + &"00".repeat(32),
                "submissionTag": "submit",
                "status": 0u8,
                "log": "error: unsupported direct command",
                "opType": "0x".to_string() + &"00".repeat(32),
                "opCommand": "0x".to_string() + &"00".repeat(32),
                "version": "0.1.0",
                "data": "0x",
            }
        });
        let resp: ActionResponse = serde_json::from_value(envelope).unwrap();
        assert!(!resp.result.ok());
        assert!(resp.result.decode::<serde_json::Value>().is_err());
    }
}
