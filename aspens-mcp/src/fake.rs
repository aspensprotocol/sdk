//! A scriptable, recording [`Venue`] for tests: no network.
//!
//! Every call is appended to a [`Call`] log, so a test can assert what was (and
//! was not) sent. Clones share state.
//!
//! The order-status reads answer from a store of the venue's own `MyOrder`
//! records ([`FakeVenue::set_order`]), run through the same conversion the
//! real venue's answers go through. A successful cancel marks a stored order
//! cancelled, as the venue would.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::trading::arborter_pb::{MyOrder, MyOrderState};
use aspens::commands::trading::balance::Balances;
use aspens::commands::trading::book_snapshot::BookState;
use aspens::health::PingResult;
use eyre::{Result, eyre};

use crate::ledger::Side;
use crate::orders::{VenueOrder, order_id_hex};
use crate::venue::{
    Addresses, AttestationView, CancelResult, LimitOrder, SendResult, Venue, venue_orders,
};

/// One recorded call. `Config` reads are not recorded. A write records
/// `config_session`: the `session` of the config snapshot it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Book {
        market_id: String,
    },
    /// A prepared limit order, signed and sent. Preparing alone records
    /// nothing: nothing is signed until the send.
    SendLimit {
        config_session: u64,
        market_id: String,
        side: Side,
        qty: String,
        price: String,
        post_only: bool,
        hidden: bool,
        settle_base: Option<String>,
        settle_quote: Option<String>,
    },
    GetMyOrders {
        ids: Vec<[u8; 32]>,
    },
    ListMyOrders {
        market_id: Option<String>,
        include_closed: bool,
    },
    Cancel {
        market_id: String,
        side: Side,
        order_id: [u8; 32],
    },
    Deposit {
        config_session: u64,
        network: String,
        token_symbol: String,
        amount: u128,
    },
    Withdraw {
        config_session: u64,
        network: String,
        token_symbol: String,
        amount: u128,
    },
    Balances,
    Health,
    Attestation,
}

/// Runs inside `send_prepared`, before its result is returned.
pub type SendHook = Arc<dyn Fn() + Send + Sync>;

/// What [`FakeVenue::prepare_limit`] hands to `send_prepared`.
#[derive(Debug)]
pub struct FakePrepared {
    config_session: u64,
    order: LimitOrder,
}

struct State {
    config: GetConfigResponse,
    books: HashMap<String, BookState>,
    /// The venue's order-status records, by id.
    orders: BTreeMap<[u8; 32], MyOrder>,
    /// Ids the next preparations derive, in order.
    derived_ids: VecDeque<[u8; 32]>,
    prepared: u8,
    sends: VecDeque<SendResult>,
    cancels: VecDeque<Result<CancelResult, String>>,
    balances: Balances,
    addresses: Addresses,
    healthy: bool,
    health_error: Option<String>,
    attestation: AttestationView,
    on_send: Option<SendHook>,
    on_attestation: Option<SendHook>,
    read_error: Option<String>,
    config_error: Option<String>,
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
            orders: BTreeMap::new(),
            derived_ids: VecDeque::new(),
            prepared: 0,
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
            on_attestation: None,
            read_error: None,
            config_error: None,
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

    pub fn set_book(&self, market_id: &str, book: BookState) {
        self.lock().books.insert(market_id.to_string(), book);
    }

    /// Store (or replace) the venue's status record of one of the agent's
    /// orders, keyed by its 32-byte `order_id`.
    pub fn set_order(&self, order: MyOrder) {
        let id = <[u8; 32]>::try_from(order.order_id.as_slice())
            .expect("FakeVenue::set_order needs a 32-byte order id");
        self.lock().orders.insert(id, order);
    }

    /// The id the next `prepare_limit` derives. Unqueued, it is the id of the
    /// next queued send when that is `Accepted` (the venue derives the same
    /// id), else a fresh one starting `0xde`.
    pub fn push_derived_id(&self, id: [u8; 32]) {
        self.lock().derived_ids.push_back(id);
    }

    /// Queue the result of the next `send_prepared`.
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

    /// Run `hook` inside `attestation`, before it returns (lock released).
    pub fn set_on_attestation(&self, hook: SendHook) {
        self.lock().on_attestation = Some(hook);
    }

