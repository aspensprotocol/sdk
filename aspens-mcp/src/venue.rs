//! The one boundary between aspens-mcp's pure logic and the venue.
//!
//! [`Venue`] is what the MCP tools call; [`SdkVenue`] implements it over the
//! `aspens` SDK, and `fake::FakeVenue` implements it for tests without a network.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::config::{attestation_report_json, get_attestation, get_config};
use aspens::commands::trading::arborter_pb::{self, CancelOrderResponse, SendOrderResponse, Trade};
use aspens::commands::trading::balance::{Balances, fetch_balances};
use aspens::commands::trading::book_snapshot::{BookState, fetch_book_snapshot};
use aspens::commands::trading::cancel_order::call_cancel_order_from_config_with_wallet;
use aspens::commands::trading::deposit::call_deposit_from_config_with_wallet;
use aspens::commands::trading::send_order::{origin_network_for_side, send_order_with_wallets};
use aspens::commands::trading::stream_trades::{StreamTradesOptions, stream_trades};
use aspens::commands::trading::withdraw::{
    WithdrawOpts, call_withdraw_from_config_with_wallet_opts,
};
use aspens::health::{PingResult, ping_grpc_server};
use aspens::{AspensClient, CurveType, Wallet, chain_curve, load_trader_wallet};
use eyre::{Result, eyre};

use crate::config::Config;
use crate::ledger::{Fill, Side};
use crate::orders::order_id_hex;

/// How long a book or trades stream is collected before it is cut off.
pub const STREAM_WINDOW: Duration = Duration::from_millis(1500);

