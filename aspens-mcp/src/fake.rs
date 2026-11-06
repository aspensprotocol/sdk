//! A scriptable, recording [`Venue`] for tests: no network.
//!
//! Every call is appended to a [`Call`] log, so a test can assert what was (and
//! was not) sent. Clones share state.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::balance::Balances;
use aspens::commands::trading::book_snapshot::BookState;
use aspens::health::PingResult;
use eyre::{Result, eyre};

use crate::ledger::Side;
use crate::venue::{Addresses, AttestationView, CancelResult, SendResult, TradeFill, Venue};

/// One recorded call. `Config` reads are not recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Book {
        market_id: String,
        filter_by_trader: Option<String>,
    },
    MyFills {
        market_id: String,
        trader: String,
    },
    SendLimit {
        market_id: String,
        side: Side,
        qty: String,
        price: String,
        post_only: bool,
        settle_base: Option<String>,
        settle_quote: Option<String>,
    },
    Cancel {
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    },
    Deposit {
        network: String,
        token_symbol: String,
        amount: u128,
    },
    Withdraw {
        network: String,
        token_symbol: String,
        amount: u128,
    },
    Balances,
    Health,
    Attestation,
}

/// Runs inside `send_limit`, before its result is returned.
pub type SendHook = Arc<dyn Fn() + Send + Sync>;

struct State {
    config: GetConfigResponse,
    books: HashMap<(String, Option<String>), BookState>,
    fills: HashMap<(String, String), Vec<TradeFill>>,
    sends: VecDeque<SendResult>,
    cancels: VecDeque<Result<CancelResult, String>>,
    balances: Balances,
    addresses: Addresses,
    healthy: bool,
    health_error: Option<String>,
    attestation: AttestationView,
    on_send: Option<SendHook>,
    read_error: Option<String>,
    calls: Vec<Call>,
}

#[derive(Clone)]
pub struct FakeVenue {
    state: Arc<Mutex<State>>,
}

impl Default for FakeVenue {
    fn default() -> Self {
        Self::new(GetConfigResponse::default())
    }
}

impl FakeVenue {
    /// A healthy venue with `config`, empty books and fills, no scripted sends
    /// (an unscripted send is `Failed { code: "unscripted" }`), and cancels that
    /// succeed unless scripted otherwise.
    pub fn new(config: GetConfigResponse) -> Self {
        let state = State {
            config,
            books: HashMap::new(),
            fills: HashMap::new(),
            sends: VecDeque::new(),
            cancels: VecDeque::new(),
            balances: Balances {
                tokens: vec![],
                native: vec![],
            },
            addresses: Addresses::default(),
            healthy: true,
            health_error: None,
            attestation: AttestationView {
                report: serde_json::Value::Null,
                verified: None,
                note: None,
            },
            on_send: None,
            read_error: None,
            calls: Vec::new(),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_config(&self, config: GetConfigResponse) {
        self.lock().config = config;
    }

    pub fn set_book(&self, market_id: &str, filter_by_trader: Option<&str>, book: BookState) {
        let key = (market_id.to_string(), filter_by_trader.map(str::to_string));
        self.lock().books.insert(key, book);
    }

    pub fn set_fills(&self, market_id: &str, trader: &str, fills: Vec<TradeFill>) {
        let key = (market_id.to_string(), trader.to_string());
        self.lock().fills.insert(key, fills);
    }

    /// Queue the result of the next `send_limit`.
    pub fn push_send(&self, r: SendResult) {
        self.lock().sends.push_back(r);
    }

    /// Queue the result of the next `cancel`; `Err(msg)` makes it fail.
    pub fn push_cancel(&self, r: Result<CancelResult, String>) {
        self.lock().cancels.push_back(r);
    }

    pub fn set_balances(&self, balances: Balances) {
        self.lock().balances = balances;
    }

    pub fn set_addresses(&self, addresses: Addresses) {
        self.lock().addresses = addresses;
    }

    pub fn set_healthy(&self, healthy: bool) {
        self.lock().healthy = healthy;
    }

    /// Make `health` fail with `msg` as its error text.
    pub fn set_health_error(&self, msg: &str) {
        let mut s = self.lock();
        s.healthy = false;
        s.health_error = Some(msg.to_string());
    }

    pub fn set_attestation(&self, view: AttestationView) {
        self.lock().attestation = view;
    }

    pub fn set_on_send(&self, hook: SendHook) {
        self.lock().on_send = Some(hook);
    }

    /// Make every read that can fail (`book`, `my_fills`, `balances`,
    /// `attestation`) return `Err(msg)`; `None` restores normal reads.
    pub fn set_read_error(&self, msg: Option<&str>) {
        self.lock().read_error = msg.map(str::to_string);
    }

    fn read_result(&self) -> Result<()> {
        match &self.lock().read_error {
            Some(m) => Err(eyre!(m.clone())),
            None => Ok(()),
        }
    }

    /// Every recorded call, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Only the `SendLimit` calls.
    pub fn sends(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::SendLimit { .. }))
            .collect()
    }

    fn record(&self, c: Call) {
        self.lock().calls.push(c);
    }
}

impl Venue for FakeVenue {
    async fn config(&self) -> Result<GetConfigResponse> {
        Ok(self.lock().config.clone())
    }

    async fn book(&self, market_id: String, filter_by_trader: Option<String>) -> Result<BookState> {
        let key = (market_id.clone(), filter_by_trader.clone());
        self.record(Call::Book {
            market_id,
            filter_by_trader,
        });
        self.read_result()?;
        Ok(self.lock().books.get(&key).cloned().unwrap_or_default())
    }