    /// Make `config` return `Err(msg)`; `None` restores it.
    pub fn set_config_error(&self, msg: Option<&str>) {
        self.lock().config_error = msg.map(str::to_string);
    }

    /// Make every read that can fail (`book`, the order-status reads,
    /// `balances`, `attestation`) return `Err(msg)`; `None` restores normal
    /// reads.
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

/// The `session` of a config snapshot: the identity a write records.
fn session(cfg: &GetConfigResponse) -> u64 {
    cfg.config.as_ref().map_or(0, |c| c.session)
}

impl Venue for FakeVenue {
    async fn config(&self) -> Result<GetConfigResponse> {
        let s = self.lock();
        match &s.config_error {
            Some(m) => Err(eyre!(m.clone())),
            None => Ok(s.config.clone()),
        }
    }

    async fn book(&self, market_id: String) -> Result<BookState> {
        self.record(Call::Book {
            market_id: market_id.clone(),
        });
        self.read_result()?;
        Ok(self
            .lock()
            .books
            .get(&market_id)
            .cloned()
            .unwrap_or_default())
    }

    type Prepared = FakePrepared;

    fn prepare_limit(
        &self,
        cfg: &GetConfigResponse,
        order: LimitOrder,
    ) -> Result<(String, FakePrepared)> {
        let mut s = self.lock();
        let next_accepted = match s.sends.front() {
            Some(SendResult::Accepted { order_id, .. }) => Some(order_id.clone()),
            _ => None,
        };
        let id = match s.derived_ids.pop_front() {
            Some(id) => order_id_hex(&id),
            None => next_accepted.unwrap_or_else(|| {
                s.prepared = s.prepared.wrapping_add(1);
                let mut id = [0u8; 32];
                id[0] = 0xde;
                id[1] = s.prepared;
                order_id_hex(&id)
            }),
        };
        Ok((
            id,
            FakePrepared {
                config_session: session(cfg),
                order,
            },
        ))
    }

