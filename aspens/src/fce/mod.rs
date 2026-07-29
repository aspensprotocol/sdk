//! FCE direct-action transport (feature `fce`).
//!
//! Drives actions through the Flare Confidential Extension proxy
//! (`POST /direct` + poll `/action/result/{id}`) instead of dialing arborter
//! gRPC directly. The wire format is pinned against Flare's `tee-node@v0.0.22`
//! and Aspens' `send-direct` client — see `sdk/docs/fce-transport-design.md`.
//!
//! This is transport + envelope codec only: order/withdraw **signing** stays in
//! [`crate::orders`] / [`crate::wallet`] and is unchanged (arborter authenticates
//! the same envelope regardless of transport). FCE trading carries no JWT.
//!
//! Reads (`get_book_state` / `get_my_state` / `export_history`) are **one-shot
//! snapshots**, not live streams — the adapter drains the arborter stream into a
//! point-in-time result. Config discovery still uses gRPC (no `GET_CONFIG`
//! command), so an FCE-enabled client is hybrid.
//!
//! ```no_run
//! # async fn demo() -> eyre::Result<()> {
//! use aspens::fce::{FceClient, PlaceOrderRequest};
//! let client = FceClient::new("https://ext-proxy.example", Some("api-key".into()))?;
//! // build `req` from the SDK's signed order (signatureHash + orderId unchanged)
//! # let req: PlaceOrderRequest = unimplemented!();
//! let outcome = client.place_order(&req).await?;
//! if outcome.ok() { let resp = outcome.into_data()?; let _ = resp.order_id; }
//! # Ok(()) }
//! ```

pub mod payloads;
pub mod proxy;
pub mod result;
pub mod wire;

pub use payloads::{
    BookLevel, CancelOrderRequest, CancelOrderResponse, ExportHistoryRequest,
    ExportHistoryResponse, GetBookStateRequest, GetBookStateResponse, GetMyStateRequest,
    GetMyStateResponse, OpenOrder, PlaceOrderRequest, PlaceOrderResponse, TradeRecord,
    WithdrawRequest, WithdrawVoucher,
};
pub use proxy::{FceClient, Outcome};
pub use result::{ActionResponse, ActionResult};
pub use wire::{
    DirectInstruction, OP_CANCEL_ORDER, OP_EXPORT_HISTORY, OP_GET_BOOK_STATE, OP_GET_MY_STATE,
    OP_PLACE_ORDER, OP_TYPE_ASPENS, OP_WITHDRAW, to_bytes32,
};
