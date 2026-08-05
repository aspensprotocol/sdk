//! Direct-action request/response payloads — the JSON that rides in
//! `DirectInstruction.message` (design §3), pinned field-for-field against the
//! adapter's `extension/pkg/types/types.go`.
//!
//! Field names are camelCase; **all amounts are u128 decimal strings** (never
//! numbers). The SDK's signing (`aspens::orders::derive_order_id`, the EIP-712
//! `signatureHash`) is unchanged — these structs just serialize what it already
//! produces. `signatureHash` is go-ethereum `hexutil.Bytes` → `0x`-hex.

use serde::{Deserialize, Serialize};

use super::wire::hexbytes;

// ---- PLACE_ORDER ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    /// "BID" | "ASK".
    pub side: String,
    /// u128 decimal.
    pub quantity: String,
    /// u128 decimal; `None` => MARKET order (omitted, matching Go `omitempty`).
    #[serde(rename = "price", skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(rename = "baseAccountAddress")]
    pub base_account_address: String,
    #[serde(rename = "quoteAccountAddress")]
    pub quote_account_address: String,
    /// "DIRECT" | "DISCRETIONARY"; omitted when `None`.
    #[serde(rename = "executionType", skip_serializing_if = "Option::is_none")]
    pub execution_type: Option<String>,
    /// Omitted when `None` (Go marshals `false` as omitted; set `Some(true)` to
    /// mark post-only).
    #[serde(rename = "postOnly", skip_serializing_if = "Option::is_none")]
    pub post_only: Option<bool>,
    /// EIP-712 signature of the order (0x-hex).
    #[serde(rename = "signatureHash", with = "hexbytes")]
    pub signature_hash: Vec<u8>,
    /// SDK-derived canonical order id (from `derive_order_id`).
    #[serde(rename = "orderId")]
    pub order_id: String,
    /// Committed lock, u128 decimal.
    #[serde(rename = "amountIn")]
    pub amount_in: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    #[serde(rename = "orderId")]
    pub order_id: u64,
    #[serde(rename = "orderInBook")]
    pub order_in_book: bool,
    pub fills: i64,
}

// ---- CANCEL_ORDER ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    #[serde(rename = "marketId")]
    pub market_id: String,
    /// "BID" | "ASK".
    pub side: String,
    #[serde(rename = "tokenAddress")]
    pub token_address: String,
    /// arborter-internal order id.
    #[serde(rename = "orderId")]
    pub order_id: u64,
    #[serde(rename = "signatureHash", with = "hexbytes")]
    pub signature_hash: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderResponse {
    pub canceled: bool,
}

// ---- WITHDRAW (direct action → MidribV3 voucher) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawRequest {
    /// network key, e.g. "flare-coston2".
    pub network: String,
    /// token contract address on `network`.
    pub token: String,
    /// withdrawer; the voucher pays THIS address.
    pub account: String,
    /// u128 decimal.
    pub amount: String,
    /// signature over `network|token|account|amount` (EIP-191 / Ed25519), 0x-hex.
    #[serde(with = "hexbytes")]
    pub signature: Vec<u8>,
}

/// The MidribV3 withdrawal voucher the WITHDRAW result carries — present to
/// `MidribV3.withdraw(voucher, signature)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawVoucher {
    pub account: String,
    pub token: String,
    pub amount: String,
    pub nonce: u64,
    pub expiry: u64,
    #[serde(with = "hexbytes")]
    pub signature: Vec<u8>,
}

// ---- Direct reads (one-shot snapshots; NOT live streams) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMyStateRequest {
    #[serde(rename = "marketId")]
    pub market_id: String,
    /// base or quote account address to filter by.
    pub trader: String,
}