    async fn send_prepared(&self, prepared: FakePrepared) -> SendResult {
        let FakePrepared {
            config_session,
            order,
        } = prepared;
        self.record(Call::SendLimit {
            config_session,
            market_id: order.market_id,
            side: order.side,
            qty: order.qty,
            price: order.price,
            post_only: order.post_only,
            hidden: order.hidden,
            settle_base: order.settle_base,
            settle_quote: order.settle_quote,
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

    async fn orders_by_id(&self, ids: Vec<[u8; 32]>) -> Result<Vec<VenueOrder>> {
        self.record(Call::GetMyOrders { ids: ids.clone() });
        self.read_result()?;
        let s = self.lock();
        // Answered as the venue answers: one entry per id, `found: false`
        // for an id it has no record of.
        let entries: Vec<MyOrder> = ids
            .iter()
            .map(|id| {
                s.orders.get(id).cloned().unwrap_or_else(|| MyOrder {
                    order_id: id.to_vec(),
                    ..Default::default()
                })
            })
            .collect();
        venue_orders(&entries)
    }

    async fn list_orders(
        &self,
        market_id: Option<String>,
        include_closed: bool,
    ) -> Result<Vec<VenueOrder>> {
        self.record(Call::ListMyOrders {
            market_id: market_id.clone(),
            include_closed,
        });
        self.read_result()?;
        let open = [
            MyOrderState::Open as i32,
            MyOrderState::PartiallyFilled as i32,
        ];
        let entries: Vec<MyOrder> = self
            .lock()
            .orders
            .values()
            .filter(|o| market_id.as_ref().is_none_or(|m| &o.market_id == m))
            .filter(|o| include_closed || open.contains(&o.state))
            .cloned()
            .collect();
        venue_orders(&entries)
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
        let mut s = self.lock();
        let r = match s.cancels.pop_front() {
            None => Ok(CancelResult::Cancelled),
            Some(r) => r.map_err(|m| eyre!(m)),
        };
        if let (Ok(CancelResult::Cancelled), Some(o)) = (&r, s.orders.get_mut(&order_id)) {
            o.state = MyOrderState::Cancelled as i32;
        }
        r
    }

    async fn deposit(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> Result<()> {
        self.record(Call::Deposit {
            config_session: session(cfg),
            network,
            token_symbol,
            amount,
        });
        Ok(())
    }

    async fn withdraw(
        &self,
        cfg: &GetConfigResponse,
        network: String,
        token_symbol: String,
        amount: u128,
    ) -> Result<()> {
        self.record(Call::Withdraw {
            config_session: session(cfg),
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
        let hook = self.lock().on_attestation.clone();
        if let Some(hook) = hook {
            hook();
        }
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

    fn limit(qty: &str) -> LimitOrder {
        LimitOrder {
            market_id: "m".to_string(),
            side: Side::Buy,
            qty: qty.to_string(),
            price: "2.5".to_string(),
            post_only: true,
            hidden: true,
            settle_base: None,
            settle_quote: Some("0xq".to_string()),
        }
    }

    async fn send(v: &FakeVenue, qty: &str) -> SendResult {
        let (_, p) = v
            .prepare_limit(&GetConfigResponse::default(), limit(qty))
            .unwrap();
        v.send_prepared(p).await
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
                config_session: 0,
                market_id: "m".to_string(),
                side: Side::Buy,
                qty: "3".to_string(),
                price: "2.5".to_string(),
                post_only: true,
                hidden: true,
                settle_base: None,
                settle_quote: Some("0xq".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn derived_ids_are_scripted_or_follow_the_accepted_send() {
        let v = FakeVenue::default();
        let cfg = GetConfigResponse::default();
        v.push_derived_id([0x11; 32]);
        v.push_send(SendResult::Accepted {
            order_id: format!("0x{}", "22".repeat(32)),
            in_book: true,
            taker_fills: vec![],
        });
        let (first, _) = v.prepare_limit(&cfg, limit("1")).unwrap();
        assert_eq!(first, format!("0x{}", "11".repeat(32)));
        let (second, _) = v.prepare_limit(&cfg, limit("1")).unwrap();
        assert_eq!(second, format!("0x{}", "22".repeat(32)));
        // Preparing records nothing: nothing was signed.
        assert!(v.calls().is_empty());
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

    fn stored(b: u8, market: &str, state: MyOrderState) -> MyOrder {
        MyOrder {
            order_id: vec![b; 32],
            found: true,
            market_id: market.into(),
            side: aspens::commands::trading::arborter_pb::Side::Bid as i32,
            initial_qty_known: true,
            initial_qty: "9".into(),
            filled_qty: "0".into(),
            remaining_qty: "9".into(),
            state: state as i32,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn order_status_reads_answer_from_the_store() {
        let v = FakeVenue::default();
        v.set_order(stored(1, "m", MyOrderState::Open));
        v.set_order(stored(2, "m", MyOrderState::Filled));
        v.set_order(stored(3, "n", MyOrderState::PartiallyFilled));

        let got = v.orders_by_id(vec![[2; 32], [7; 32]]).await.unwrap();
        assert_eq!(got.len(), 1, "an unknown id is absent");
        assert_eq!(got[0].order_id, format!("0x{}", "02".repeat(32)));

        let ids = |os: Vec<VenueOrder>| os.into_iter().map(|o| o.order_id).collect::<Vec<_>>();
        assert_eq!(
            ids(v.list_orders(None, false).await.unwrap()),
            vec![order_id_hex(&[1; 32]), order_id_hex(&[3; 32])]
        );
        assert_eq!(
            ids(v.list_orders(Some("m".into()), true).await.unwrap()),
            vec![order_id_hex(&[1; 32]), order_id_hex(&[2; 32])]
        );
        assert_eq!(
            v.calls()[0],
            Call::GetMyOrders {
                ids: vec![[2; 32], [7; 32]]
            }
        );

        // A successful cancel closes the stored order.
        v.cancel("m".into(), Side::Buy, [1; 32]).await.unwrap();
        assert!(ids(v.list_orders(Some("m".into()), false).await.unwrap()).is_empty());

        v.set_read_error(Some("down"));
        assert!(v.orders_by_id(vec![[1; 32]]).await.is_err());
        assert!(v.list_orders(None, true).await.is_err());
    }

    #[tokio::test]
    async fn scripted_cancels_and_health() {
        let v = FakeVenue::default();
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
