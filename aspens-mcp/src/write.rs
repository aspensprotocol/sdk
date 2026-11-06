//! The bodies of the write tools: the order path, cancel, deposit, withdraw and
//! halt, and the venue pin that orders, deposits and withdrawals check.
//!
//! Every check runs before anything is signed. A refusal is an error whose text
//! lists every failed check and says that nothing was signed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::book_snapshot::marketable_price;
use aspens::decimals::format_decimal_amount;
use eyre::{Result, eyre};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::config::{Config, MarketLimits, parse_amount};
use crate::ledger::{Ledger, Record, SentOutcome, SentRecord, Side};
use crate::limits::{BookTop, OrderRequest, Refusal, addr_matches, check_order, utc_day};
use crate::orders::{VenueView, derive};
use crate::server::InFlight;
use crate::venue::{CancelResult, SendResult, Venue};
use crate::views::{self, put_amount};

/// How an order's limit price is set.
pub(crate) enum Pricing {
    /// The agent's price, a decimal string in market units.
    Limit(String),
    /// The book's best opposite price moved by `slippage_bps` against the agent.
    Marketable { slippage_bps: u32 },
}

pub(crate) struct PlaceRequest {
    pub market_id: String,
    pub side: Side,
    /// A decimal string in market units.
    pub qty: String,
    pub pricing: Pricing,
    pub post_only: bool,
    pub settle_base: Option<String>,
    pub settle_quote: Option<String>,
}

/// What the write tools act on, borrowed from the server.
pub(crate) struct Writer<'a, V: Venue> {
    pub venue: &'a Arc<V>,
    pub cfg: &'a Config,
    pub markets: &'a BTreeMap<String, MarketLimits>,
    pub ledger: &'a Arc<Mutex<Ledger>>,
    /// Set once the venue's attestation has verified; a failure is never kept.
    pub attested: &'a AtomicBool,
    /// Every send task holds a guard from here until its outcome is recorded.
    pub in_flight: &'a InFlight,
    pub now: u64,
}

fn refused(what: &str, reasons: &[String]) -> eyre::Report {
    eyre!("{what} refused, nothing was signed: {}", reasons.join("; "))
}

fn refusal_texts(rs: &[Refusal]) -> Vec<String> {
    rs.iter().map(ToString::to_string).collect()
}