    async fn my_fills(&self, market_id: String, trader: String) -> Result<Vec<TradeFill>> {
        let key = (market_id.clone(), trader.clone());
        self.record(Call::MyFills { market_id, trader });
        self.read_result()?;
        Ok(self.lock().fills.get(&key).cloned().unwrap_or_default())
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
        self.record(Call::SendLimit {
            market_id,
            side,
            qty,
            price,
            post_only,
            settle_base,
            settle_quote,
        });
        // Run the hook with the lock released, so it may inspect this venue.
        let hook = self.lock().on_send.clone();
        if let Some(hook) = hook {
            hook();
        }
        self.lock()
            .sends
            .pop_front()
            .unwrap_or_else(|| SendResult::Failed {
                code: "unscripted".to_string(),
                message: "FakeVenue: no SendResult queued".to_string(),
            })
    }

    async fn cancel(
        &self,
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    ) -> Result<CancelResult> {
        self.record(Call::Cancel {
            market_id,
            side,
            order_id,
        });
        match self.lock().cancels.pop_front() {
            None => Ok(CancelResult::Cancelled),
            Some(r) => r.map_err(|m| eyre!(m)),
        }
    }

    async fn deposit(&self, network: String, token_symbol: String, amount: u128) -> Result<()> {
        self.record(Call::Deposit {
            network,
            token_symbol,
            amount,
        });
        Ok(())
    }

    async fn withdraw(&self, network: String, token_symbol: String, amount: u128) -> Result<()> {
        self.record(Call::Withdraw {
            network,
            token_symbol,
            amount,
        });
        Ok(())
    }

    async fn balances(&self) -> Result<Balances> {
        self.record(Call::Balances);
        self.read_result()?;
        Ok(self.lock().balances.clone())
    }

    async fn health(&self) -> PingResult {
        self.record(Call::Health);
        let (healthy, error) = {
            let s = self.lock();
            (s.healthy, s.health_error.clone())
        };
        PingResult {
            success: healthy,
            url: "fake://venue".to_string(),
            latency_ms: healthy.then_some(1),
            error: (!healthy).then(|| error.unwrap_or_else(|| "FakeVenue: unhealthy".to_string())),
        }
    }

    async fn attestation(&self, _nonce: Vec<u8>) -> Result<AttestationView> {
        self.record(Call::Attestation);
        self.read_result()?;
        Ok(self.lock().attestation.clone())
    }

    fn addresses(&self) -> Addresses {
        self.lock().addresses.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn send(v: &FakeVenue, qty: &str) -> SendResult {
        v.send_limit(
            "m".to_string(),
            Side::Buy,
            qty.to_string(),
            "2.5".to_string(),
            true,
            None,
            Some("0xq".to_string()),
        )
        .await
    }

    #[tokio::test]
    async fn sends_pop_in_order_and_are_recorded() {
        let v = FakeVenue::default();
        let accepted = SendResult::Accepted {
            order_id: format!("0x{}", "ab".repeat(32)),
            in_book: true,
            taker_fills: vec![],
        };
        v.push_send(accepted.clone());
        v.push_send(SendResult::Unknown);

        assert_eq!(send(&v, "1").await, accepted);
        assert_eq!(send(&v, "3").await, SendResult::Unknown);
        assert!(
            matches!(send(&v, "4").await, SendResult::Failed { code, .. } if code == "unscripted")
        );

        let sends = v.sends();
        assert_eq!(sends.len(), 3);
        assert_eq!(
            sends[1],
            Call::SendLimit {
                market_id: "m".to_string(),
                side: Side::Buy,
                qty: "3".to_string(),
                price: "2.5".to_string(),
                post_only: true,
                settle_base: None,
                settle_quote: Some("0xq".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn on_send_hook_runs_and_clones_share_state() {
        let v = FakeVenue::default();
        let seen = Arc::new(AtomicUsize::new(0));
        let probe = v.clone();
        let s = seen.clone();
        // The hook can read the venue: the lock is not held while it runs.
        v.set_on_send(Arc::new(move || {
            s.store(probe.calls().len(), Ordering::SeqCst);
        }));
        v.push_send(SendResult::Unknown);
        let _ = v.balances().await;
        send(&v, "1").await;
        // Balances + the SendLimit itself were recorded before the hook ran.
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn scripted_reads_and_cancels() {
        let v = FakeVenue::default();
        let fill = TradeFill {
            order_hit: None,
            qty: 7,
            price: 11,
            ts: 13,
            maker: true,
        };
        v.set_fills("m", "0xa", vec![fill.clone()]);
        assert_eq!(
            v.my_fills("m".into(), "0xa".into()).await.unwrap(),
            vec![fill]
        );
        assert!(
            v.my_fills("m".into(), "0xb".into())
                .await
                .unwrap()
                .is_empty()
        );

        v.push_cancel(Ok(CancelResult::AlreadyGone));
        v.push_cancel(Err("boom".into()));
        let id = [9u8; 32];
        assert_eq!(
            v.cancel("m".into(), Side::Sell, id).await.unwrap(),
            CancelResult::AlreadyGone
        );
        assert!(v.cancel("m".into(), Side::Sell, id).await.is_err());
        assert_eq!(
            v.cancel("m".into(), Side::Sell, id).await.unwrap(),
            CancelResult::Cancelled
        );

        v.set_healthy(false);
        assert!(!v.health().await.success);
        assert_eq!(v.calls().last(), Some(&Call::Health));
    }
}