/// One fill of the agent's, from the venue's historical Trades stream.
/// Amounts are pair-decimal units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeFill {
    /// The order the trade matched (`0x` + 64 hex), `None` when the venue sent none.
    pub order_hit: Option<String>,
    pub qty: u128,
    pub price: u128,
    /// The venue's `Trade.timestamp`.
    pub ts: u64,
    /// The agent's address is one of the trade's maker addresses.
    pub maker: bool,
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

    /// The book, collected for [`STREAM_WINDOW`], optionally filtered to one trader.
    fn book(
        &self,
        market_id: String,
        filter_by_trader: Option<String>,
    ) -> impl Future<Output = Result<BookState>> + Send;

    /// `trader`'s historical fills on `market_id`, collected for [`STREAM_WINDOW`].
    fn my_fills(
        &self,
        market_id: String,
        trader: String,
    ) -> impl Future<Output = Result<Vec<TradeFill>>> + Send;

    /// Send a limit order. `qty` and `price` are HUMAN decimal strings (the SDK
    /// scales them by pair decimals): the caller formats them from pair units with
    /// `aspens::decimals::format_decimal_amount`. Never hidden, never a market
    /// order, never a discretionary fill.
    #[allow(clippy::too_many_arguments)]
    fn send_limit(
        &self,
        market_id: String,
        side: Side,
        qty: String,
        price: String,
        post_only: bool,
        settle_base: Option<String>,
        settle_quote: Option<String>,
    ) -> impl Future<Output = SendResult> + Send;

    fn cancel(
        &self,
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    ) -> impl Future<Output = Result<CancelResult>> + Send;

    /// `amount` is in the token's own decimals on `network`.
    fn deposit(
        &self,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> impl Future<Output = Result<()>> + Send;

    /// `amount` is in the token's own decimals on `network`.
    fn withdraw(
        &self,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> impl Future<Output = Result<()>> + Send;

    fn balances(&self) -> impl Future<Output = Result<Balances>> + Send;

    fn health(&self) -> impl Future<Output = PingResult> + Send;

    fn attestation(&self, nonce: Vec<u8>) -> impl Future<Output = Result<AttestationView>> + Send;

    fn addresses(&self) -> Addresses;
}

/// gRPC codes that mean the venue looked at the order and refused it.
const DEFINITIVE_REJECTIONS: [tonic::Code; 9] = [
    tonic::Code::InvalidArgument,
    tonic::Code::FailedPrecondition,
    tonic::Code::PermissionDenied,
    tonic::Code::Unauthenticated,
    tonic::Code::NotFound,
    tonic::Code::AlreadyExists,
    tonic::Code::ResourceExhausted,
    tonic::Code::OutOfRange,
    tonic::Code::Unimplemented,
];

fn find_status(e: &eyre::Report) -> Option<&tonic::Status> {
    e.chain().find_map(|c| c.downcast_ref::<tonic::Status>())
}

/// Map a send error. Only a server status that definitively refuses the order is
/// `Failed`; anything else (a timeout, a dropped connection, an error that is not
/// a status) may have placed the order and is `Unknown`.
pub fn classify_send_error(e: &eyre::Report) -> SendResult {
    match find_status(e) {
        Some(s) if DEFINITIVE_REJECTIONS.contains(&s.code()) => SendResult::Failed {
            code: format!("{:?}", s.code()),
            message: s.message().to_string(),
        },
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

/// EVM-style `0x` addresses compare lowercase; anything else (base58) exactly.
fn canonical_address(a: &str) -> String {
    if a.starts_with("0x") || a.starts_with("0X") {
        a.to_lowercase()
    } else {
        a.to_string()
    }
}

/// The agent made this trade as maker: its address is a maker address.
pub fn is_maker(trade: &Trade, agent: &str) -> bool {
    let agent = canonical_address(agent);
    [&trade.maker_base_address, &trade.maker_quote_address]
        .iter()
        .any(|m| canonical_address(m) == agent)
}

/// A trade's qty and price as pair-decimal `u128`s, `None` (logged) when unparseable.
fn trade_amounts(t: &Trade) -> Option<(u128, u128)> {
    match (t.qty.parse::<u128>(), t.price.parse::<u128>()) {
        (Ok(q), Ok(p)) => Some((q, p)),
        _ => {
            tracing::warn!(qty = %t.qty, price = %t.price, ts = t.timestamp, "skipping trade with unparseable amounts");
            None
        }
    }
}

/// Map a placement response. The id must be exactly 32 bytes; every fill's
/// `ts` is the venue's `Trade.timestamp`.
pub fn send_result_from_response(resp: &SendOrderResponse) -> SendResult {
    let Ok(id) = <[u8; 32]>::try_from(resp.order_id.as_slice()) else {
        return SendResult::Failed {
            code: "bad_order_id".to_string(),
            message: format!(
                "venue returned a {}-byte order id (expected 32)",
                resp.order_id.len()
            ),
        };
    };
    SendResult::Accepted {
        order_id: order_id_hex(&id),
        in_book: resp.order_in_book,
        taker_fills: resp
            .trades
            .iter()
            .filter_map(|t| {
                trade_amounts(t).map(|(qty, price)| Fill {
                    qty,
                    price,
                    ts: t.timestamp,
                })
            })
            .collect(),
    }
}

/// Map one Trades-stream entry to the agent's fill, `None` when it can't be read.
pub fn trade_fill(t: &Trade, agent: &str) -> Option<TradeFill> {
    let (qty, price) = trade_amounts(t)?;
    let order_hit = match t.order_hit.len() {
        0 => None,
        _ => match <[u8; 32]>::try_from(t.order_hit.as_slice()) {
            Ok(id) => Some(order_id_hex(&id)),
            Err(_) => {
                tracing::warn!(
                    len = t.order_hit.len(),
                    ts = t.timestamp,
                    "trade order_hit is not 32 bytes; fill left unattributed"
                );
                None
            }
        },
    };
    Some(TradeFill {
        order_hit,
        qty,
        price,
        ts: t.timestamp,
        maker: is_maker(t, agent),
    })
}

/// The agent's address on `market_id`'s base leg: the venue's `filter_by_trader`.
pub fn base_leg_address(
    config: &GetConfigResponse,
    market_id: &str,
    addrs: &Addresses,
) -> Result<String> {
    let market = config
        .get_market_by_id(market_id)
        .ok_or_else(|| eyre!("no such market in the venue config: {market_id}"))?;
    let chain = config
        .get_chain(&market.base_chain_network)
        .ok_or_else(|| eyre!("base chain '{}' not in config", market.base_chain_network))?;
    let addr = match chain_curve(chain)? {
        CurveType::Secp256k1 => &addrs.evm,
        CurveType::Ed25519 => &addrs.solana,
    };
    addr.clone().ok_or_else(|| {
        eyre!(
            "no trader key for base chain '{}' of {market_id}",
            market.base_chain_network
        )
    })
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

    async fn book(&self, market_id: String, filter_by_trader: Option<String>) -> Result<BookState> {
        fetch_book_snapshot(self.url.clone(), market_id, filter_by_trader, STREAM_WINDOW).await
    }

    async fn my_fills(&self, market_id: String, trader: String) -> Result<Vec<TradeFill>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Trade>();
        let opts = StreamTradesOptions {
            market_id,
            historical_closed_trades: true,
            filter_by_trader: Some(trader.clone()),
        };
        let mut handle = tokio::spawn(stream_trades(self.url.clone(), opts, move |t| {
            let _ = tx.send(t);
        }));
        let deadline = tokio::time::sleep(STREAM_WINDOW);
        tokio::pin!(deadline);
        let mut trades = Vec::new();
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    // The stream never ends on its own; stop it rather than leak it.
                    handle.abort();
                    break;
                }
                Some(t) = rx.recv() => trades.push(t),
                res = &mut handle => {
                    // A failed stream is an error, never an empty fill list.
                    res.map_err(|e| eyre!("trades stream task: {e}"))??;
                    break;
                }
            }
        }
        while let Ok(t) = rx.try_recv() {
            trades.push(t);
        }
        Ok(trades
            .iter()
            .filter_map(|t| trade_fill(t, &trader))
            .collect())
    }

    async fn send_limit(
        &self,
        market_id: String,
        side: Side,
        qty: String,
        price: String,
        post_only: bool,
        settle_base: Option<String>,
        settle_quote: Option<String>,
    ) -> SendResult {
        let config = match self.config().await {
            Ok(c) => c,
            // Nothing was signed or sent.
            Err(e) => {
                return SendResult::Failed {
                    code: "config_unavailable".to_string(),
                    message: format!("{e:#}"),
                };
            }
        };
        let wire = match side {
            Side::Buy => 1,
            Side::Sell => 2,
        };
        let result = send_order_with_wallets(
            self.url.clone(),
            market_id,
            wire,
            qty,
            Some(price),
            &self.wallets(),
            config,
            post_only,
            false,
            None,
            vec![],
            settle_base,
            settle_quote,
        )
        .await;
        match result {
            Ok(resp) => send_result_from_response(&resp),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "send_order failed");
                // The SDK turns a "session changed" refusal into a plain error;
                // the venue restarted, so the next send needs a fresh config.
                if format!("{e:#}").contains("session changed") {
                    self.invalidate_config();
                }
                classify_send_error(&e)
            }
        }
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

    async fn deposit(&self, network: String, token_symbol: String, amount: u128) -> Result<()> {
        let config = self.config().await?;
        let wallet = self.wallet_for_network(&config, &network)?;
        call_deposit_from_config_with_wallet(network, token_symbol, amount, wallet, config).await
    }

    async fn withdraw(&self, network: String, token_symbol: String, amount: u128) -> Result<()> {
        let config = self.config().await?;
        let wallet = self.wallet_for_network(&config, &network)?;
        call_withdraw_from_config_with_wallet_opts(
            self.url.clone(),
            network,
            token_symbol,
            amount,
            wallet,
            config,
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

    fn trade(maker_base: &str, maker_quote: &str, taker_base: &str) -> Trade {
        Trade {
            maker_base_address: maker_base.to_string(),
            maker_quote_address: maker_quote.to_string(),
            taker_base_address: taker_base.to_string(),
            taker_quote_address: taker_base.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn maker_flag_uses_canonical_lowercase() {
        let checksummed = "0x52908400098527886E0F7030069857D2E4169EE7";
        let lower = checksummed.to_lowercase();
        let other = "0x8617e340b3d01fa5f11f306f4090fd50e238070d";

        assert!(is_maker(&trade(&lower, other, other), checksummed));
        assert!(is_maker(&trade(other, &lower, other), checksummed));
        // Taker on both legs: not a maker fill.
        assert!(!is_maker(&trade(other, other, &lower), checksummed));

        // base58 compares exactly: case is significant.
        let sol = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
        assert!(is_maker(&trade(sol, other, other), sol));
        assert!(!is_maker(&trade(&sol.to_lowercase(), other, other), sol));
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
                priced(1_701, "x", "2500", vec![]),
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
        let short = SendOrderResponse {
            order_id: vec![1; 31],
            ..Default::default()
        };
        assert!(matches!(
            send_result_from_response(&short),
            SendResult::Failed { code, .. } if code == "bad_order_id"
        ));
    }

    #[test]
    fn trade_fill_maps_hit_ts_and_maker() {
        let agent = "0x52908400098527886E0F7030069857D2E4169EE7";
        let mut t = priced(1_900, "70", "3100", vec![0xcd; 32]);
        t.maker_base_address = agent.to_lowercase();
        assert_eq!(
            trade_fill(&t, agent),
            Some(TradeFill {
                order_hit: Some(format!("0x{}", "cd".repeat(32))),
                qty: 70,
                price: 3100,
                ts: 1_900,
                maker: true,
            })
        );
        let empty_hit = priced(1_901, "70", "3100", vec![]);
        let f = trade_fill(&empty_hit, agent).unwrap();
        assert_eq!((f.order_hit, f.maker), (None, false));
        assert_eq!(trade_fill(&priced(1_902, "70", "-1", vec![]), agent), None);
    }

    #[test]
    fn base_leg_address_follows_base_chain_curve() {
        use aspens::commands::config::config_pb::{Chain, Configuration, Market};
        let chain = |network: &str, arch: &str| Chain {
            network: network.to_string(),
            architecture: arch.to_string(),
            ..Default::default()
        };
        let market = |id: &str, base: &str, quote: &str| Market {
            market_id: id.to_string(),
            base_chain_network: base.to_string(),
            quote_chain_network: quote.to_string(),
            ..Default::default()
        };
        let cfg = GetConfigResponse {
            config: Some(Configuration {
                chains: vec![chain("sol", "solana"), chain("base", "evm")],
                markets: vec![
                    market("sol-base", "sol", "base"),
                    market("base-sol", "base", "sol"),
                ],
                ..Default::default()
            }),
        };
        let addrs = Addresses {
            evm: Some("0xevm".to_string()),
            solana: Some("SoLaddr".to_string()),
        };
        assert_eq!(
            base_leg_address(&cfg, "sol-base", &addrs).unwrap(),
            "SoLaddr"
        );
        assert_eq!(base_leg_address(&cfg, "base-sol", &addrs).unwrap(), "0xevm");
        let evm_only = Addresses {
            evm: Some("0xevm".to_string()),
            solana: None,
        };
        assert!(base_leg_address(&cfg, "sol-base", &evm_only).is_err());
        assert!(base_leg_address(&cfg, "nope", &addrs).is_err());
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