fn wire_side(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

/// The state of an order just sent, from its send outcome alone: an accepted
/// order the venue put in the book rests with what its taker fills left.
fn placed_view(sent: &SentRecord) -> crate::orders::OrderView {
    let mut view = VenueView::default();
    if let SentOutcome::Accepted {
        order_id,
        in_book: true,
        taker_fills,
    } = &sent.outcome
    {
        let filled = taker_fills
            .iter()
            .fold(0u128, |acc, f| acc.saturating_add(f.qty));
        view.in_book
            .insert(order_id.clone(), sent.qty.saturating_sub(filled));
    }
    derive(sent, None, false, &view)
}

impl<V: Venue> Writer<'_, V> {
    /// The venue pin, when `[venue]` is set: every network in `networks` must
    /// have an `expected_contracts` entry equal to the venue's trade contract
    /// there, and a pinned `attestation_mrtd` needs a verified attestation.
    /// Returns every failure; empty means the pin holds (or is not set).
    async fn check_pin(&self, cfg: &GetConfigResponse, networks: &[&str]) -> Vec<Refusal> {
        let Some(pin) = &self.cfg.venue else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let networks: BTreeSet<&str> = networks.iter().copied().collect();
        for n in networks {
            let actual = cfg
                .get_chain(n)
                .and_then(|c| c.trade_contract.as_ref())
                .map(|t| t.address.as_str())
                .filter(|a| !a.is_empty());
            let msg = match (pin.expected_contracts.get(n), actual) {
                (None, _) => format!(
                    "[venue] is set but expected_contracts has no entry for network {n}, so its \
                     contract cannot be checked"
                ),
                (Some(_), None) => format!("the venue reports no trade contract for network {n}"),
                (Some(want), Some(got)) if !addr_matches(got, want) => format!(
                    "network {n}: the venue reports trade contract {got}, expected_contracts \
                     says {want}"
                ),
                (Some(_), Some(_)) => continue,
            };
            out.push(Refusal::VenuePin(msg));
        }
        if pin.attestation_mrtd.is_some() && !self.attested.load(Ordering::Acquire) {
            let mut nonce = [0u8; 32];
            let verdict = match getrandom::fill(&mut nonce) {
                Err(e) => Err(format!("generating a nonce: {e}")),
                Ok(()) => match self.venue.attestation(nonce.to_vec()).await {
                    Ok(v) if v.verified == Some(true) => Ok(()),
                    Ok(v) => Err(format!(
                        "venue.attestation_mrtd is set, and the venue attestation did not verify \
                         ({}). This fails closed: aspens-mcp cannot verify an attestation yet, so \
                         with attestation_mrtd set every order, deposit and withdrawal is \
                         refused. Remove attestation_mrtd to rely on the contract pin alone",
                        v.note.as_deref().unwrap_or("no verdict")
                    )),
                    Err(e) => Err(format!("fetching the venue attestation: {e:#}")),
                },
            };
            match verdict {
                Ok(()) => self.attested.store(true, Ordering::Release),
                Err(msg) => out.push(Refusal::VenuePin(msg)),
            }
        }
        out
    }

    /// The order path. In order: the checks that need no venue (halted); take
    /// one venue config snapshot (everything
    /// below is checked against it, and the order is signed against it); parse
    /// amounts; read the book; check the venue pin; then take the ledger, check
    /// the limits, and in a spawned task record the usage, send, and record the
    /// outcome. The ledger is held from the check to the outcome record, so no
    /// read can see a sent order the ledger lacks, and two orders cannot both
    /// pass on the same usage. The task runs to completion even if this future
    /// is dropped, so a usage record is never left without its outcome.
    pub async fn place(&self, req: PlaceRequest) -> Result<Value> {
        let what = "order";
        // The refusals that need no venue, first: a halted server says so even
        // when the venue is unreachable. The limits check below re-reads the
        // halt under the ledger lock; these only lead an early refusal.
        let offline = self.offline_checks(false).await;
        let early = |reason: String| {
            let mut rs = offline.clone();
            rs.push(reason);
            refused(what, &rs)
        };
        let Some(limits) = self.markets.get(&req.market_id) else {
            return Err(early(
                Refusal::MarketNotTradable(req.market_id.clone()).to_string(),
            ));
        };
        let d = limits.pair_decimals;

        // The one config snapshot. Limits were resolved in the pair units of
        // the config at startup; a venue that changed them since would have
        // these amounts scaled differently than they were checked.
        let vcfg = match self.venue.config().await {
            Ok(c) => c,
            Err(_) if !offline.is_empty() => return Err(refused(what, &offline)),
            Err(e) => return Err(e),
        };
        let market = vcfg.get_market_by_id(&req.market_id).ok_or_else(|| {
            early(format!(
                "market {} is no longer in the venue config",
                req.market_id
            ))
        })?;
        if i64::from(market.pair_decimals) != i64::from(d) {
            return Err(early(format!(
                "venue changed this market's pair decimals ({d} \u{2192} {}); restart \
                 aspens-mcp to re-resolve limits",
                market.pair_decimals
            )));
        }
        let networks = [
            market.base_chain_network.clone(),
            market.quote_chain_network.clone(),
        ];

        // 1. Amounts, rejecting any precision the market cannot carry.
        let qty =
            parse_amount(&req.qty, d).map_err(|e| early(format!("qty {:?}: {e:#}", req.qty)))?;
        // `Ok(price)` for a limit order; `Err(slippage_bps)` until the book is read.
        let limit_price: std::result::Result<u128, u32> = match &req.pricing {
            Pricing::Limit(p) => {
                Ok(parse_amount(p, d).map_err(|e| early(format!("price {p:?}: {e:#}")))?)
            }
            Pricing::Marketable { slippage_bps } => Err(*slippage_bps),
        };

        // 2. One book read: the price guard's reference, and a marketable price.
        let book = match self.venue.book(req.market_id.clone(), None).await {
            Ok(b) => b,
            Err(_) if !offline.is_empty() => return Err(refused(what, &offline)),
            Err(e) => return Err(e),
        };
        let top = BookTop {
            best_bid: book.best_bid(),
            best_ask: book.best_ask(),
        };
        let price = match limit_price {
            Ok(p) => p,
            Err(slippage_bps) => marketable_price(&book, req.side == Side::Buy, slippage_bps)
                .map_err(|e| early(format!("{e:#}")))?,
        };

        // 3. The venue pin, on both legs' networks, against the snapshot.
        let pin = self
            .check_pin(&vcfg, &[networks[0].as_str(), networks[1].as_str()])
            .await;

        // 4. The limits, with the ledger held until the outcome is recorded.
        let mut ledger = self.ledger.clone().lock_owned().await;
        let day = utc_day(self.now);
        let used = ledger.usage(&day, &req.market_id);
        let halted = ledger.halted();
        let checked = check_order(
            self.markets,
            &self.cfg.settle_address_allowlist,
            halted.as_ref(),
            used,
            &top,
            &OrderRequest {
                market_id: &req.market_id,
                side: req.side,
                qty,
                price,
                settle_base: req.settle_base.as_deref(),
                settle_quote: req.settle_quote.as_deref(),
            },
        );
        let commit = match checked {
            Ok(c) if pin.is_empty() => c,
            Ok(_) => return Err(refused(what, &refusal_texts(&pin))),
            Err(mut rs) => {
                rs.extend(pin);
                return Err(refused(what, &refusal_texts(&rs)));
            }
        };

        // 5-7 run in their own task, which owns the ledger guard: dropping
        // this future cannot stop it between the usage and the outcome.
        let venue = self.venue.clone();
        let now = self.now;
        let in_flight = self.in_flight.enter();
        let task = tokio::spawn(async move {
            let _in_flight = in_flight;
            // 5. Usage, durable before anything is signed.
            ledger
                .append(Record::Usage {
                    ts: now,
                    day,
                    market_id: req.market_id.clone(),
                    side: req.side,
                    qty: commit.qty,
                    quote: commit.quote,
                    price,
                })
                .map_err(|e| eyre!("recording usage failed, nothing was signed: {e:#}"))?;

            // 6. Send, signed against the checked snapshot. The SDK takes human
            // strings and scales them by its pair decimals, checked equal above.
            let result = venue
                .send_limit(
                    &vcfg,
                    req.market_id.clone(),
                    req.side,
                    format_decimal_amount(qty, d),
                    format_decimal_amount(price, d),
                    req.post_only,
                    req.settle_base.clone(),
                    req.settle_quote.clone(),
                )
                .await;

            // 7. The outcome. `Unknown` is recorded as such and never re-sent.
            let outcome = match result {
                SendResult::Accepted {
                    order_id,
                    in_book,
                    taker_fills,
                } => SentOutcome::Accepted {
                    order_id,
                    in_book,
                    taker_fills,
                },
                SendResult::Failed { code, message } => SentOutcome::Failed { code, message },
                SendResult::Unknown => SentOutcome::Unknown,
            };
            let sent = SentRecord {
                ts: now,
                market_id: req.market_id,
                side: req.side,
                qty,
                price,
                outcome,
            };
            if let Err(e) = ledger.append(Record::Sent(sent.clone())) {
                tracing::error!(error = %format!("{e:#}"), outcome = ?sent.outcome, "order sent but its outcome was not recorded");
                return Err(eyre!(
                    "the order was sent ({:?}) but recording its outcome failed: {e:#}. Do not \
                     resubmit it; its usage is recorded",
                    sent.outcome
                ));
            }
            Ok(sent)
        });
        let sent = task.await.map_err(|e| {
            eyre!("internal: the send task failed ({e}); do not resubmit, check my_orders")
        })??;

        // 8. The order as it stands.
        Ok(views::order(&sent, &placed_view(&sent), d))
    }

    /// Cancel an order. An id in the ledger takes its market and side from
    /// there; any other id needs both. Allowed while halted.
    pub async fn cancel(
        &self,
        order_id: &str,
        market_id: Option<String>,
        side: Option<Side>,
    ) -> Result<Value> {
        let id = crate::server::parse_order_id(order_id)?;
        let from_ledger = {
            let ledger = self.ledger.lock().await;
            ledger.sent().into_iter().find_map(|s| match &s.outcome {
                SentOutcome::Accepted { order_id, .. } if *order_id == id => {
                    Some((s.market_id.clone(), s.side))
                }
                _ => None,
            })
        };
        let (market_id, side) = match (from_ledger, market_id, side) {
            (Some(known), _, _) => known,
            (None, Some(m), Some(s)) => (m, s),
            (None, m, s) => {
                let missing: Vec<&str> = [("market_id", m.is_none()), ("side", s.is_none())]
                    .into_iter()
                    .filter_map(|(f, missing)| missing.then_some(f))
                    .collect();
                return Err(eyre!(
                    "cancel refused, nothing was signed: order {id} is not in this server's \
                     ledger, so market_id and side are both required (missing: {})",
                    missing.join(", ")
                ));
            }
        };
        let vcfg = self.venue.config().await?;
        if vcfg.get_market_by_id(&market_id).is_none() {
            return Err(eyre!(
                "cancel refused, nothing was signed: no such market in the venue config: \
                 {market_id}"
            ));
        }
        let bytes = hex::decode(&id[2..]).map_err(|e| eyre!("internal: order id {id}: {e}"))?;
        let raw = <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| eyre!("internal: order id {id} is not 32 bytes"))?;

        let result = match self.venue.cancel(market_id.clone(), side, raw).await? {
            CancelResult::Cancelled => {
                let rec = Record::Cancelled {
                    ts: self.now,
                    order_id: id.clone(),
                };
                if let Err(e) = self.ledger.lock().await.append(rec) {
                    return Err(eyre!(
                        "the venue cancelled order {id} but recording it failed: {e:#}"
                    ));
                }
                json!({ "result": "cancelled", "note": null })
            }
            // Filled or cancelled before: unknown which, so nothing is recorded.
            CancelResult::AlreadyGone => json!({
                "result": "already_gone",
                "note": "not live on the venue: filled, cancelled earlier, or never placed",
            }),
        };
        let mut out = views::object(json!({
            "orderId": id,
            "marketId": market_id,
            "side": wire_side(side),
        }));
        out.extend(views::object(result));
        Ok(Value::Object(out))
    }

    /// The token's address and decimals on `network`, from the venue config.
    fn token(cfg: &GetConfigResponse, network: &str, symbol: &str) -> Result<(String, u32)> {
        cfg.get_token(network, symbol)
            .map(|t| (t.address.clone(), t.decimals))
            .ok_or_else(|| eyre!("no token {symbol} on network {network} in the venue config"))
    }

    /// `deposits.max_per_call` for this token, in its own units: keyed
    /// `"<network>:<token-address>"`, the address compared case-insensitively
    /// when it is 0x hex.
    fn max_per_call(&self, network: &str, address: &str, decimals: u32) -> Result<Option<u128>> {
        for (key, value) in &self.cfg.deposits.max_per_call {
            let Some((n, a)) = key.split_once(':') else {
                continue;
            };
            if n == network && addr_matches(a, address) {
                return parse_amount(value, decimals)
                    .map(Some)
                    .map_err(|e| eyre!("deposits.max_per_call.\"{key}\" = {value:?}: {e:#}"));
            }
        }
        Ok(None)
    }

    /// Parse a transfer amount in the token's units; zero is refused.
    fn transfer_amount(amount: &str, decimals: u32) -> Result<u128> {
        let raw = parse_amount(amount, decimals).map_err(|e| eyre!("amount {amount:?}: {e:#}"))?;
        if raw == 0 {
            return Err(eyre!("amount must be greater than zero"));
        }
        Ok(raw)
    }

    fn transfer_view(network: &str, token: &str, raw: u128, decimals: u32) -> Value {
        let mut m = views::object(json!({
            "network": network,
            "token": token,
            "status": "submitted",
        }));
        put_amount(&mut m, "amount", Some(raw), decimals);
        Value::Object(m)
    }

    /// The refusals that need no venue: halted, and for deposits
    /// `deposits.enabled`. Checked first, so a halted server says so even
    /// when the venue is unreachable.
    async fn offline_checks(&self, deposit: bool) -> Vec<String> {
        let mut rs = Vec::new();
        if let Some(h) = self.ledger.lock().await.halted() {
            rs.push(Refusal::Halted(h).to_string());
        }
        if deposit && !self.cfg.deposits.enabled {
            rs.push("deposits are disabled ([deposits] enabled = false)".to_string());
        }
        rs
    }

    /// The checks of a deposit or withdrawal, against one config snapshot.
    /// Returns the snapshot and the amount in the token's units, or every
    /// refusal. Deposit order: halted, `deposits.enabled`, `max_per_call`, the
    /// venue pin; a withdrawal skips the two deposit checks.
    async fn check_transfer(
        &self,
        what: &str,
        deposit: bool,
        network: &str,
        token: &str,
        amount: &str,
    ) -> Result<(GetConfigResponse, u128, u32)> {
        let mut rs = self.offline_checks(deposit).await;
        let vcfg = match self.venue.config().await {
            Ok(c) => c,
            Err(_) if !rs.is_empty() => return Err(refused(what, &rs)),
            Err(e) => return Err(e),
        };
        let checked = Self::token(&vcfg, network, token).and_then(|(address, decimals)| {
            let raw = Self::transfer_amount(amount, decimals)?;
            Ok((address, decimals, raw))
        });
        match checked {
            Err(e) => rs.push(format!("{e:#}")),
            Ok((address, decimals, raw)) => {
                if deposit {
                    match self.max_per_call(network, &address, decimals) {
                        Ok(Some(max)) if raw > max => rs.push(format!(
                            "max_per_call exceeded: amount {} ({raw}) > limit {} ({max}) for \
                             {network}:{address}",
                            format_decimal_amount(raw, decimals),
                            format_decimal_amount(max, decimals),
                        )),
                        Ok(_) => {}
                        Err(e) => rs.push(format!("{e:#}")),
                    }
                }
                rs.extend(refusal_texts(&self.check_pin(&vcfg, &[network]).await));
                if rs.is_empty() {
                    return Ok((vcfg, raw, decimals));
                }
            }
        }
        Err(refused(what, &rs))
    }

    /// Deposit from the agent's wallet, signed against the checked snapshot.
    /// Not usage.
    pub async fn deposit(&self, network: String, token: String, amount: String) -> Result<Value> {
        let (vcfg, raw, decimals) = self
            .check_transfer("deposit", true, &network, &token, &amount)
            .await?;
        self.venue
            .deposit(&vcfg, network.clone(), token.clone(), raw)
            .await?;
        Ok(Self::transfer_view(&network, &token, raw, decimals))
    }

    /// Withdraw to the agent's own address (the SDK pays the signing wallet),
    /// signed against the checked snapshot.
    pub async fn withdraw(&self, network: String, token: String, amount: String) -> Result<Value> {
        let (vcfg, raw, decimals) = self
            .check_transfer("withdraw", false, &network, &token, &amount)
            .await?;
        self.venue
            .withdraw(&vcfg, network.clone(), token.clone(), raw)
            .await?;
        Ok(Self::transfer_view(&network, &token, raw, decimals))
    }

    /// Halt trading. Records a halt only when not already halted; reports the
    /// halt in force either way.
    pub async fn halt(&self, reason: Option<String>) -> Result<Value> {
        let mut ledger = self.ledger.lock().await;
        if ledger.halted().is_none() {
            ledger.append(Record::Halt {
                ts: self.now,
                reason,
            })?;
        }
        Ok(json!({
            "halted": true,
            "haltReason": ledger.halted().flatten(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use aspens::commands::config::config_pb::{Chain, Configuration, Market};
    use aspens::commands::trading::arborter_pb::{self, OrderbookEntry};
    use aspens::commands::trading::book_snapshot::BookState;

    use crate::config::DepositsConfig;
    use crate::fake::FakeVenue;

    const MARKET: &str = "base::0xaaa::base::0xbbb";

    fn venue() -> FakeVenue {
        let v = FakeVenue::new(GetConfigResponse {
            config: Some(Configuration {
                chains: vec![Chain {
                    network: "base".into(),
                    architecture: "evm".into(),
                    ..Default::default()
                }],
                markets: vec![Market {
                    market_id: MARKET.into(),
                    base_chain_network: "base".into(),
                    quote_chain_network: "base".into(),
                    base_chain_token_decimals: 18,
                    quote_chain_token_decimals: 8,
                    pair_decimals: 6,
                    ..Default::default()
                }],
                session: 3,
            }),
        });
        let mut book = BookState::default();
        for (b, side, price) in [
            (1u8, arborter_pb::Side::Bid, 2_400_000u128),
            (2, arborter_pb::Side::Ask, 2_600_000),
        ] {
            book.apply(&OrderbookEntry {
                order_id: vec![b; 32],
                price: price.to_string(),
                quantity: "500000".into(),
                side: side as i32,
                market_id: MARKET.into(),
                state: arborter_pb::OrderState::Confirmed as i32,
                ..Default::default()
            });
        }
        v.set_book(MARKET, None, book);
        v
    }

    fn config(dir: &std::path::Path) -> Config {
        Config {
            stack_url: "https://venue.example".into(),
            env_file: None,
            state_dir: dir.to_path_buf(),
            allow_withdraw: false,
            settle_address_allowlist: vec![],
            deposits: DepositsConfig::default(),
            venue: None,
            markets: BTreeMap::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dropping_the_handler_mid_send_still_records_the_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let fake = venue();
        fake.push_send(SendResult::Accepted {
            order_id: format!("0x{}", "c7".repeat(32)),
            in_book: true,
            taker_fills: vec![],
        });
        // The send blocks until released, and says when it has started.
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let entered_tx = std::sync::Mutex::new(Some(entered_tx));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        fake.set_on_send(Arc::new(move || {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            // Bounded, so a handler that blocks in its own poll fails, not hangs.
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(2));
        }));

        let venue = Arc::new(fake);
        let cfg = config(dir.path());
        let markets = BTreeMap::from([(
            MARKET.to_string(),
            MarketLimits {
                pair_decimals: 6,
                max_order_qty: 1_500_000,
                max_daily_qty: 7_000_000,
                max_order_quote: 25_000_000,
                max_daily_quote: 250_000_000,
                max_deviation_bps: 100,
            },
        )]);
        let ledger = Arc::new(Mutex::new(Ledger::open(dir.path()).unwrap()));
        let attested = AtomicBool::new(false);
        let in_flight = InFlight::default();
        let writer = Writer {
            venue: &venue,
            cfg: &cfg,
            markets: &markets,
            ledger: &ledger,
            attested: &attested,
            in_flight: &in_flight,
            now: 1_700_000_000,
        };
        let place = writer.place(PlaceRequest {
            market_id: MARKET.into(),
            side: Side::Buy,
            qty: "1".into(),
            pricing: Pricing::Limit("2.5".into()),
            post_only: false,
            settle_base: None,
            settle_quote: None,
        });
        // Drive the handler until the send is in flight, then drop it.
        tokio::select! {
            _ = place => panic!("the send was supposed to block"),
            r = entered_rx => r.unwrap(),
        }
        // The send task, not the dropped handler, holds the in-flight guard.
        assert_eq!(in_flight.count(), 1);
        release_tx.send(()).unwrap();
        assert!(in_flight.wait_idle(std::time::Duration::from_secs(5)).await);

        // The task still holds the ledger until it records the outcome.
        let ledger = tokio::time::timeout(std::time::Duration::from_secs(5), ledger.lock())
            .await
            .expect("the send task never finished");
        assert_eq!(
            ledger.usage(&utc_day(1_700_000_000), MARKET),
            (1_000_000, 2_500_000)
        );
        let sent = ledger.sent();
        assert_eq!(sent.len(), 1);
        assert!(matches!(sent[0].outcome, SentOutcome::Accepted { .. }));
    }
}
