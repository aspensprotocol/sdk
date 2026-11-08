//! The one boundary between aspens-mcp's pure logic and the venue.
//!
//! [`Venue`] is what the MCP tools call; [`SdkVenue`] implements it over the
//! `aspens` SDK, and `fake::FakeVenue` implements it for tests without a network.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::config::{attestation_report_json, get_attestation, get_config};
use aspens::commands::trading::arborter_pb::{
    self, CancelOrderResponse, FillRole, MyOrder, SendOrderResponse, Trade,
};
use aspens::commands::trading::balance::{Balances, fetch_balances};
use aspens::commands::trading::book_snapshot::{BookState, fetch_book_snapshot};
use aspens::commands::trading::cancel_order::call_cancel_order_from_config_with_wallet;
use aspens::commands::trading::deposit::call_deposit_from_config_with_wallet;
use aspens::commands::trading::my_orders::MAX_DELEGATION_TTL;
use aspens::commands::trading::send_order::{
    PreparedSend, origin_network_for_side, prepare_order_with_wallets,
    send_prepared_order_with_wallets,
};
use aspens::commands::trading::withdraw::{
    WithdrawOpts, call_withdraw_from_config_with_wallet_opts,
};
use aspens::health::{PingResult, ping_grpc_server};
use aspens::{
    AspensClient, CurveType, DelegateKey, ReadSession, Wallet, chain_curve,
    get_my_orders_from_config_with_wallet, list_my_orders_from_config_with_wallet,
    load_trader_wallet, open_read_session_with_wallets, refresh_read_session_with_wallets,
};
use eyre::{Result, eyre};

use crate::config::Config;
use crate::ledger::{Fill, Side};
use crate::orders::{OrderFill, OrderState, Role, VenueOrder, order_id_hex};

/// How long a book stream is collected before it is cut off.
pub const STREAM_WINDOW: Duration = Duration::from_millis(1500);

/// A limit order as the order path hands it to the venue. `qty` and `price`
/// are HUMAN decimal strings (the SDK scales them by the config's pair
/// decimals): the caller formats them from pair units with
/// `aspens::decimals::format_decimal_amount`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitOrder {
    pub market_id: String,
    pub side: Side,
    pub qty: String,
    pub price: String,
    pub post_only: bool,
    pub hidden: bool,
    pub settle_base: Option<String>,
    pub settle_quote: Option<String>,
}

/// The outcome of a send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResult {
    /// The venue placed the order. `order_id` is `0x` + 64 hex.
    Accepted {
        order_id: String,
        in_book: bool,
        taker_fills: Vec<Fill>,
    },
    /// The venue definitively refused the order: nothing was placed.
    Failed { code: String, message: String },
    /// The order may or may not have been placed.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    Cancelled,
    /// The order was not live: filled, cancelled before, or never existed.
    AlreadyGone,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttestationView {
    /// `attestation_report_json` of the venue's report.
    pub report: serde_json::Value,
    /// `Some(true)` only when the quote was fully verified; `None` when not checked.
    pub verified: Option<bool>,
    pub note: Option<String>,
}

/// The agent's trader addresses. `evm` is lowercase 0x-hex, as the venue reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Addresses {
    pub evm: Option<String>,
    pub solana: Option<String>,
}

