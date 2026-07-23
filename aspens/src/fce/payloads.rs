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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMyStateResponse {
    #[serde(rename = "openOrders")]
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
    pub bids: Vec<BookLevel>,
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
