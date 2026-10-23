//! Native Tron transactions from a user wallet, over the node's HTTP API.
//!
//! A Tron chain's configured RPC URL is the node's HTTP base
//! (`https://api.trongrid.io`). Reads go through `<base>/jsonrpc` with the
//! EVM code ([`jsonrpc_url`]); the calls Tron's JSON-RPC cannot make go to the
//! native `/wallet*` routes under the same base:
//!
//! * `/walletsolidity/getnowblock` — the solidified block a new transaction
//!   references (a head block can be reorged away, stranding the transaction
//!   until it expires),
//! * `/wallet/getnowblock` — the head, whose time the expiration counts from
//!   and against which an unseen transaction is judged expired,
//! * `/wallet/triggerconstantcontract` — the call simulated before it is
//!   signed,
//! * `/wallet/broadcasthex` — Tron has no `eth_sendRawTransaction`,
//! * `/wallet/gettransactioninfobyid` — the receipt, whose `receipt.result`
//!   says whether the call did anything.
//!
//! [`send_tron_tx`] is the one way the SDK sends a Tron transaction. The
//! transaction is encoded here from the caller's own contract and calldata and
//! signed once; a node-built transaction is never signed. Its `Ok` means the
//! head receipt says `SUCCESS`. That is enough for a user's own transaction:
//! the venue credits a deposit only once the chain finalizes it, so a reorged
//! deposit is simply never credited.
//!
//! # API key
//!
//! TronGrid authenticates with a `TRON-PRO-API-KEY` header, never a query
//! parameter. [`TronHttp::with_api_key`] puts that header on every request to
//! the node; [`TronHttp::for_network`] takes the key from
//! `ASPENS_TRON_API_KEY_<NETWORK>` ([`api_key_env_key`]), beside the
//! endpoint's own `ASPENS_RPC_URL_<NETWORK>`, so each network's endpoint
//! carries its own key. With no key set, no header is sent and the node
//! serves the request at its keyless rate limit. The EVM reads under
//! `/jsonrpc` (the chain-id probe and balance reads) go out keyless.

use std::time::{Duration, Instant};

use alloy_primitives::Address;
use eyre::{Result, eyre};
use serde_json::{Value, json};
use url::Url;

use super::{
    EXPIRATION_MS, RefBlock, build_trigger, signed_hex, tron_address_bytes, tron_sign_txid, txid,
};
use crate::wallet::Wallet;

/// Tron's block interval. A transaction still unseen once the head is a block
/// past its expiration can no longer be included.
pub const BLOCK_INTERVAL_MS: i64 = 3_000;

// Fee limits cap the energy a transaction may burn, in sun (energy price 100
// sun). Each is 3 x the energy measured for that call on Nile x 100 sun.
// Bandwidth is burned on top of the limit. Mainnet USDT carries a dynamic
// energy_factor of 34000 (Nile's copy has none), so on mainnet the USDT legs
// (`deposit`'s transferFrom and a USDT voucher withdraw) cost several times the
// Nile energy and may exceed these limits; that surfaces as an OUT_OF_ENERGY
// `Err` naming the account, never as a false success.

/// USDT `approve`: 3 x 22,688 energy x 100 sun. (On Nile the token's origin
/// paid all of it.)
pub const APPROVE_FEE_LIMIT_SUN: i64 = 3 * 22_688 * 100;
/// MidribV3 `deposit(token, amount)` of USDT: 3 x 87,602 energy x 100 sun.
pub const DEPOSIT_FEE_LIMIT_SUN: i64 = 3 * 87_602 * 100;
/// MidribV3 `depositNative()` into a new balance slot: 3 x 32,798 energy x 100
/// sun.
pub const DEPOSIT_NATIVE_FEE_LIMIT_SUN: i64 = 3 * 32_798 * 100;
/// MidribV3 voucher `withdraw`, the costliest measured case (USDT through the
/// Tron payout override; a TRX payout to a never-activated address measured
/// 83,435): 3 x 84,386 energy x 100 sun.
pub const WITHDRAW_FEE_LIMIT_SUN: i64 = 3 * 84_386 * 100;

/// `base` with `suffix` appended to its path, keeping any query string.
fn join_path(base: &str, suffix: &str) -> Result<Url> {
    let mut url: Url = base
        .parse()
        .map_err(|e| eyre!("could not parse the Tron node url: {e}"))?;
    let path = format!("{}{suffix}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

/// The `/jsonrpc` URL for a Tron chain's HTTP base: where the EVM read code
/// points. The one place that path is derived.
pub fn jsonrpc_url(base: &str) -> Result<String> {
    Ok(join_path(base, "/jsonrpc")?.to_string())
}

/// A block's number, ID and timestamp, from either `getnowblock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    /// Block height.
    pub number: u64,
    /// Block ID; its first 8 bytes are the number (checked when read).
    pub id: [u8; 32],
    /// Block timestamp in milliseconds.
    pub time_ms: i64,
}

/// A transaction's execution record (`gettransactioninfobyid`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfo {
    /// The txID the record is for (checked against the one asked about).
    pub id: [u8; 32],
    /// The block that includes it.
    pub block_number: u64,
    /// `receipt.result == "SUCCESS"`, and nothing else.
    pub success: bool,
    /// Why it failed: the receipt result, the node's message and any revert
    /// data. `None` on success.
    pub failure: Option<String>,
}

/// Why a broadcast did not go through.
#[derive(Debug)]
pub enum BroadcastError {
    /// The node judged the transaction and refused it. The same bytes would
    /// get the same answer.
    Rejected {
        /// Tron's response code, e.g. `CONTRACT_VALIDATE_ERROR`.
        code: String,
        /// The node's decoded message.
        message: String,
    },
    /// Nobody answered, or the node was busy: whether it was received is
    /// unknown, and the identical bytes are safe to send again.
    Unanswered(eyre::Report),
}

/// The header TronGrid reads its API key from.
pub const API_KEY_HEADER: &str = "TRON-PRO-API-KEY";

