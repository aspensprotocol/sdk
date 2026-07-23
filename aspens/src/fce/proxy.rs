//! FCE extension-proxy client — `POST /direct` + poll `/action/result/{id}`
//! (design §4/§5), pinned against `extension/tools/cmd/send-direct`.
//!
//! Requires a tokio runtime at call time (reqwest async), same as
//! `tdx_verify::collateral`. The `fce` feature pulls only `reqwest`; the caller
//! provides the runtime.

use std::time::Duration;

use eyre::{Result, WrapErr, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::payloads::*;
use super::result::{ActionResponse, ActionResult};
use super::wire::{
    self, DirectInstruction, OP_CANCEL_ORDER, OP_EXPORT_HISTORY, OP_GET_BOOK_STATE,
    OP_GET_MY_STATE, OP_PLACE_ORDER, OP_WITHDRAW,
};

/// Default result-poll schedule (mirrors `send-direct`: 15 attempts, 2s apart).
const POLL_ATTEMPTS: u32 = 15;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A client for the FCE direct-action channel. Point `proxy_url` at the
/// ext-proxy **external** endpoint (host `:6674`, i.e. `EXT_PROXY_URL`).
#[derive(Debug, Clone)]
pub struct FceClient {
    proxy_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
    poll_attempts: u32,
    poll_interval: Duration,
}

impl FceClient {
    /// Build a client. `api_key` is the proxy's `DIRECT_API_KEY` (sent as
    /// `X-API-Key`); pass `None` only against a proxy with auth disabled.
    pub fn new(proxy_url: impl Into<String>, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .wrap_err("building FCE proxy http client")?;
        Ok(Self {
            proxy_url: proxy_url.into().trim_end_matches('/').to_string(),
            api_key,
            http,
            poll_attempts: POLL_ATTEMPTS,
            poll_interval: POLL_INTERVAL,
        })
    }

    /// Override the result-poll schedule (default 15 × 2s).
    pub fn with_polling(mut self, attempts: u32, interval: Duration) -> Self {
        self.poll_attempts = attempts;
        self.poll_interval = interval;
        self
    }

    // ---- typed direct actions ----

    pub async fn place_order(
        &self,
        req: &PlaceOrderRequest,
    ) -> Result<Outcome<PlaceOrderResponse>> {
        self.action(OP_PLACE_ORDER, req).await
    }

    pub async fn cancel_order(
        &self,
        req: &CancelOrderRequest,
    ) -> Result<Outcome<CancelOrderResponse>> {
        self.action(OP_CANCEL_ORDER, req).await
    }

    /// WITHDRAW returns a MidribV3 `WithdrawVoucher` on success.
    pub async fn withdraw(&self, req: &WithdrawRequest) -> Result<Outcome<WithdrawVoucher>> {
        self.action(OP_WITHDRAW, req).await
    }

    /// Point-in-time snapshot (not a live stream).
    pub async fn get_my_state(
        &self,
        req: &GetMyStateRequest,
    ) -> Result<Outcome<GetMyStateResponse>> {
        self.action(OP_GET_MY_STATE, req).await
    }

    /// Point-in-time snapshot (not a live stream).
    pub async fn get_book_state(
        &self,
        req: &GetBookStateRequest,
    ) -> Result<Outcome<GetBookStateResponse>> {
        self.action(OP_GET_BOOK_STATE, req).await
    }

    /// Point-in-time snapshot (not a live stream).
    pub async fn export_history(
        &self,
        req: &ExportHistoryRequest,
    ) -> Result<Outcome<ExportHistoryResponse>> {
        self.action(OP_EXPORT_HISTORY, req).await
    }

    // ---- generic action = submit + poll + decode ----

    /// Serialize `req` to the payload JSON, submit as `command`, poll the
    /// result, and decode `data` (on success) into `Resp`.
    pub async fn action<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        command: &str,
        req: &Req,
    ) -> Result<Outcome<Resp>> {
        let payload = serde_json::to_vec(req).wrap_err("serializing action payload")?;
        let id = self.submit_direct(command, payload).await?;
        let result = self.poll_result(&id).await?;
        let data = if result.ok() && !result.data_bytes().is_empty() {
            Some(
                result
                    .decode::<Resp>()
                    .wrap_err("decoding action result data")?,
            )
        } else {
            None
        };
        Ok(Outcome {
            status: result.status,
            log: result.log,
            data,
        })
    }

    /// `POST /direct` a `DirectInstruction`; returns the 32-byte action id.
    pub async fn submit_direct(&self, command: &str, payload_json: Vec<u8>) -> Result<[u8; 32]> {
        let di = DirectInstruction::new(command, payload_json);
        let body = serde_json::to_vec(&di).wrap_err("serializing DirectInstruction")?;

        let mut rb = self
            .http
            .post(format!("{}/direct", self.proxy_url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(k) = &self.api_key {
            rb = rb.header("X-API-Key", k);
        }
        let resp = rb.send().await.wrap_err("POST /direct")?;
        let status = resp.status();
        let bytes = resp.bytes().await.wrap_err("reading /direct response")?;
        if !status.is_success() {
            bail!(
                "POST /direct -> {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
        }

        #[derive(serde::Deserialize)]
        struct DirectResp {
            data: DirectRespData,
        }
        #[derive(serde::Deserialize)]
        struct DirectRespData {
            #[serde(with = "wire::hex32")]
            id: [u8; 32],
        }
        let dr: DirectResp =
            serde_json::from_slice(&bytes).wrap_err("decoding /direct response")?;
        Ok(dr.data.id)
    }

    /// Poll `GET /action/result/{id}?submissionTag=submit` until a 200.
    pub async fn poll_result(&self, id: &[u8; 32]) -> Result<ActionResult> {
        let url = format!(
            "{}/action/result/0x{}?submissionTag=submit",
            self.proxy_url,
            hex::encode(id)
        );
        for _ in 0..self.poll_attempts {
            let resp = self
                .http
                .get(&url)
                .send()
                .await
                .wrap_err("GET /action/result")?;
            if resp.status().is_success() {
                let bytes = resp.bytes().await.wrap_err("reading result")?;
                let ar: ActionResponse =
                    serde_json::from_slice(&bytes).wrap_err("decoding ActionResponse")?;
                return Ok(ar.result);
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        bail!(
            "no result after {} polls (action 0x{})",
            self.poll_attempts,
            hex::encode(id)
        )
    }
}

/// The decoded outcome of a direct action: the raw `status`/`log` plus the
/// typed `data` (present only when `status == 1` and the action returns data).
#[derive(Debug, Clone)]
pub struct Outcome<T> {
    /// 1 = ok, 0 = error.
    pub status: u8,
    /// "ok" or "error: <msg>".
    pub log: String,
    pub data: Option<T>,
}

impl<T> Outcome<T> {
    pub fn ok(&self) -> bool {
        self.status == 1
    }

    /// The typed data, or an error carrying `log` when the action failed / had none.
    pub fn into_data(self) -> Result<T> {
        match self.data {
            Some(d) => Ok(d),
            None => bail!(
                "action failed or returned no data: status={} log={}",
                self.status,
                self.log
            ),
        }
    }
}