/// Everything aspens-mcp asks of a venue.
pub trait Venue: Send + Sync + 'static {
    /// The venue config, cached after the first success.
    fn config(&self) -> impl Future<Output = Result<GetConfigResponse>> + Send;

    /// The public book, collected for [`STREAM_WINDOW`].
    fn book(&self, market_id: String) -> impl Future<Output = Result<BookState>> + Send;

    /// A limit order, built against `cfg` (the config snapshot the caller
    /// checked, never one fetched here) but neither signed nor sent, and the
    /// id the venue will derive for it (`0x` + 64 hex). Never a market order,
    /// never a discretionary fill. No I/O.
    type Prepared: Send + 'static;
    fn prepare_limit(
        &self,
        cfg: &GetConfigResponse,
        order: LimitOrder,
    ) -> Result<(String, Self::Prepared)>;

    /// Sign and send a prepared order, once.
    fn send_prepared(&self, prepared: Self::Prepared) -> impl Future<Output = SendResult> + Send;

    /// The agent's orders with these ids, as the venue's order-status records
    /// hold them. An id the venue has no record of for the agent is absent.
    fn orders_by_id(
        &self,
        ids: Vec<[u8; 32]>,
    ) -> impl Future<Output = Result<Vec<VenueOrder>>> + Send;

    /// The agent's open orders, on one market or all, plus with
    /// `include_closed` those closed within the venue's retention.
    fn list_orders(
        &self,
        market_id: Option<String>,
        include_closed: bool,
    ) -> impl Future<Output = Result<Vec<VenueOrder>>> + Send;

    fn cancel(
        &self,
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    ) -> impl Future<Output = Result<CancelResult>> + Send;

    /// `amount` is in the token's own decimals on `network`. Signed against
    /// `cfg`, the snapshot the caller checked.
    fn deposit(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> impl Future<Output = Result<()>> + Send;

    /// `amount` is in the token's own decimals on `network`. Signed against
    /// `cfg`, the snapshot the caller checked.
    fn withdraw(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> impl Future<Output = Result<()>> + Send;

    fn balances(&self) -> impl Future<Output = Result<Balances>> + Send;

    fn health(&self) -> impl Future<Output = PingResult> + Send;

    fn attestation(&self, nonce: Vec<u8>) -> impl Future<Output = Result<AttestationView>> + Send;

    fn addresses(&self) -> Addresses;
}

/// gRPC codes that mean the venue looked at the order and refused it. OutOfRange
/// is absent: tonic builds it on the client when decoding an oversized response,
/// after the request was written.
const DEFINITIVE_REJECTIONS: [tonic::Code; 8] = [
    tonic::Code::InvalidArgument,
    tonic::Code::FailedPrecondition,
    tonic::Code::PermissionDenied,
    tonic::Code::Unauthenticated,
    tonic::Code::NotFound,
    tonic::Code::AlreadyExists,
    tonic::Code::ResourceExhausted,
    tonic::Code::Unimplemented,
];

fn find_status(e: &eyre::Report) -> Option<&tonic::Status> {
    e.chain().find_map(|c| c.downcast_ref::<tonic::Status>())
}

/// Map a send error. Only a server status that definitively refuses the order is
/// `Failed`: a code in `DEFINITIVE_REJECTIONS` on a status with no `source()`.
/// A status with a source was built by tonic on the client from a transport
/// error (an h2 reset, an io error), possibly after the request was written.
/// Anything else (a timeout, a dropped connection, an error that is not a
/// status) may have placed the order and is `Unknown`.
pub fn classify_send_error(e: &eyre::Report) -> SendResult {
    use std::error::Error as _;
    match find_status(e) {
        Some(s) if s.source().is_none() && DEFINITIVE_REJECTIONS.contains(&s.code()) => {
            SendResult::Failed {
                code: format!("{:?}", s.code()),
                message: s.message().to_string(),
            }
        }
        _ => SendResult::Unknown,
    }
}

/// Map a cancel outcome. A `NotFound` status means the order is already gone.
pub fn classify_cancel(r: Result<CancelOrderResponse>) -> Result<CancelResult> {
    match r {
        // The SDK reports NOT_FOUND as `order_canceled: false`.
        Ok(resp) if resp.order_canceled => Ok(CancelResult::Cancelled),
        Ok(_) => Ok(CancelResult::AlreadyGone),
        Err(e) if find_status(&e).is_some_and(|s| s.code() == tonic::Code::NotFound) => {
            Ok(CancelResult::AlreadyGone)
        }
        Err(e) => Err(e),
    }
}

/// A trade's qty and price as pair-decimal `u128`s.
fn trade_amounts(t: &Trade) -> Result<(u128, u128)> {
    match (t.qty.parse::<u128>(), t.price.parse::<u128>()) {
        (Ok(q), Ok(p)) => Ok((q, p)),
        _ => Err(eyre!(
            "trade at ts {} has unparseable amounts: qty {:?}, price {:?}",
            t.timestamp,
            t.qty,
            t.price
        )),
    }
}

/// Map a placement response. The venue processed the order, so a response
/// that can't be read in full (an id that is not 32 bytes, a taker fill with
/// unparseable amounts) is `Unknown`, never `Failed`. Every fill's `ts` is the
/// venue's `Trade.timestamp`.
pub fn send_result_from_response(resp: &SendOrderResponse) -> SendResult {
    let Ok(id) = <[u8; 32]>::try_from(resp.order_id.as_slice()) else {
        tracing::warn!(
            len = resp.order_id.len(),
            "venue returned an order id that is not 32 bytes; outcome unknown"
        );
        return SendResult::Unknown;
    };
    let mut taker_fills = Vec::with_capacity(resp.trades.len());
    for t in &resp.trades {
        match trade_amounts(t) {
            Ok((qty, price)) => taker_fills.push(Fill {
                qty,
                price,
                ts: t.timestamp,
            }),
            Err(e) => {
                // Dropping it would under-report spend.
                tracing::warn!(error = %e, "unreadable taker fill; outcome unknown");
                return SendResult::Unknown;
            }
        }
    }
    SendResult::Accepted {
        order_id: order_id_hex(&id),
        in_book: resp.order_in_book,
        taker_fills,
    }
}

fn amount(what: &str, id: &str, v: &str) -> Result<u128> {
    v.parse::<u128>()
        .map_err(|_| eyre!("venue order {id}: {what} {v:?} is not a pair-unit integer"))
}

/// Map one entry of `GetMyOrders`/`ListMyOrders`: `None` when the venue has
/// no record of the id. Anything unreadable is an error, never dropped: a
/// dropped fill would under-report what filled.
pub fn venue_order(o: &MyOrder) -> Result<Option<VenueOrder>> {
    let id = <[u8; 32]>::try_from(o.order_id.as_slice())
        .map_err(|_| eyre!("venue returned an order id of {} bytes", o.order_id.len()))?;
    if !o.found {
        return Ok(None);
    }
    let order_id = order_id_hex(&id);
    let side = match arborter_pb::Side::try_from(o.side) {
        Ok(arborter_pb::Side::Bid) => Side::Buy,
        Ok(arborter_pb::Side::Ask) => Side::Sell,
        _ => return Err(eyre!("venue order {order_id}: unknown side {}", o.side)),
    };
    let remaining = if o.initial_qty_known {
        Some(amount("remaining qty", &order_id, &o.remaining_qty)?)
    } else {
        None
    };
    let mut fills = Vec::with_capacity(o.fills.len());
    for f in &o.fills {
        let fill_id = <[u8; 32]>::try_from(f.fill_id.as_slice()).map_err(|_| {
            eyre!(
                "venue order {order_id}: a fill id of {} bytes",
                f.fill_id.len()
            )
        })?;
        let role = match FillRole::try_from(f.role) {
            Ok(FillRole::Maker) => Role::Maker,
            Ok(FillRole::Taker) => Role::Taker,
            _ => {
                return Err(eyre!(
                    "venue order {order_id}: unknown fill role {}",
                    f.role
                ));
            }
        };
        fills.push(OrderFill {
            fill_id: Some(order_id_hex(&fill_id)),
            role,
            qty: amount("fill qty", &order_id, &f.qty)?,
            price: amount("fill price", &order_id, &f.price)?,
            ts: f.match_ts_ms,
        });
    }
    Ok(Some(VenueOrder {
        market_id: o.market_id.clone(),
        side,
        hidden: o.is_hidden,
        state: OrderState::from_venue(o.state),
        filled: amount("filled qty", &order_id, &o.filled_qty)?,
        remaining,
        fills,
        order_id,
    }))
}

/// The venue orders among `entries`, skipping those it has no record of.
pub fn venue_orders(entries: &[MyOrder]) -> Result<Vec<VenueOrder>> {
    entries
        .iter()
        .filter_map(|o| venue_order(o).transpose())
        .collect()
}

/// Why the agent cannot read the status of its `side` order on `market_id`:
/// the leg it gives (quote for a buy, base for a sell) is on a chain whose
/// curve has no trader key. `None` when it can.
pub fn collateral_key_missing(
    config: &GetConfigResponse,
    market_id: &str,
    side: Side,
    addrs: &Addresses,
) -> Result<Option<String>> {
    let network = origin_network_for_side(config, market_id, wire_side(side))?;
    let chain = config
        .get_chain(network)
        .ok_or_else(|| eyre!("chain '{network}' not in config"))?;
    let have = match chain_curve(chain)? {
        CurveType::Secp256k1 => addrs.evm.is_some(),
        CurveType::Ed25519 => addrs.solana.is_some(),
    };
    Ok((!have).then(|| {
        format!(
            "no trader key for chain '{network}', the leg this order gives, so its status cannot \
             be read"
        )
    }))
}

fn wire_side(side: Side) -> arborter_pb::Side {
    match side {
        Side::Buy => arborter_pb::Side::Bid,
        Side::Sell => arborter_pb::Side::Ask,
    }
}

/// [`Venue`] over the `aspens` SDK.
pub struct SdkVenue {
    url: String,
    evm: Option<Wallet>,
    solana: Option<Wallet>,
    attestation_mrtd: Option<String>,
    config: Mutex<Option<GetConfigResponse>>,
    /// The order-status read session. Its delegate key is made at startup,
    /// held in memory only and never written anywhere.
    read: tokio::sync::Mutex<ReadState>,
}

/// Make `state` a delegated read session that is not due for renewal.
///
/// The first time, the startup key is delegated by every trader wallet that
/// owns an account on the venue, for 24 hours. After that, a session with a
/// delegation under five minutes from expiry moves to a fresh delegate key
/// (`refresh_read_session_with_wallets`), re-signed by the wallets; the old key
/// is dropped and never re-delegated, so any one key reads for at most 24 h.
async fn ensure_session(
    state: &mut ReadState,
    cfg: &GetConfigResponse,
    wallets: &[&Wallet],
) -> Result<()> {
    let key = match std::mem::replace(state, ReadState::Empty) {
        ReadState::Session(mut s) => {
            let refreshed = refresh_read_session_with_wallets(cfg, &mut s, wallets).await;
            *state = ReadState::Session(s);
            return refreshed.map(|_| ());
        }
        ReadState::Key(k) => k,
        ReadState::Empty => DelegateKey::generate()?,
    };
    let session = open_read_session_with_wallets(cfg, wallets, key, MAX_DELEGATION_TTL).await?;
    *state = ReadState::Session(session);
    Ok(())
}

/// The read side's delegate key, before and after its delegations are signed.
enum ReadState {
    /// Not delegated yet.
    Key(DelegateKey),
    /// Delegated by every trader wallet that owns an account on the venue.
    Session(ReadSession),
    /// A delegation attempt failed after taking the key; a new key is made.
    Empty,
}

/// Load the trader wallet for `curve` when its variable is set; a set but
/// malformed key is an error.
fn load_wallet(curve: CurveType, var: &str) -> Result<Option<Wallet>> {
    if std::env::var(var).is_ok_and(|v| !v.trim().is_empty()) {
        load_trader_wallet(curve).map(Some)
    } else {
        Ok(None)
    }
}

impl SdkVenue {
    /// Load the env file into the process environment (once; the SDK reads keys
    /// from there) and the trader wallets. A curve with no key is `None`.
    pub fn new(cfg: &Config) -> Result<SdkVenue> {
        // With no env_file the keys come from the inherited environment. The
        // builder is only run when a file is named: unnamed, it would read a
        // stray `.env` from the working directory.
        if let Some(env_file) = &cfg.env_file {
            let path = env_file
                .to_str()
                .ok_or_else(|| eyre!("env_file path is not UTF-8: {}", env_file.display()))?;
            AspensClient::builder()
                .with_url(&cfg.stack_url)?
                .with_env_file(path)
                .build()?;
        }
        let evm = load_wallet(CurveType::Secp256k1, "TRADER_PRIVKEY")?;
        let solana = load_wallet(CurveType::Ed25519, "TRADER_PRIVKEY_SOLANA")?;
        if evm.is_none() && solana.is_none() {
            return Err(eyre!(
                "no trader key: set TRADER_PRIVKEY and/or TRADER_PRIVKEY_SOLANA"
            ));
        }
        Ok(SdkVenue {
            url: cfg.stack_url.clone(),
            evm,
            solana,
            attestation_mrtd: cfg.venue.as_ref().and_then(|v| v.attestation_mrtd.clone()),
            config: Mutex::new(None),
            read: tokio::sync::Mutex::new(ReadState::Key(DelegateKey::generate()?)),
        })
    }

    fn wallets(&self) -> Vec<&Wallet> {
        self.evm.iter().chain(self.solana.iter()).collect()
    }

    fn wallet_for(&self, curve: CurveType) -> Result<&Wallet> {
        match curve {
            CurveType::Secp256k1 => self.evm.as_ref(),
            CurveType::Ed25519 => self.solana.as_ref(),
        }
        .ok_or_else(|| eyre!("no trader key for curve {curve:?}"))
    }

    fn wallet_for_network(&self, config: &GetConfigResponse, network: &str) -> Result<&Wallet> {
        let chain = config
            .get_chain(network)
            .ok_or_else(|| eyre!("chain '{network}' not in config"))?;
        self.wallet_for(chain_curve(chain)?)
    }

    fn cached_config(&self) -> Option<GetConfigResponse> {
        self.config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Drop the cached config: the venue restarted and the cached session is stale.
    fn invalidate_config(&self) {
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

impl Venue for SdkVenue {
    async fn config(&self) -> Result<GetConfigResponse> {
        if let Some(c) = self.cached_config() {
            return Ok(c);
        }
        let c = get_config(self.url.clone()).await?;
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = Some(c.clone());
        Ok(c)
    }

    async fn book(&self, market_id: String) -> Result<BookState> {
        fetch_book_snapshot(self.url.clone(), market_id, None, STREAM_WINDOW).await
    }

    type Prepared = PreparedSend;

    fn prepare_limit(
        &self,
        cfg: &GetConfigResponse,
        order: LimitOrder,
    ) -> Result<(String, PreparedSend)> {
        let prepared = prepare_order_with_wallets(
            &order.market_id,
            wire_side(order.side) as i32,
            &order.qty,
            Some(&order.price),
            &self.wallets(),
            cfg,
            order.post_only,
            order.hidden,
            None,
            vec![],
            order.settle_base,
            order.settle_quote,
        )?;
        Ok((order_id_hex(&prepared.order_id()), prepared))
    }

    async fn send_prepared(&self, prepared: PreparedSend) -> SendResult {
        match send_prepared_order_with_wallets(self.url.clone(), prepared, &self.wallets()).await {
            Ok(resp) => send_result_from_response(&resp),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "send_order failed");
                // A "session changed" refusal: the venue restarted, so the
                // next `config()` must refetch.
                if format!("{e:#}").contains("session changed") {
                    self.invalidate_config();
                }
                classify_send_error(&e)
            }
        }
    }

    async fn orders_by_id(&self, ids: Vec<[u8; 32]>) -> Result<Vec<VenueOrder>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let cfg = self.config().await?;
        let mut state = self.read.lock().await;
        ensure_session(&mut state, &cfg, &self.wallets()).await?;
        let ReadState::Session(session) = &mut *state else {
            return Err(eyre!("internal: no read session after delegating"));
        };
        let entries =
            get_my_orders_from_config_with_wallet(&self.url, &cfg, session, &self.wallets(), &ids)
                .await?;
        venue_orders(&entries)
    }

    async fn list_orders(
        &self,
        market_id: Option<String>,
        include_closed: bool,
    ) -> Result<Vec<VenueOrder>> {
        let cfg = self.config().await?;
        let mut state = self.read.lock().await;
        ensure_session(&mut state, &cfg, &self.wallets()).await?;
        let ReadState::Session(session) = &mut *state else {
            return Err(eyre!("internal: no read session after delegating"));
        };
        let entries = list_my_orders_from_config_with_wallet(
            &self.url,
            &cfg,
            session,
            &self.wallets(),
            market_id.as_deref(),
            include_closed,
        )
        .await?;
        venue_orders(&entries)
    }

    async fn cancel(
        &self,
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    ) -> Result<CancelResult> {
        let config = self.config().await?;
        let origin = origin_network_for_side(&config, &market_id, wire_side(side))?;
        let wallet = self.wallet_for_network(&config, origin)?;
        let side = match side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        classify_cancel(
            call_cancel_order_from_config_with_wallet(
                self.url.clone(),
                market_id,
                side.to_string(),
                order_id,
                wallet,
                config,
            )
            .await,
        )
    }

    async fn deposit(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> Result<()> {
        let wallet = self.wallet_for_network(cfg, &network)?;
        call_deposit_from_config_with_wallet(network, token_symbol, amount, wallet, cfg.clone())
            .await
    }

    async fn withdraw(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> Result<()> {
        let wallet = self.wallet_for_network(cfg, &network)?;
        call_withdraw_from_config_with_wallet_opts(
            self.url.clone(),
            network,
            token_symbol,
            amount,
            wallet,
            cfg.clone(),
            WithdrawOpts::default(),
        )
        .await
    }

    async fn balances(&self) -> Result<Balances> {
        let config = self.config().await?;
        fetch_balances(config, &self.wallets()).await
    }

    async fn health(&self) -> PingResult {
        ping_grpc_server(self.url.clone()).await
    }

    async fn attestation(&self, nonce: Vec<u8>) -> Result<AttestationView> {
        let resp = get_attestation(self.url.clone(), Some(nonce)).await?;
        let report = resp
            .report
            .ok_or_else(|| eyre!("venue returned no attestation report"))?;
        let (verified, note) = attestation_verdict(self.attestation_mrtd.as_deref());
        Ok(AttestationView {
            report: attestation_report_json(&report),
            verified,
            note,
        })
    }

    fn addresses(&self) -> Addresses {
        Addresses {
            evm: self.evm.as_ref().map(|w| w.address().to_lowercase()),
            solana: self.solana.as_ref().map(Wallet::address),
        }
    }
}

/// What can be said about a quote given the configured pin. Never `Some(true)`:
/// `aspens::tdx_verify` needs a DCAP `QuoteVerifier` (the `dcap` feature, not
/// built here) and the expected pubkey manifest and image digests to rebuild
/// REPORTDATA. A pinned MRTD alone cannot decide, and a measurement match on an
/// unverified quote proves nothing.
fn attestation_verdict(mrtd: Option<&str>) -> (Option<bool>, Option<String>) {
    let note = match mrtd {
        None => "not verified: no venue.attestation_mrtd is pinned",
        Some(_) => {
            "not verified: a pinned MRTD alone cannot verify the quote; that needs DCAP \
             verification of the quote signature and the expected pubkey manifest and image \
             digests for REPORTDATA, which aspens-mcp does not have. Verify raw_quote offline"
        }
    };
    (None, Some(note.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_err(code: tonic::Code) -> eyre::Report {
        eyre::Report::new(tonic::Status::new(code, "venue said no"))
    }

    #[test]
    fn status_not_found_maps_to_already_gone() {
        assert_eq!(
            classify_cancel(Err(status_err(tonic::Code::NotFound))).unwrap(),
            CancelResult::AlreadyGone
        );
        // The SDK already turns NOT_FOUND into `order_canceled: false`.
        assert_eq!(
            classify_cancel(Ok(CancelOrderResponse {
                order_canceled: false
            }))
            .unwrap(),
            CancelResult::AlreadyGone
        );
        assert_eq!(
            classify_cancel(Ok(CancelOrderResponse {
                order_canceled: true
            }))
            .unwrap(),
            CancelResult::Cancelled
        );
        // A NotFound wrapped in context is still found.
        let wrapped = status_err(tonic::Code::NotFound).wrap_err("cancelling");
        assert_eq!(
            classify_cancel(Err(wrapped)).unwrap(),
            CancelResult::AlreadyGone
        );
        // Anything else is an error.
        assert!(classify_cancel(Err(status_err(tonic::Code::Internal))).is_err());
        assert!(classify_cancel(Err(status_err(tonic::Code::Unavailable))).is_err());
        assert!(classify_cancel(Err(eyre!("connection reset"))).is_err());
    }

    #[test]
    fn timeout_maps_to_unknown_server_status_maps_to_failed() {
        assert_eq!(DEFINITIVE_REJECTIONS.len(), 8);
        for code in DEFINITIVE_REJECTIONS {
            match classify_send_error(&status_err(code)) {
                SendResult::Failed { code: c, message } => {
                    assert_eq!(c, format!("{code:?}"), "{code:?}");
                    assert!(message.contains("venue said no"), "{message}");
                }
                other => panic!("{code:?} must be Failed, got {other:?}"),
            }
        }
        // Wrapped in context, the status is still found.
        let wrapped = status_err(tonic::Code::InvalidArgument).wrap_err("sending order");
        assert!(matches!(
            classify_send_error(&wrapped),
            SendResult::Failed { .. }
        ));

        for code in [
            tonic::Code::Unavailable,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Cancelled,
            tonic::Code::Unknown,
            tonic::Code::Internal,
            tonic::Code::Aborted,
            tonic::Code::DataLoss,
            tonic::Code::Ok,
            tonic::Code::OutOfRange,
        ] {
            assert_eq!(
                classify_send_error(&status_err(code)),
                SendResult::Unknown,
                "{code:?}"
            );
        }
        // A transport error that is not a status at all.
        assert_eq!(
            classify_send_error(&eyre!("transport error: connection reset")),
            SendResult::Unknown
        );
    }

    #[test]
    fn session_changed_is_failed_not_unknown() {
        use aspens::commands::trading::send_order::send_status_error;
        let e = send_status_error(tonic::Status::failed_precondition(
            "session changed: 3 -> 4",
        ));
        match classify_send_error(&e) {
            SendResult::Failed { code, message } => {
                assert_eq!(code, "FailedPrecondition");
                assert_eq!(message, "session changed: 3 -> 4");
            }
            other => panic!("a session-changed refusal placed nothing: {other:?}"),
        }
    }

    #[test]
    fn insufficient_balance_is_failed_not_unknown() {
        use aspens::commands::trading::send_order::{send_status_error, wrap_balance_error};
        let e = wrap_balance_error(
            send_status_error(tonic::Status::invalid_argument("insufficient balance")),
            "Insufficient deposited balance on base.\nToken: USDC".into(),
        );
        match classify_send_error(&e) {
            SendResult::Failed { code, message } => {
                assert_eq!(code, "InvalidArgument");
                assert_eq!(message, "insufficient balance");
            }
            other => panic!("an insufficient-balance refusal placed nothing: {other:?}"),
        }
        // A lost answer stays Unknown when enhanced too.
        let lost = wrap_balance_error(
            send_status_error(tonic::Status::unavailable("insufficient balance?")),
            "Insufficient deposited balance on base.".into(),
        );
        assert_eq!(classify_send_error(&lost), SendResult::Unknown);
    }

    fn priced(ts: u64, qty: &str, price: &str, hit: Vec<u8>) -> Trade {
        Trade {
            timestamp: ts,
            qty: qty.to_string(),
            price: price.to_string(),
            order_hit: hit,
            ..Default::default()
        }
    }

    #[test]
    fn response_maps_id_and_venue_timestamps() {
        let resp = SendOrderResponse {
            order_in_book: true,
            order_id: vec![0xab; 32],
            trades: vec![
                priced(1_700, "40", "2500", vec![]),
                priced(1_702, "60", "2600", vec![]),
            ],
            ..Default::default()
        };
        assert_eq!(
            send_result_from_response(&resp),
            SendResult::Accepted {
                order_id: format!("0x{}", "ab".repeat(32)),
                in_book: true,
                taker_fills: vec![
                    Fill {
                        qty: 40,
                        price: 2500,
                        ts: 1_700
                    },
                    Fill {
                        qty: 60,
                        price: 2600,
                        ts: 1_702
                    },
                ],
            }
        );
    }

    #[test]
    fn bad_order_id_in_ok_response_is_unknown() {
        for len in [0, 31, 33] {
            let resp = SendOrderResponse {
                order_in_book: true,
                order_id: vec![1; len],
                ..Default::default()
            };
            assert_eq!(
                send_result_from_response(&resp),
                SendResult::Unknown,
                "{len}"
            );
        }
    }

    #[test]
    fn unparseable_taker_fill_makes_send_unknown() {
        let resp = SendOrderResponse {
            order_id: vec![0xab; 32],
            trades: vec![
                priced(1_700, "40", "2500", vec![]),
                priced(1_701, "x", "2500", vec![]),
            ],
            ..Default::default()
        };
        assert_eq!(send_result_from_response(&resp), SendResult::Unknown);
    }

    #[test]
    fn out_of_range_is_unknown() {
        // Built by tonic's decoder on the client, after the request went out.
        let s = tonic::Status::out_of_range("Error, decoded message length too large");
        assert_eq!(
            classify_send_error(&eyre::Report::new(s)),
            SendResult::Unknown
        );
    }

    #[test]
    fn status_with_source_is_unknown() {
        for code in DEFINITIVE_REJECTIONS {
            let mut s = tonic::Status::new(code, "h2 reset");
            s.set_source(std::sync::Arc::new(std::io::Error::other("x")));
            assert_eq!(
                classify_send_error(&eyre::Report::new(s)),
                SendResult::Unknown,
                "{code:?}"
            );
        }
        // Status::from_error over an io error: also client-side, also Unknown.
        let s = tonic::Status::from_error(Box::new(std::io::Error::other("x")));
        assert_eq!(
            classify_send_error(&eyre::Report::new(s)),
            SendResult::Unknown
        );
    }

    fn my_order(found: bool) -> MyOrder {
        use aspens::commands::trading::arborter_pb::{MyFill, MyOrderState};
        MyOrder {
            order_id: vec![0xab; 32],
            found,
            market_id: "m-1".into(),
            side: arborter_pb::Side::Ask as i32,
            is_hidden: true,
            price: "2600".into(),
            initial_qty_known: true,
            initial_qty: "1200".into(),
            filled_qty: "450".into(),
            remaining_qty: "750".into(),
            state: MyOrderState::Cancelled as i32,
            fills: vec![
                MyFill {
                    fill_id: vec![0xc1; 32],
                    price: "2610".into(),
                    qty: "150".into(),
                    match_ts_ms: 1_700,
                    role: FillRole::Taker as i32,
                },
                MyFill {
                    fill_id: vec![0xc2; 32],
                    price: "2600".into(),
                    qty: "300".into(),
                    match_ts_ms: 1_800,
                    role: FillRole::Maker as i32,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn venue_order_maps_every_field() {
        let v = venue_order(&my_order(true)).unwrap().unwrap();
        assert_eq!(v.order_id, format!("0x{}", "ab".repeat(32)));
        assert_eq!(v.market_id, "m-1");
        assert_eq!(v.side, Side::Sell);
        assert!(v.hidden);
        assert_eq!(v.state, OrderState::Cancelled);
        assert_eq!(v.filled, 450);
        assert_eq!(v.remaining, Some(750));
        assert_eq!(
            v.fills,
            vec![
                OrderFill {
                    fill_id: Some(format!("0x{}", "c1".repeat(32))),
                    role: Role::Taker,
                    qty: 150,
                    price: 2610,
                    ts: 1_700,
                },
                OrderFill {
                    fill_id: Some(format!("0x{}", "c2".repeat(32))),
                    role: Role::Maker,
                    qty: 300,
                    price: 2600,
                    ts: 1_800,
                },
            ]
        );
    }

    #[test]
    fn venue_order_not_found_is_none_and_unknown_size_has_no_remaining() {
        assert_eq!(venue_order(&my_order(false)).unwrap(), None);
        let mut traded = my_order(true);
        traded.initial_qty_known = false;
        traded.remaining_qty = String::new();
        assert_eq!(venue_order(&traded).unwrap().unwrap().remaining, None);
        // `venue_orders` keeps only what the venue found.
        let got = venue_orders(&[my_order(false), my_order(true)]).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn unreadable_venue_order_is_an_error() {
        let mut bad_id = my_order(true);
        bad_id.order_id = vec![1; 31];
        assert!(venue_order(&bad_id).is_err());
        let mut bad_qty = my_order(true);
        bad_qty.fills[1].qty = "x".into();
        assert!(venue_order(&bad_qty).is_err());
        let mut bad_role = my_order(true);
        bad_role.fills[0].role = 0;
        assert!(venue_order(&bad_role).is_err());
        let mut bad_side = my_order(true);
        bad_side.side = 0;
        assert!(venue_order(&bad_side).is_err());
        let mut bad_remaining = my_order(true);
        bad_remaining.remaining_qty = "-1".into();
        assert!(venue_order(&bad_remaining).is_err());
        // One bad entry fails the whole read rather than vanishing from it.
        assert!(venue_orders(&[my_order(true), bad_qty]).is_err());
    }

    #[test]
    fn collateral_key_follows_the_leg_the_order_gives() {
        use aspens::commands::config::config_pb::{Chain, Configuration, Market};
        let chain = |network: &str, arch: &str| Chain {
            network: network.to_string(),
            architecture: arch.to_string(),
            ..Default::default()
        };
        let cfg = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![chain("sol", "solana"), chain("base", "evm")],
                markets: vec![Market {
                    market_id: "sol-base".to_string(),
                    base_chain_network: "sol".to_string(),
                    quote_chain_network: "base".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        };
        let evm_only = Addresses {
            evm: Some("0xevm".to_string()),
            solana: None,
        };
        // A buy gives quote (EVM here): readable. A sell gives base (Solana).
        assert_eq!(
            collateral_key_missing(&cfg, "sol-base", Side::Buy, &evm_only).unwrap(),
            None
        );
        let missing = collateral_key_missing(&cfg, "sol-base", Side::Sell, &evm_only)
            .unwrap()
            .unwrap();
        assert!(
            missing.contains("no trader key for chain 'sol'"),
            "{missing}"
        );
        assert!(collateral_key_missing(&cfg, "nope", Side::Buy, &evm_only).is_err());
    }

    /// The read session moves to a fresh delegate key when it re-delegates,
    /// so the key made at startup stops being renewed: a leaked key reads for
    /// at most 24 h however long the process runs.
    #[tokio::test]
    async fn read_session_refresh_rotates_the_delegate_key() {
        use aspens::commands::config::config_pb::{Chain, Configuration};
        let cfg = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![Chain {
                    network: "base-sepolia".into(),
                    architecture: "evm".into(),
                    ..Default::default()
                }],
                venue_id: "0123456789abcdef0123456789abcdef".into(),
                ..Default::default()
            }),
        };
        let wallet = Wallet::from_evm_hex(
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
        )
        .unwrap();
        let wallets = [&wallet];

        // First use: delegated for the startup key, for 24 h.
        let startup = DelegateKey::generate().unwrap();
        let startup_pk = startup.pubkey();
        let mut state = ReadState::Key(startup);
        ensure_session(&mut state, &cfg, &wallets).await.unwrap();
        let ReadState::Session(s) = &state else {
            panic!("no session")
        };
        assert_eq!(s.delegate_pubkey(), startup_pk);
        let d = &s.delegations()[0];
        assert_eq!(d.expires - d.issued, MAX_DELEGATION_TTL.as_secs());

        // Not due: the same key, untouched.
        ensure_session(&mut state, &cfg, &wallets).await.unwrap();
        let ReadState::Session(s) = &state else {
            panic!("no session")
        };
        assert_eq!(s.delegate_pubkey(), startup_pk);

        // Due (a delegation with a minute left): a new key, named by every
        // delegation, and the old key is gone from the session.
        let old = DelegateKey::generate().unwrap();
        let old_pk = old.pubkey();
        let short = aspens::create_read_delegation_with_wallet(
            &wallet,
            "base-sepolia",
            aspens::Architecture::Evm,
            "0123456789abcdef0123456789abcdef",
            &old,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let mut state = ReadState::Session(ReadSession::new(vec![short], old).unwrap());
        ensure_session(&mut state, &cfg, &wallets).await.unwrap();
        let ReadState::Session(s) = &state else {
            panic!("no session")
        };
        assert_ne!(s.delegate_pubkey(), old_pk);
        for d in s.delegations() {
            assert_eq!(d.delegate_pubkey, s.delegate_pubkey().to_vec());
        }
    }

    #[test]
    fn attestation_never_claims_verified() {
        for pin in [None, Some("ab".repeat(48))] {
            let (verified, note) = attestation_verdict(pin.as_deref());
            assert_eq!(verified, None);
            assert!(note.unwrap().starts_with("not verified"));
        }
    }
}