/// The env-var key holding the Tron API key for `network`:
/// `ASPENS_TRON_API_KEY_<NETWORK>`, with `network` normalized as for
/// [`crate::chain_client::rpc_override_env_key`] (e.g. `tron-nile` →
/// `ASPENS_TRON_API_KEY_TRON_NILE`).
pub fn api_key_env_key(network: &str) -> String {
    format!(
        "ASPENS_TRON_API_KEY_{}",
        crate::chain_client::network_env_suffix(network)
    )
}

/// A Tron node's HTTP API at one base URL.
#[derive(Clone)]
pub struct TronHttp {
    base: String,
    client: reqwest::Client,
    /// Sent as [`API_KEY_HEADER`] on every request when set.
    api_key: Option<String>,
}

impl std::fmt::Debug for TronHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The base can hold a secret in its path or query, and the key is one.
        f.debug_struct("TronHttp")
            .field("base", &crate::chain_client::redact_rpc_url(&self.base))
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

/// What one POST came back with.
enum Posted {
    Answered(Value),
    /// A transport failure or a gateway error: the node never answered.
    Unanswered(eyre::Report),
}

impl TronHttp {
    /// A client for the node at `base` (the chain's configured RPC URL).
    pub fn new(base: &str) -> Result<Self> {
        join_path(base, "")?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| eyre!("could not build the Tron HTTP client: {e}"))?;
        Ok(Self {
            base: base.to_string(),
            client,
            api_key: None,
        })
    }

    /// The client for `network`'s node at `base`, carrying the API key from
    /// [`api_key_env_key`] when that variable is set and not blank.
    pub fn for_network(base: &str, network: &str) -> Result<Self> {
        Ok(Self::new(base)?.with_api_key(std::env::var(api_key_env_key(network)).ok()))
    }

    /// Send `key` as the [`API_KEY_HEADER`] header on every request. `None`
    /// or a blank key sends no header.
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        self
    }

    /// The node's HTTP base URL.
    pub fn base(&self) -> &str {
        &self.base
    }

    async fn post_raw(&self, path: &str, body: &Value) -> Result<Posted> {
        let url = join_path(&self.base, path)?;
        let endpoint = crate::chain_client::redact_rpc_url(&self.base);
        let mut request = self.client.post(url).json(body);
        if let Some(key) = &self.api_key {
            request = request.header(API_KEY_HEADER, key);
        }
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                let kind = if e.is_timeout() {
                    "timed out"
                } else if e.is_connect() {
                    "connect failed"
                } else {
                    "request failed"
                };
                return Ok(Posted::Unanswered(eyre!("{path} at {endpoint}: {kind}")));
            }
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(_) => {
                return Ok(Posted::Unanswered(eyre!(
                    "{path} at {endpoint}: the response body failed"
                )));
            }
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(json) if status.is_success() => Ok(Posted::Answered(json)),
            _ if status.is_server_error() || status.as_u16() == 429 || status.as_u16() == 403 => {
                Ok(Posted::Unanswered(eyre!(
                    "{path} at {endpoint}: HTTP {}",
                    status.as_u16()
                )))
            }
            _ => Err(eyre!(
                "{path} at {endpoint}: HTTP {} with an unreadable answer",
                status.as_u16()
            )),
        }
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        match self.post_raw(path, body).await? {
            Posted::Answered(v) => {
                if let Some(e) = v.get("Error").and_then(Value::as_str) {
                    return Err(eyre!("{path}: node error: {e}"));
                }
                Ok(v)
            }
            Posted::Unanswered(e) => Err(e),
        }
    }

    /// The head block (`/wallet/getnowblock`).
    pub async fn head_block(&self) -> Result<BlockHeader> {
        parse_block(
            &self.post("/wallet/getnowblock", &json!({})).await?,
            "getnowblock",
        )
    }

    /// The latest solidified block (`/walletsolidity/getnowblock`).
    pub async fn solidified_block(&self) -> Result<BlockHeader> {
        parse_block(
            &self.post("/walletsolidity/getnowblock", &json!({})).await?,
            "walletsolidity getnowblock",
        )
    }

    /// The reference for a new transaction: the solidified block's reference
    /// fields, and the head's time to count the expiration from.
    pub async fn reference_block(&self) -> Result<RefBlock> {
        let solid = self.solidified_block().await?;
        let head = self.head_block().await?;
        let be = solid.number.to_be_bytes();
        Ok(RefBlock {
            bytes: [be[6], be[7]],
            hash: solid.id[8..16].try_into().expect("8 bytes"),
            head_time_ms: head.time_ms,
        })
    }

    /// Run the call a transaction is about to make, as `owner`, against the
    /// head state, signing and sending nothing. `Ok` only when it executed
    /// successfully.
    pub async fn simulate(
        &self,
        owner: Address,
        contract: Address,
        data: &[u8],
        call_value_sun: i64,
    ) -> Result<()> {
        let mut body = json!({
            "owner_address": hex::encode(tron_address_bytes(owner)),
            "contract_address": hex::encode(tron_address_bytes(contract)),
            "data": hex::encode(data),
            "visible": false,
        });
        if call_value_sun != 0 {
            body["call_value"] = json!(call_value_sun);
        }
        let resp = self.post("/wallet/triggerconstantcontract", &body).await?;
        executed_successfully(&resp).map_err(|e| {
            let revert_data = resp
                .pointer("/constant_result/0")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(|d| format!(" (revert data 0x{d})"))
                .unwrap_or_default();
            eyre!("simulated call to {contract:#x} as {owner:#x}: {e}{revert_data}")
        })
    }

    /// Broadcast a signed transaction ([`signed_hex`]). A node that already
    /// holds it (`DUP_TRANSACTION_ERROR`) counts as accepted.
    pub async fn broadcast_hex(&self, signed: &str) -> Result<(), BroadcastError> {
        let resp = match self
            .post_raw("/wallet/broadcasthex", &json!({ "transaction": signed }))
            .await
        {
            Ok(Posted::Answered(v)) => v,
            Ok(Posted::Unanswered(e)) => return Err(BroadcastError::Unanswered(e)),
            Err(e) => return Err(BroadcastError::Unanswered(e)),
        };
        if resp.get("result").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let Some(code) = resp.get("code").and_then(Value::as_str) else {
            return Err(BroadcastError::Unanswered(eyre!(
                "broadcasthex: no result: {}",
                resp.get("Error")
                    .and_then(Value::as_str)
                    .unwrap_or("unrecognised response")
            )));
        };
        let message = hex_text(resp.get("message"));
        match code {
            "DUP_TRANSACTION_ERROR" => Ok(()),
            "SERVER_BUSY" | "NO_CONNECTION" | "NOT_ENOUGH_EFFECTIVE_CONNECTION" => Err(
                BroadcastError::Unanswered(eyre!("broadcasthex: {code}: {message}")),
            ),
            _ => Err(BroadcastError::Rejected {
                code: code.to_string(),
                message,
            }),
        }
    }

    /// The head execution record of `id`, or `None` while no block includes
    /// it.
    pub async fn tx_info(&self, id: [u8; 32]) -> Result<Option<TxInfo>> {
        let id_hex = hex::encode(id);
        let resp = self
            .post(
                "/wallet/gettransactioninfobyid",
                &json!({ "value": id_hex }),
            )
            .await?;
        if resp.as_object().is_some_and(|o| o.is_empty()) {
            return Ok(None);
        }
        let echoed = resp.get("id").and_then(Value::as_str);
        if echoed != Some(id_hex.as_str()) {
            return Err(eyre!(
                "gettransactioninfobyid {id_hex}: the node answered for {echoed:?}"
            ));
        }
        let block_number = resp
            .get("blockNumber")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("gettransactioninfobyid {id_hex}: no blockNumber"))?;
        let result = resp.pointer("/receipt/result").and_then(Value::as_str);
        let success = result == Some("SUCCESS");
        let failure = (!success).then(|| {
            let mut reason = result.unwrap_or("no receipt result").to_string();
            let message = hex_text(resp.get("resMessage"));
            if !message.is_empty() {
                reason.push_str(": ");
                reason.push_str(&message);
            }
            if let Some(data) = resp
                .pointer("/contractResult/0")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                reason.push_str(&format!(" (revert data 0x{data})"));
            }
            reason
        });
        Ok(Some(TxInfo {
            id,
            block_number,
            success,
            failure,
        }))
    }
}

