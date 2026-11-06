//! The bodies of the write tools: the order path, cancel, deposit, withdraw and
//! halt, and the venue pin that orders, deposits and withdrawals check.
//!
//! Every check runs before anything is signed. A refusal is an error whose text
//! lists every failed check and says that nothing was signed.

use std::collections::{BTreeMap, BTreeSet};
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
    pub venue: &'a V,
    pub cfg: &'a Config,
    pub markets: &'a BTreeMap<String, MarketLimits>,
    pub ledger: &'a Mutex<Ledger>,
    /// Set once the venue's attestation has verified; a failure is never kept.
    pub attested: &'a AtomicBool,
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

    /// The order path. In order: parse amounts; read the book; check the venue
    /// pin; then, holding the ledger, check the limits, record the usage, send,
    /// and record the outcome. The ledger is held from the check to the outcome
    /// record, so no read can see a sent order the ledger lacks, and two orders
    /// cannot both pass on the same usage.
    pub async fn place(&self, req: PlaceRequest) -> Result<Value> {
        let what = "order";
        let Some(limits) = self.markets.get(&req.market_id) else {
            let mut rs = Vec::new();
            if let Some(h) = self.ledger.lock().await.halted() {
                rs.push(Refusal::Halted(h));
            }
            rs.push(Refusal::MarketNotTradable(req.market_id.clone()));
            return Err(refused(what, &refusal_texts(&rs)));
        };
        let d = limits.pair_decimals;

        // 1. Amounts, rejecting any precision the market cannot carry.
        let qty = parse_amount(&req.qty, d)
            .map_err(|e| refused(what, &[format!("qty {:?}: {e:#}", req.qty)]))?;
        // `Ok(price)` for a limit order; `Err(slippage_bps)` until the book is read.
        let limit_price: std::result::Result<u128, u32> =
            match &req.pricing {
                Pricing::Limit(p) => Ok(parse_amount(p, d)
                    .map_err(|e| refused(what, &[format!("price {p:?}: {e:#}")]))?),
                Pricing::Marketable { slippage_bps } => Err(*slippage_bps),
            };

        // 2. One book read: the price guard's reference, and a marketable price.
        let book = self.venue.book(req.market_id.clone(), None).await?;
        let top = BookTop {
            best_bid: book.best_bid(),
            best_ask: book.best_ask(),
        };
        let price = match limit_price {
            Ok(p) => p,
            Err(slippage_bps) => marketable_price(&book, req.side == Side::Buy, slippage_bps)
                .map_err(|e| refused(what, &[format!("{e:#}")]))?,
        };

        // 3. The venue pin, on both legs' networks.
        let pin = if self.cfg.venue.is_some() {
            let vcfg = self.venue.config().await?;
            let m = vcfg
                .get_market_by_id(&req.market_id)
                .ok_or_else(|| eyre!("no such market in the venue config: {}", req.market_id))?;
            let networks = [
                m.base_chain_network.as_str(),
                m.quote_chain_network.as_str(),
            ];
            self.check_pin(&vcfg, &networks).await
        } else {
            Vec::new()
        };

        // 4. The limits, with the ledger held through step 7.
        let mut ledger = self.ledger.lock().await;
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

        // 5. Usage, durable before anything is signed.
        ledger
            .append(Record::Usage {
                ts: self.now,
                day,
                market_id: req.market_id.clone(),
                side: req.side,
                qty: commit.qty,
                quote: commit.quote,
            })
            .map_err(|e| eyre!("recording usage failed, nothing was signed: {e:#}"))?;

        // 6. Send. The SDK takes human strings and scales them by pair decimals.
        let result = self
            .venue
            .send_limit(
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
            ts: self.now,
            market_id: req.market_id.clone(),
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
        drop(ledger);

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
    fn transfer_amount(what: &str, amount: &str, decimals: u32) -> Result<u128> {
        let raw = parse_amount(amount, decimals)
            .map_err(|e| refused(what, &[format!("amount {amount:?}: {e:#}")]))?;
        if raw == 0 {
            return Err(refused(what, &["amount must be greater than zero".into()]));
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

    /// Deposit from the agent's wallet. Checks, in order: halted,
    /// `deposits.enabled`, `max_per_call`, the venue pin. Not usage.
    pub async fn deposit(&self, network: String, token: String, amount: String) -> Result<Value> {
        let what = "deposit";
        let vcfg = self.venue.config().await?;
        let (address, decimals) =
            Self::token(&vcfg, &network, &token).map_err(|e| refused(what, &[format!("{e:#}")]))?;
        let raw = Self::transfer_amount(what, &amount, decimals)?;

        let mut rs = Vec::new();
        if let Some(h) = self.ledger.lock().await.halted() {
            rs.push(Refusal::Halted(h).to_string());
        }
        if !self.cfg.deposits.enabled {
            rs.push("deposits are disabled ([deposits] enabled = false)".to_string());
        }
        match self.max_per_call(&network, &address, decimals) {
            Ok(Some(max)) if raw > max => rs.push(format!(
                "max_per_call exceeded: amount {raw} > limit {max} for {network}:{address}"
            )),
            Ok(_) => {}
            Err(e) => rs.push(format!("{e:#}")),
        }
        rs.extend(refusal_texts(&self.check_pin(&vcfg, &[&network]).await));
        if !rs.is_empty() {
            return Err(refused(what, &rs));
        }
        self.venue
            .deposit(network.clone(), token.clone(), raw)
            .await?;
        Ok(Self::transfer_view(&network, &token, raw, decimals))
    }

    /// Withdraw to the agent's own address (the SDK pays the signing wallet).
    /// Checks: halted, the venue pin.
    pub async fn withdraw(&self, network: String, token: String, amount: String) -> Result<Value> {
        let what = "withdraw";
        let vcfg = self.venue.config().await?;
        let (_, decimals) =
            Self::token(&vcfg, &network, &token).map_err(|e| refused(what, &[format!("{e:#}")]))?;
        let raw = Self::transfer_amount(what, &amount, decimals)?;

        let mut rs = Vec::new();
        if let Some(h) = self.ledger.lock().await.halted() {
            rs.push(Refusal::Halted(h).to_string());
        }
        rs.extend(refusal_texts(&self.check_pin(&vcfg, &[&network]).await));
        if !rs.is_empty() {
            return Err(refused(what, &rs));
        }
        self.venue
            .withdraw(network.clone(), token.clone(), raw)
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