/// Go marshals nil slices as JSON `null`; tolerate that as an empty vec
/// (first-live-run conformance fix, 2026-07-27).
fn null_as_empty<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let opt = Option::<Vec<T>>::deserialize(de)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMyStateResponse {
    #[serde(rename = "openOrders", default, deserialize_with = "null_as_empty")]
    pub open_orders: Vec<OpenOrder>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenOrder {
    #[serde(rename = "orderId")]
    pub order_id: u64,
    #[serde(rename = "marketId")]
    pub market_id: String,
    pub side: String,
    pub price: String,
    pub quantity: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBookStateRequest {
    #[serde(rename = "marketId")]
    pub market_id: String,
    /// cap per side (0 => default).
    pub depth: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBookStateResponse {
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub bids: Vec<BookLevel>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub quantity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportHistoryRequest {
    #[serde(rename = "marketId")]
    pub market_id: String,
    pub trader: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportHistoryResponse {
    #[serde(default, deserialize_with = "null_as_empty")]
    pub trades: Vec<TradeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub timestamp: u64,
    pub price: String,
    pub quantity: String,
    #[serde(rename = "orderHit")]
    pub order_hit: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_order_field_names_match_adapter() {
        let req = PlaceOrderRequest {
            side: "BID".into(),
            quantity: "5".into(),
            price: Some("1000".into()),
            market_id: "m".into(),
            base_account_address: "0xbase".into(),
            quote_account_address: "0xquote".into(),
            execution_type: None,
            post_only: None,
            signature_hash: vec![0xab, 0xcd],
            order_id: "0xoid".into(),
            amount_in: "5".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        // camelCase names the adapter's json.Unmarshal expects
        for k in [
            "side",
            "quantity",
            "price",
            "marketId",
            "baseAccountAddress",
            "quoteAccountAddress",
            "signatureHash",
            "orderId",
            "amountIn",
        ] {
            assert!(v.get(k).is_some(), "missing field {k}");
        }
        // optional fields omitted when None (matches Go omitempty)
        assert!(v.get("executionType").is_none());
        assert!(v.get("postOnly").is_none());
        // signatureHash is 0x-hex
        assert_eq!(v["signatureHash"], "0xabcd");
    }

    #[test]
    fn market_order_omits_price() {
        let req = PlaceOrderRequest {
            side: "ASK".into(),
            quantity: "1".into(),
            price: None,
            market_id: "m".into(),
            base_account_address: "0xb".into(),
            quote_account_address: "0xq".into(),
            execution_type: None,
            post_only: None,
            signature_hash: vec![],
            order_id: "0x".into(),
            amount_in: "1".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("price").is_none());
    }
}

/// GET_CONFIG takes no arguments — the arborter's `GetConfigRequest` is empty.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetConfigRequest {}

/// The arborter's `GetConfigResponse` as its canonical PROTOBUF encoding, hex
/// on the wire — not a JSON mirror of the config schema.
///
/// The adapter relays these bytes without reading them, so it needs no bindings
/// for the config proto and no rebuild when a field is added — which matters
/// because it ships inside the measured image, where any change costs a Flare
/// re-registration. Decoding happens here, against the same generated type the
/// gRPC path returns, so both transports yield an identical `GetConfigResponse`.
///
/// Hex rather than base64: every other value on this wire is `0x`-hex, and it
/// reuses the tested [`hexbytes`](super::wire) helper instead of adding a
/// dependency for one field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetConfigEnvelope {
    #[serde(rename = "configProto", with = "crate::fce::wire::hexbytes")]
    pub config_proto: Vec<u8>,
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::commands::config::config_pb::GetConfigResponse;
    use prost::Message as _;

    /// The envelope must decode to the same generated type the gRPC path
    /// returns. That is the whole point of the passthrough: one schema, decoded
    /// once, identical on both transports.
    #[test]
    fn envelope_round_trips_into_the_generated_config_type() {
        let original = GetConfigResponse::default();
        let hex_body = hex::encode(original.encode_to_vec());
        let json = format!(r#"{{"configProto":"0x{hex_body}"}}"#);

        let env: GetConfigEnvelope = serde_json::from_str(&json).expect("decode envelope");
        let decoded =
            GetConfigResponse::decode(env.config_proto.as_slice()).expect("decode protobuf");
        assert_eq!(decoded, original);
    }

    /// Arbitrary binary must survive intact — protobuf is full of non-UTF-8
    /// bytes, and anything that stringified them would corrupt the config.
    #[test]
    fn non_utf8_protobuf_bytes_survive_the_round_trip() {
        let raw = vec![0x0a, 0x03, 0xff, 0xfe, 0xfd, 0x12, 0x00];
        let json = format!(r#"{{"configProto":"0x{}"}}"#, hex::encode(&raw));
        let env: GetConfigEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env.config_proto, raw);
    }

    /// The field is `configProto` on the wire — the adapter's Go struct tag. A
    /// rename on either side must fail here, not at runtime against a proxy.
    #[test]
    fn envelope_field_is_camel_case_config_proto() {
        let env = GetConfigEnvelope {
            config_proto: vec![0x00],
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""configProto""#), "got {json}");
    }

    /// Malformed hex must be an error, never silently an empty config — an
    /// empty config makes `lookup_market` fail far from the actual cause.
    #[test]
    fn a_malformed_payload_is_an_error_not_an_empty_config() {
        let r: Result<GetConfigEnvelope, _> = serde_json::from_str(r#"{"configProto":"0xnothex"}"#);
        assert!(r.is_err(), "malformed hex must not decode");
    }
}