fn parse_block(block: &Value, call: &str) -> Result<BlockHeader> {
    let id: [u8; 32] = block
        .get("blockID")
        .and_then(Value::as_str)
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| eyre!("{call}: no 32-byte blockID"))?;
    let header = block
        .pointer("/block_header/raw_data")
        .ok_or_else(|| eyre!("{call}: no block_header.raw_data"))?;
    let number = header
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("{call}: no block number"))?;
    let time_ms = header
        .get("timestamp")
        .and_then(Value::as_i64)
        .ok_or_else(|| eyre!("{call}: no block timestamp"))?;
    if id[..8] != number.to_be_bytes() {
        return Err(eyre!(
            "{call}: blockID {} does not belong to block number {number}",
            hex::encode(id)
        ));
    }
    Ok(BlockHeader {
        number,
        id,
        time_ms,
    })
}

/// Whether a `triggerconstantcontract` answer reports a successful execution.
/// `result.result` only says the node took the request; the outcome is the
/// returned transaction's `ret`, whose success is the omitted zero value.
fn executed_successfully(resp: &Value) -> Result<()> {
    let result = resp.get("result");
    if result
        .and_then(|r| r.get("result"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        let code = result
            .and_then(|r| r.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("no result");
        return Err(eyre!(
            "refused: {code}: {}",
            hex_text(result.and_then(|r| r.get("message")))
        ));
    }
    let ret = resp
        .pointer("/transaction/ret/0")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("the answer carries no execution result"))?;
    let code = ret.get("ret").and_then(Value::as_str);
    let contract_ret = ret.get("contractRet").and_then(Value::as_str);
    if !matches!(code, None | Some("SUCESS" | "SUCCESS"))
        || !matches!(contract_ret, None | Some("SUCCESS"))
    {
        return Err(eyre!(
            "did not succeed ({}): {}",
            contract_ret.or(code).unwrap_or("unknown"),
            hex_text(result.and_then(|r| r.get("message")))
        ));
    }
    Ok(())
}

/// Tron sends `message` / `resMessage` as hex-encoded UTF-8; fall back to the
/// raw value when it is not.
fn hex_text(v: Option<&Value>) -> String {
    let Some(s) = v.and_then(Value::as_str) else {
        return String::new();
    };
    hex::decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| s.to_string())
}

/// How [`send_tron_tx_with`] waits.
#[derive(Debug, Clone, Copy)]
pub struct SendConfig {
    /// Between two looks at the transaction.
    pub poll_interval: Duration,
    /// A backstop on the whole wait, for a node whose head stops advancing.
    /// Reaching it is an `Err`.
    pub give_up_after: Duration,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            give_up_after: Duration::from_secs(300),
        }
    }
}

/// A transaction the head reports as `SUCCESS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TronReceipt {
    /// The txID, `sha256(raw_data)`.
    pub txid: [u8; 32],
    /// The block that includes it.
    pub block_number: u64,
}

/// Why [`send_tron_tx`] did not succeed.
#[derive(Debug)]
pub enum TronTxError {
    /// Nothing was signed or broadcast: the simulation refused the call, or
    /// the node could not be read before signing. Trying again is free.
    NotSent(eyre::Report),
    /// The transaction was signed and may have reached a node; the message
    /// names its txID and what is known of its fate.
    Sent(eyre::Report),
}

impl std::fmt::Display for TronTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TronTxError::NotSent(e) => write!(f, "{e:#}; nothing was signed or sent"),
            TronTxError::Sent(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for TronTxError {}

/// Build, sign, broadcast and confirm one `TriggerSmartContract` call from
/// `wallet` to `contract`, with the default wait.
///
/// * The exact call is simulated first; a call that would revert is an `Err`
///   and nothing is signed or broadcast.
/// * The transaction references the solidified block and expires
///   [`EXPIRATION_MS`] after the head. It is signed once.
/// * A broadcast nobody answered is sent again as the identical hex, never
///   re-signed: Tron has no nonce, so a re-signed transaction (a new
///   timestamp, so a new txID) could land beside the first.
/// * `Ok` only for a receipt whose `receipt.result` is `SUCCESS`. Any other
///   receipt is an `Err` with the txID and the reason; OUT_OF_ENERGY and a
///   broadcast refused for the account's resources name the account. A failed
///   receipt is read at the head, so its message says the failure is not final
///   yet rather than that the transaction had no effect.
/// * Unseen once the head is a block past the expiration is an `Err` that
///   calls the transaction UNCONFIRMED and says to check the txID before
///   retrying: unseen through one base URL is not proof it never landed.
pub async fn send_tron_tx(
    http: &TronHttp,
    wallet: &Wallet,
    contract: Address,
    data: &[u8],
    call_value_sun: i64,
    fee_limit_sun: i64,
) -> Result<TronReceipt, TronTxError> {
    send_tron_tx_with(
        http,
        wallet,
        contract,
        data,
        call_value_sun,
        fee_limit_sun,
        &SendConfig::default(),
    )
    .await
}

/// [`send_tron_tx`] with an explicit wait policy.
pub async fn send_tron_tx_with(
    http: &TronHttp,
    wallet: &Wallet,
    contract: Address,
    data: &[u8],
    call_value_sun: i64,
    fee_limit_sun: i64,
    config: &SendConfig,
) -> Result<TronReceipt, TronTxError> {
    let owner = wallet
        .as_evm()
        .ok_or_else(|| {
            TronTxError::NotSent(eyre!("a Tron transaction needs a secp256k1 (EVM) wallet"))
        })?
        .address();

    // The very call about to be signed, run first: one that would revert is
    // never sent, since it would burn its energy and change nothing.
    http.simulate(owner, contract, data, call_value_sun)
        .await
        .map_err(TronTxError::NotSent)?;

    let reference = http.reference_block().await.map_err(TronTxError::NotSent)?;
    let raw = build_trigger(
        owner,
        contract,
        data,
        call_value_sun,
        fee_limit_sun,
        &reference,
    );
    let id = txid(&raw);
    let id_hex = hex::encode(id);
    let expiration_ms = reference.head_time_ms + EXPIRATION_MS;
    // The only signature this transaction gets.
    let signature = tron_sign_txid(wallet, &raw)
        .await
        .map_err(TronTxError::NotSent)?;
    let signed = signed_hex(&raw, &signature);

    let sent = |e: eyre::Report| TronTxError::Sent(e);
    let started = Instant::now();
    let mut accepted = false;
    let mut unanswered = false;
    loop {
        if !accepted {
            match http.broadcast_hex(&signed).await {
                Ok(()) => accepted = true,
                Err(BroadcastError::Rejected { code, message }) => {
                    return Err(sent(rejection(&id_hex, owner, &code, &message, unanswered)));
                }
                // Received or not, nobody said: the same bytes go out again.
                Err(BroadcastError::Unanswered(e)) => {
                    unanswered = true;
                    tracing::warn!(txid = %id_hex, error = %e, "tron broadcast unanswered; resending the same transaction");
                }
            }
        }

        // Looked for whether or not the broadcast was acknowledged: an
        // unanswered one may still have reached a node.
        match http.tx_info(id).await {
            Ok(Some(info)) => return judge(info, &id_hex, owner).map_err(sent),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(txid = %id_hex, error = %e, "tron receipt read failed; retrying")
            }
        }

        if let Ok(head) = http.head_block().await
            && head.time_ms > expiration_ms + BLOCK_INTERVAL_MS
        {
            // One last look: it may have landed between the read above and
            // this head. Unseen is still not proof it never landed: the base
            // URL can be a load balancer, so the head and this look may come
            // from different nodes. The outcome is unconfirmed, never "no
            // effect", so the user checks before sending it again.
            let endpoint = crate::chain_client::redact_rpc_url(http.base());
            return match http.tx_info(id).await {
                Ok(Some(info)) => judge(info, &id_hex, owner).map_err(sent),
                Ok(None) => Err(sent(eyre!(
                    "tron transaction {id_hex} is UNCONFIRMED: not seen before expiry by \
                     {endpoint} (its head, block {} at {} ms, is past the expiration \
                     {expiration_ms} ms plus a block); check the txID or the account {owner:#x} \
                     before retrying",
                    head.number,
                    head.time_ms
                ))),
                Err(e) => Err(sent(eyre!(
                    "tron transaction {id_hex} is UNCONFIRMED: past its expiration, and its \
                     receipt cannot be read ({e}); whether it landed is unknown, so check it \
                     before retrying"
                ))),
            };
        }

        if started.elapsed() >= config.give_up_after {
            return Err(sent(eyre!(
                "tron transaction {id_hex} is UNCONFIRMED: no receipt after {:?} and its \
                 expiration could not be judged; whether it landed is unknown, so check it \
                 before retrying",
                config.give_up_after
            )));
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

/// Said of a failed receipt: it was read at the head, where its block can
/// still be reorged out and the same signed bytes re-included before they
/// expire, so the failure is not final.
const AT_HEAD: &str = "That block is not yet solidified, so this is not final: check the txID \
                       once it is solidified before retrying";

/// `Ok` for a receipt whose result is `SUCCESS`, and only that.
fn judge(info: TxInfo, id_hex: &str, owner: Address) -> Result<TronReceipt> {
    if info.success {
        return Ok(TronReceipt {
            txid: info.id,
            block_number: info.block_number,
        });
    }
    let reason = info
        .failure
        .unwrap_or_else(|| "no receipt result".to_string());
    if reason.starts_with("OUT_OF_ENERGY") {
        return Err(eyre!(
            "tron transaction {id_hex} ran out of energy in block {}: {reason}. Energy is paid \
             by burning {owner:#x}'s TRX up to the fee limit: fund that account. {AT_HEAD}",
            info.block_number
        ));
    }
    Err(eyre!(
        "tron transaction {id_hex} FAILED at the head, in block {}: {reason}. {AT_HEAD}",
        info.block_number
    ))
}

fn rejection(
    id_hex: &str,
    owner: Address,
    code: &str,
    message: &str,
    earlier_unanswered: bool,
) -> eyre::Report {
    let caveat = if earlier_unanswered {
        "; an earlier broadcast of it went unanswered, so it may still land before it expires"
    } else {
        ""
    };
    let lower = message.to_ascii_lowercase();
    let funding = code == "BANDWITH_ERROR"
        || (code == "CONTRACT_VALIDATE_ERROR"
            && (lower.contains("balance")
                || lower.contains("owneraccount")
                || lower.contains("insufficient")));
    if funding {
        eyre!(
            "tron node refused transaction {id_hex}: {code}: {message}. The account {owner:#x} \
             cannot pay for it: fund it with TRX{caveat}"
        )
    } else {
        eyre!("tron node refused transaction {id_hex}: {code}: {message}{caveat}")
    }
}

/// A mock Tron node (wiremock) and helpers to read back what was sent to it,
/// shared by the deposit and withdraw tests.
#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    pub(crate) const SOLID_NUMBER: u64 = 72_052_479;
    pub(crate) const HEAD_TIME: i64 = 1_792_691_274_000;

    /// The fixture's test-only key (public by design, never funded on mainnet).
    pub(crate) fn wallet() -> Wallet {
        Wallet::from_evm_hex("0xdfdf4b45dfa67d802fa6d8472e275b02ed0ca396194d65ae3d334fa1b6801d5c")
            .unwrap()
    }

    pub(crate) fn fast() -> SendConfig {
        SendConfig {
            poll_interval: Duration::from_millis(5),
            give_up_after: Duration::from_secs(3600),
        }
    }

    pub(crate) fn block(number: u64, time_ms: i64) -> Value {
        json!({
            "blockID": format!("{:016x}{}", number, "cd".repeat(24)),
            "block_header": {"raw_data": {"timestamp": time_ms, "number": number}}
        })
    }

    /// A successful simulation, the solidified block, a head at
    /// [`HEAD_TIME`], and a head receipt route that knows no transaction.
    pub(crate) async fn mount_node(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/wallet/triggerconstantcontract"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"result": true},
                "constant_result": [""],
                "transaction": {"ret": [{}]},
                "energy_used": 32_798,
            })))
            .with_priority(10)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/walletsolidity/getnowblock"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(block(SOLID_NUMBER, HEAD_TIME - 57_000)),
            )
            .with_priority(10)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wallet/getnowblock"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(block(SOLID_NUMBER + 19, HEAD_TIME)),
            )
            .with_priority(10)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wallet/gettransactioninfobyid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .with_priority(10)
            .mount(server)
            .await;
    }

    pub(crate) async fn mount_broadcast_ok(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/wallet/broadcasthex"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": true})))
            .with_priority(10)
            .mount(server)
            .await;
    }

    /// Answers `gettransactioninfobyid` for whatever txID it is asked about:
    /// `{}` for the first `unseen` asks, then a record with `result`.
    pub(crate) struct TxInfoEcho {
        result: &'static str,
        unseen: usize,
        asked: AtomicUsize,
    }

    impl TxInfoEcho {
        pub(crate) fn new(result: &'static str, unseen: usize) -> Self {
            Self {
                result,
                unseen,
                asked: AtomicUsize::new(0),
            }
        }
    }

    impl Respond for TxInfoEcho {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            if self.asked.fetch_add(1, Ordering::SeqCst) < self.unseen {
                return ResponseTemplate::new(200).set_body_json(json!({}));
            }
            let asked: Value = serde_json::from_slice(&request.body).unwrap();
            let id = asked["value"].as_str().unwrap();
            let mut record = json!({
                "id": id, "blockNumber": SOLID_NUMBER + 21,
                "blockTimeStamp": HEAD_TIME + 3_000, "contractResult": [""],
                "receipt": {"energy_usage_total": 32_798, "result": self.result},
            });
            if self.result != "SUCCESS" {
                record["result"] = json!("FAILED");
                record["resMessage"] = json!(hex::encode("REVERT opcode executed"));
                record["contractResult"] = json!(["90b8ec18"]);
            }
            ResponseTemplate::new(200).set_body_json(record)
        }
    }

    pub(crate) async fn mount_tx_info(server: &MockServer, echo: TxInfoEcho) {
        Mock::given(method("POST"))
            .and(path("/wallet/gettransactioninfobyid"))
            .respond_with(echo)
            .with_priority(1)
            .mount(server)
            .await;
    }

    /// Answers every JSON-RPC request with `result`, echoing its id.
    pub(crate) struct JsonRpcEcho(pub(crate) &'static str);

    impl Respond for JsonRpcEcho {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let asked: Value = serde_json::from_slice(&request.body).unwrap();
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": asked["id"], "result": self.0}))
        }
    }

    pub(crate) async fn mount_jsonrpc(server: &MockServer, result: &'static str) {
        Mock::given(method("POST"))
            .and(path("/jsonrpc"))
            .respond_with(JsonRpcEcho(result))
            .mount(server)
            .await;
    }

    pub(crate) const MIDRIB: &str = "0xc76d503a0aecd5115aa23639fe90e2de7ad63719";
    pub(crate) const USDT: &str = "0xeca9bc828a3005b9a3b909f2cc5c2a54794de05f";

    /// Nile as the venue lists it: a MidribV3Tron instance, USDT and TRX.
    pub(crate) fn nile_chain() -> crate::commands::config::config_pb::Chain {
        use crate::commands::config::config_pb::{Chain, Token, TradeContract};
        let token = |symbol: &str, address: &str| Token {
            name: symbol.into(),
            symbol: symbol.into(),
            address: address.into(),
            decimals: 6,
        };
        Chain {
            architecture: "Tron".into(),
            network: "tron-nile".into(),
            chain_id: 3_448_148_188,
            trade_contract: Some(TradeContract {
                address: MIDRIB.into(),
            }),
            tokens: [
                ("USDT".to_string(), token("USDT", USDT)),
                (
                    "TRX".to_string(),
                    token("TRX", crate::evm::NATIVE_TOKEN_SENTINEL),
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    /// Bodies `server` received at `route`, as JSON.
    pub(crate) async fn bodies(server: &MockServer, route: &str) -> Vec<Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == route)
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    /// The `transaction` hex of every broadcast `server` received.
    pub(crate) async fn broadcasts(server: &MockServer) -> Vec<String> {
        bodies(server, "/wallet/broadcasthex")
            .await
            .iter()
            .map(|b| b["transaction"].as_str().unwrap().to_string())
            .collect()
    }

    /// One protobuf field: a varint or length-delimited bytes.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Field {
        Varint(u64),
        Bytes(Vec<u8>),
    }

    fn varint(buf: &[u8], at: &mut usize) -> u64 {
        let mut out = 0u64;
        let mut shift = 0;
        loop {
            let b = buf[*at];
            *at += 1;
            out |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return out;
            }
            shift += 7;
        }
    }

    pub(crate) fn fields(buf: &[u8]) -> Vec<(u64, Field)> {
        let mut at = 0;
        let mut out = Vec::new();
        while at < buf.len() {
            let key = varint(buf, &mut at);
            let field = match key & 7 {
                0 => Field::Varint(varint(buf, &mut at)),
                2 => {
                    let len = varint(buf, &mut at) as usize;
                    at += len;
                    Field::Bytes(buf[at - len..at].to_vec())
                }
                w => panic!("unexpected wire type {w}"),
            };
            out.push((key >> 3, field));
        }
        out
    }

    pub(crate) fn field(fields: &[(u64, Field)], number: u64) -> Option<Field> {
        fields
            .iter()
            .find(|(n, _)| *n == number)
            .map(|(_, f)| f.clone())
    }

    pub(crate) fn bytes_of(fields: &[(u64, Field)], number: u64) -> Vec<u8> {
        match field(fields, number) {
            Some(Field::Bytes(b)) => b,
            other => panic!("field {number}: {other:?}"),
        }
    }

    pub(crate) fn varint_of(fields: &[(u64, Field)], number: u64) -> u64 {
        match field(fields, number) {
            Some(Field::Varint(v)) => v,
            other => panic!("field {number}: {other:?}"),
        }
    }

    /// A broadcast transaction taken apart.
    pub(crate) struct Sent {
        pub(crate) raw: Vec<u8>,
        pub(crate) signature: Vec<u8>,
        pub(crate) trigger: Vec<(u64, Field)>,
    }

    pub(crate) fn decode_sent(hex_tx: &str) -> Sent {
        let tx = fields(&hex::decode(hex_tx).unwrap());
        let raw = bytes_of(&tx, 1);
        let signature = bytes_of(&tx, 2);
        let contract = fields(&bytes_of(&fields(&raw), 11));
        let any = fields(&bytes_of(&contract, 2));
        let trigger = fields(&bytes_of(&any, 2));
        Sent {
            raw,
            signature,
            trigger,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;
    use alloy_primitives::B256;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CONTRACT: &str = "0xc76d503a0aecd5115aa23639fe90e2de7ad63719";

    fn data() -> Vec<u8> {
        vec![0xe0, 0xa2, 0x73, 0xbc, 1, 2, 3]
    }

    async fn run(server: &MockServer) -> Result<TronReceipt, TronTxError> {
        let wallet = wallet();
        tokio::time::timeout(
            Duration::from_secs(10),
            send_tron_tx_with(
                &TronHttp::new(&server.uri()).unwrap(),
                &wallet,
                CONTRACT.parse().unwrap(),
                &data(),
                0,
                WITHDRAW_FEE_LIMIT_SUN,
                &fast(),
            ),
        )
        .await
        .expect("send_tron_tx must resolve, not wait forever")
    }

    /// The fee limits are the documented arithmetic, not hand-typed sums.
    #[test]
    fn fee_limits_are_three_times_measured_energy_at_100_sun() {
        assert_eq!(APPROVE_FEE_LIMIT_SUN, 6_806_400);
        assert_eq!(DEPOSIT_FEE_LIMIT_SUN, 26_280_600);
        assert_eq!(DEPOSIT_NATIVE_FEE_LIMIT_SUN, 9_839_400);
        assert_eq!(WITHDRAW_FEE_LIMIT_SUN, 25_315_800);
    }

    #[test]
    fn jsonrpc_url_keeps_the_query_and_trims_the_slash() {
        assert_eq!(
            jsonrpc_url("https://nile.trongrid.io/").unwrap(),
            "https://nile.trongrid.io/jsonrpc"
        );
        assert_eq!(
            jsonrpc_url("https://node.example/tron?apikey=K").unwrap(),
            "https://node.example/tron/jsonrpc?apikey=K"
        );
    }

    /// The sent transaction is the caller's call, referencing the solidified
    /// block, expiring a minute after the head, signed by the wallet; `Ok`
    /// carries that transaction's txID.
    #[tokio::test]
    async fn send_tron_tx_success() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 2)).await;
        let receipt = run(&server).await.unwrap();

        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 1, "one acknowledged broadcast is enough");
        let tx = decode_sent(&sent[0]);
        let raw = fields(&tx.raw);
        let solid_id = hex::decode(block(SOLID_NUMBER, 0)["blockID"].as_str().unwrap()).unwrap();
        assert_eq!(bytes_of(&raw, 1), SOLID_NUMBER.to_be_bytes()[6..8]);
        assert_eq!(bytes_of(&raw, 4), solid_id[8..16]);
        assert_eq!(varint_of(&raw, 8) as i64, HEAD_TIME + 60_000);
        assert_eq!(varint_of(&raw, 18) as i64, WITHDRAW_FEE_LIMIT_SUN);
        let owner = wallet().as_evm().unwrap().address();
        assert_eq!(bytes_of(&tx.trigger, 1), tron_address_bytes(owner));
        assert_eq!(
            bytes_of(&tx.trigger, 2),
            tron_address_bytes(CONTRACT.parse().unwrap())
        );
        assert_eq!(field(&tx.trigger, 3), None, "no call_value");
        assert_eq!(bytes_of(&tx.trigger, 4), data());
        let id = txid(&tx.raw);
        assert_eq!(receipt.txid, id);
        let sig = alloy_primitives::Signature::from_raw(&tx.signature).unwrap();
        assert_eq!(
            sig.recover_address_from_prehash(&B256::from(id)).unwrap(),
            owner
        );
    }

    /// A REVERT receipt is in a block, burned its energy, and did nothing:
    /// an `Err` naming the txID and the reason.
    #[tokio::test]
    async fn send_tron_tx_reverted_is_err() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("REVERT", 1)).await;
        let err = run(&server).await.unwrap_err();
        assert!(matches!(err, TronTxError::Sent(_)), "{err:?}");
        let err = err.to_string();
        let id = txid(&decode_sent(&broadcasts(&server).await[0]).raw);
        assert!(err.contains(&hex::encode(id)), "{err}");
        assert!(err.contains("REVERT opcode executed"), "{err}");
        assert!(err.contains("0x90b8ec18"), "{err}");
    }

    /// Any result other than SUCCESS is a failure, including one the code
    /// has never heard of.
    #[tokio::test]
    async fn only_success_is_success() {
        for result in ["OUT_OF_ENERGY", "FAILED", "SUCCESSFUL", "success"] {
            let server = MockServer::start().await;
            mount_node(&server).await;
            mount_broadcast_ok(&server).await;
            mount_tx_info(&server, TxInfoEcho::new(result, 0)).await;
            let err = run(&server).await.unwrap_err().to_string();
            assert!(err.contains(result), "{result}: {err}");
        }
    }

    #[tokio::test]
    async fn out_of_energy_names_the_account() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("OUT_OF_ENERGY", 0)).await;
        let err = run(&server).await.unwrap_err().to_string();
        let owner = format!("{:#x}", wallet().as_evm().unwrap().address());
        assert!(err.contains(&owner), "{err} does not name {owner}");
    }

    /// A node refusing the transaction for the account's resources says
    /// whose account to fund, and the transaction is not resent.
    #[tokio::test]
    async fn broadcast_rejected_is_err_naming_the_account() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        Mock::given(method("POST"))
            .and(path("/wallet/broadcasthex"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": false, "code": "CONTRACT_VALIDATE_ERROR",
                "message": hex::encode("Validate TriggerSmartContract error, balance is not sufficient.")
            })))
            .mount(&server)
            .await;
        let err = run(&server).await.unwrap_err().to_string();
        let owner = format!("{:#x}", wallet().as_evm().unwrap().address());
        assert!(err.contains(&owner), "{err}");
        assert!(err.contains("balance is not sufficient"), "{err}");
        assert_eq!(broadcasts(&server).await.len(), 1, "not resent");
    }

    /// Accepted, never seen, and the head is now a block past the expiration:
    /// no block can include it any more, so the wait ends in an `Err` naming
    /// the txID.
    #[tokio::test]
    async fn tron_tx_expired_unseen_is_err() {
        let server = MockServer::start().await;
        // The head the transaction is built against, then one just past
        // expiration + one block.
        Mock::given(method("POST"))
            .and(path("/wallet/getnowblock"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(block(SOLID_NUMBER + 19, HEAD_TIME)),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wallet/getnowblock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(block(
                SOLID_NUMBER + 41,
                HEAD_TIME + 60_000 + BLOCK_INTERVAL_MS + 1,
            )))
            .with_priority(2)
            .mount(&server)
            .await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        let err = run(&server).await.unwrap_err().to_string();
        let id = txid(&decode_sent(&broadcasts(&server).await[0]).raw);
        assert!(err.contains(&hex::encode(id)), "{err}");
        // Unseen through one base URL is not proof it never landed: the
        // message says UNCONFIRMED and to check first, never "no effect",
        // so a user does not deposit twice.
        assert!(err.contains("is UNCONFIRMED"), "{err}");
        assert!(err.contains("not seen before expiry by"), "{err}");
        assert!(err.contains(&server.uri()), "names the endpoint: {err}");
        assert!(err.contains("check the txID or the account"), "{err}");
        assert!(err.contains("before retrying"), "{err}");
        assert!(!err.contains("no effect"), "{err}");
    }

    /// A failed receipt is read at the head, whose block can still be
    /// reorged out: the message reports the failure without claiming it is
    /// final.
    #[tokio::test]
    async fn a_head_failure_is_not_called_final() {
        for result in ["FAILED", "OUT_OF_ENERGY", "REVERT"] {
            let server = MockServer::start().await;
            mount_node(&server).await;
            mount_broadcast_ok(&server).await;
            mount_tx_info(&server, TxInfoEcho::new(result, 0)).await;
            let err = run(&server).await.unwrap_err().to_string();
            assert!(err.contains("not yet solidified"), "{result}: {err}");
            assert!(err.contains("this is not final"), "{result}: {err}");
            assert!(!err.contains("no effect"), "{result}: {err}");
        }
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("FAILED", 0)).await;
        let err = run(&server).await.unwrap_err().to_string();
        assert!(err.contains("FAILED at the head"), "{err}");
    }

    /// Sends the default transaction through `http` and returns the
    /// `TRON-PRO-API-KEY` value every request carried (`None` for none).
    async fn api_keys_sent(server: &MockServer, http: TronHttp) -> Vec<Option<String>> {
        mount_node(server).await;
        mount_broadcast_ok(server).await;
        mount_tx_info(server, TxInfoEcho::new("SUCCESS", 1)).await;
        send_tron_tx_with(
            &http,
            &wallet(),
            CONTRACT.parse().unwrap(),
            &data(),
            0,
            WITHDRAW_FEE_LIMIT_SUN,
            &fast(),
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(requests.len() >= 5, "simulate, blocks, broadcast, receipt");
        requests
            .iter()
            .map(|r| {
                r.headers
                    .get(API_KEY_HEADER)
                    .map(|v| v.to_str().unwrap().to_string())
            })
            .collect()
    }

    /// A configured key goes on every request as the `TRON-PRO-API-KEY`
    /// header, which is how TronGrid authenticates; none is sent without one.
    #[tokio::test]
    async fn the_api_key_is_sent_as_a_header_on_every_request() {
        let server = MockServer::start().await;
        let http = TronHttp::new(&server.uri())
            .unwrap()
            .with_api_key(Some("k-123".to_string()));
        let keys = api_keys_sent(&server, http).await;
        assert!(
            keys.iter().all(|k| k.as_deref() == Some("k-123")),
            "{keys:?}"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.iter().all(|r| r.url.query().is_none()),
            "the key never rides in the query"
        );

        for key in [None, Some("  ".to_string())] {
            let server = MockServer::start().await;
            let http = TronHttp::new(&server.uri()).unwrap().with_api_key(key);
            let keys = api_keys_sent(&server, http).await;
            assert!(keys.iter().all(Option::is_none), "{keys:?}");
        }
    }

    /// The key's variable sits beside the endpoint's, one per network, and
    /// the key never shows in a `Debug` print.
    #[test]
    fn the_api_key_is_per_network_and_never_printed() {
        assert_eq!(
            api_key_env_key("tron-nile"),
            "ASPENS_TRON_API_KEY_TRON_NILE"
        );
        assert_eq!(api_key_env_key("tron"), "ASPENS_TRON_API_KEY_TRON");
        let http = TronHttp::new("https://api.trongrid.io/x?apikey=q-secret")
            .unwrap()
            .with_api_key(Some("k-secret".to_string()));
        let shown = format!("{http:?}");
        assert!(!shown.contains("k-secret"), "{shown}");
        assert!(!shown.contains("q-secret"), "{shown}");
        assert!(shown.contains("<set>"), "{shown}");
    }

    /// Exactly at expiration + one block the transaction can still be in
    /// that block: the wait goes on, and its receipt decides.
    #[tokio::test]
    async fn unseen_at_the_boundary_keeps_waiting() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/wallet/getnowblock"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(block(SOLID_NUMBER + 19, HEAD_TIME)),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wallet/getnowblock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(block(
                SOLID_NUMBER + 40,
                HEAD_TIME + 60_000 + BLOCK_INTERVAL_MS,
            )))
            .with_priority(2)
            .mount(&server)
            .await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 5)).await;
        run(&server).await.unwrap();
    }

    /// No node answered the first two broadcasts. The same bytes go out
    /// again, and every receipt asked about is that one transaction's.
    #[tokio::test]
    async fn rebroadcasts_identical_hex_on_transport_error() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        Mock::given(method("POST"))
            .and(path("/wallet/broadcasthex"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/wallet/broadcasthex"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": true})))
            .with_priority(2)
            .mount(&server)
            .await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 4)).await;
        let receipt = run(&server).await.unwrap();
        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 3, "two unanswered, one accepted");
        assert!(sent.iter().all(|s| *s == sent[0]), "identical bytes resent");
        let id = hex::encode(txid(&decode_sent(&sent[0]).raw));
        assert_eq!(hex::encode(receipt.txid), id);
        let asked = bodies(&server, "/wallet/gettransactioninfobyid").await;
        assert!(asked.iter().all(|b| b["value"] == id.as_str()), "{asked:?}");
    }

    /// A simulated revert is the transaction's fate if it were sent: nothing
    /// is signed or broadcast, and the error says why.
    #[tokio::test]
    async fn simulated_revert_never_broadcasts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/wallet/triggerconstantcontract"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transaction": {"ret": [{"ret": "FAILED"}]},
                "constant_result": ["90b8ec18"],
                "result": {"result": true, "message": hex::encode("REVERT opcode executed")},
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        let err = run(&server).await.unwrap_err();
        assert!(matches!(err, TronTxError::NotSent(_)), "{err:?}");
        let err = err.to_string();
        assert!(err.contains("REVERT opcode executed"), "{err}");
        assert!(err.contains("0x90b8ec18"), "{err}");
        assert!(err.contains("nothing was signed or sent"), "{err}");
        assert!(broadcasts(&server).await.is_empty());
    }

    /// The simulation is the very call that is then sent.
    #[tokio::test]
    async fn the_simulation_is_the_call_that_is_sent() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        let wallet = wallet();
        send_tron_tx_with(
            &TronHttp::new(&server.uri()).unwrap(),
            &wallet,
            CONTRACT.parse().unwrap(),
            &data(),
            10_000_000,
            DEPOSIT_NATIVE_FEE_LIMIT_SUN,
            &fast(),
        )
        .await
        .unwrap();
        let sims = bodies(&server, "/wallet/triggerconstantcontract").await;
        assert_eq!(sims.len(), 1);
        let owner = wallet.as_evm().unwrap().address();
        assert_eq!(
            sims[0]["owner_address"],
            hex::encode(tron_address_bytes(owner))
        );
        assert_eq!(sims[0]["data"], hex::encode(data()));
        assert_eq!(sims[0]["call_value"], 10_000_000);
        let sent = decode_sent(&broadcasts(&server).await[0]);
        assert_eq!(field(&sent.trigger, 3), Some(Field::Varint(10_000_000)));
    }
}
