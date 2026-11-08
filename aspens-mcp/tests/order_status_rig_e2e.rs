//! Owner-signed order status end to end on the local rig: `GetMyOrders` and
//! `ListMyOrders` read through the `aspens` SDK, plus aspens-mcp's
//! `get_order` over stdio. The arborter must be built from a branch that
//! serves the order-status RPCs.
//!
//! Every test is `#[ignore]` and also requires `RIG=1`. The tests take one
//! global lock, so they run one at a time whatever `--test-threads` says:
//!
//! ```text
//! RIG=1 \
//! ASPENS_MCP_RIG_MARKET='<market_id from aspens-cli config>' \
//! ASPENS_MCP_RIG_TRADER_ENV=/path/to/sdk/.env.local.1 \
//! ASPENS_MCP_RIG_TRADER2_ENV=/path/to/sdk/.env.local.3 \
//! ASPENS_MCP_RIG_COUNTER_ENV=/path/to/sdk/.env.local.2 \
//!   cargo test -p aspens-mcp --test order_status_rig_e2e -- --ignored --nocapture
//! ```
//!
//! The rig is the evm-evm-local scenario (recipe: `otc/e2e/README.md`): two
//! anvil forks of Coston2, both chain id 114, so chains are addressed by RPC
//! URL. Only anvil dev keys belong in the env files; this suite only ever runs
//! against a local rig.
//!
//! Scenario 6 restarts the arborter with `docker service update --force
//! local_arborter` (the swarm service name can be changed with
//! `ASPENS_MCP_RIG_ARBORTER_SERVICE`). A restart drops every resting order on
//! the venue, by design.
//!
//! # Env vars read
//!
//! - `RIG` — must be `1`.
//! - `ASPENS_MCP_RIG_MARKET` — the market id to trade, exactly as the venue
//!   lists it.
//! - `ASPENS_MCP_RIG_TRADER_ENV` — env file of the trader (`TRADER_PRIVKEY`
//!   and `ASPENS_RPC_URL_<NETWORK>` for both chains). aspens-mcp is driven
//!   with a 0600 copy of it.
//! - `ASPENS_MCP_RIG_TRADER2_ENV` — env file of the trader's second wallet,
//!   used in scenario 7; only its `TRADER_PRIVKEY` is read.
//! - `ASPENS_MCP_RIG_COUNTER_ENV` — env file of the crossing counterparty;
//!   only its `TRADER_PRIVKEY` is read.
//! - `ASPENS_MCP_RIG_STACK` — the venue's native gRPC endpoint. Default: the
//!   trader env file's `ASPENS_MARKET_STACK_URL`, else `http://localhost:50051`.
//!
//! # Funding
//!
//! The trader needs quote token on the venue (its buys) and base token on the
//! venue (scenario 7's sell); the trader's second wallet needs quote token on
//! the venue (scenario 7's buy); the counterparty needs base token on the
//! venue (its sells). One run sells 7 base units of the counterparty's; top it up
//! between batches of runs or its sells fail with "insufficient balance".
//!
//! # Values
//!
//! A resting order's initial quantity, the crossed quantity and the remainder
//! all differ, and the counterparty crosses at a price below the maker's, so
//! a fill reported at the taker's price is caught.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::config::get_config;
use aspens::commands::trading::arborter_pb::{
    FillRole, MyOrder, MyOrderState, MyOrdersQuery, MyOrdersRequest, MyOrdersResponse,
    Side as WireSide,
};
use aspens::commands::trading::book_snapshot::fetch_book_snapshot;
use aspens::commands::trading::cancel_order::call_cancel_order_from_config_with_wallet;
use aspens::commands::trading::send_order::send_order_with_wallets;
use aspens::decimals::format_decimal_amount;
use aspens::{
    Architecture, DelegateKey, ReadSession, Wallet, build_my_orders_request,
    create_read_delegation_with_wallet, get_my_orders_from_config_with_wallet,
    list_my_orders_from_config_with_wallet, open_read_session_with_wallets,
};
use bytes::{Buf, BufMut};
use prost::Message;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

const HELP: &str = "the order-status rig e2e needs RIG=1, ASPENS_MCP_RIG_MARKET, \
                    ASPENS_MCP_RIG_TRADER_ENV, ASPENS_MCP_RIG_TRADER2_ENV and \
                    ASPENS_MCP_RIG_COUNTER_ENV; see the header of \
                    aspens-mcp/tests/order_status_rig_e2e.rs";

/// Every scenario places orders on one shared book and scenario 6 restarts
/// the venue, so they never overlap.
static RIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long a delegation made by this suite lasts.
const TTL: Duration = Duration::from_secs(3_600);

/// Set once the pre-flight sweep has run in this process.
static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Printed once per process when the market cannot catch a scaling bug.
static DECIMALS_WARNING: std::sync::Once = std::sync::Once::new();

/// The most orders the sweep or the panic guard will cancel, so neither can
/// run unbounded against a book that is not what this suite expects.
const MAX_CLEANUP: usize = 64;

/// One cancel attempt's deadline in the sweep and the panic guard.
const CANCEL_DEADLINE: Duration = Duration::from_secs(10);

// ---- rig ----

struct Rig {
    stack: String,
    market: String,
    trader_env: String,
    trader: Wallet,
    trader2: Wallet,
    counter: Wallet,
    /// The three wallets' keys, in the order trader, trader2, counter, so the
    /// panic guard can rebuild them on its own thread.
    keys: [String; 3],
    /// Every order this test placed: `(wallet index, side, id)`. Cancelled
    /// best-effort if the test panics.
    placed: std::sync::Mutex<Vec<(usize, &'static str, [u8; 32])>>,
    /// Pair decimals of the market.
    d: u32,
    base_network: String,
    quote_network: String,
}

fn env_file_vars(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: {HELP}"))
}

fn wallet_from(path: &str) -> (Wallet, String, BTreeMap<String, String>, String) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let vars = env_file_vars(&text);
    let key = vars
        .get("TRADER_PRIVKEY")
        .unwrap_or_else(|| panic!("{path} has no TRADER_PRIVKEY"))
        .clone();
    (
        Wallet::from_evm_hex(&key).expect("TRADER_PRIVKEY"),
        key,
        vars,
        text,
    )
}

fn side_name(side: i32) -> &'static str {
    if side == WireSide::Ask as i32 {
        "sell"
    } else {
        "buy"
    }
}

/// Cancel `orders` (`(wallet, market, side, id)`), each bounded by
/// [`CANCEL_DEADLINE`], logging rather than failing. An order that is already
/// closed answers NOT_FOUND, which is fine.
async fn cancel_best_effort(stack: &str, orders: &[(&Wallet, String, &str, [u8; 32])]) {
    let Ok(cfg) = get_config(stack.to_string()).await else {
        eprintln!(
            "    cleanup: GetConfig failed; {} orders left",
            orders.len()
        );
        return;
    };
    for (wallet, market, side, id) in orders.iter().take(MAX_CLEANUP) {
        let r = tokio::time::timeout(
            CANCEL_DEADLINE,
            call_cancel_order_from_config_with_wallet(
                stack.to_string(),
                market.clone(),
                side.to_string(),
                *id,
                wallet,
                cfg.clone(),
            ),
        )
        .await;
        let outcome = match r {
            Ok(Ok(_)) => "cancelled".to_string(),
            Ok(Err(e)) => format!("not cancelled ({e:#})"),
            Err(_) => "timed out".to_string(),
        };
        eprintln!(
            "    cleanup: {side} {} of {}: {outcome}",
            hex::encode(id),
            wallet.address()
        );
    }
}

/// A loud notice for a global test-value constraint the rig cannot meet.
fn warn_unmet_decimals(pair: i64, base: (String, u32), quote: (String, u32)) {
    let p = u32::try_from(pair).unwrap_or(u32::MAX);
    if p != base.1 && p != quote.1 {
        return;
    }
    DECIMALS_WARNING.call_once(|| {
        let bar = "!".repeat(78);
        eprintln!(
            "\n{bar}\n\
             WARNING: UNMET TEST-VALUE CONSTRAINT \"pair decimals != either token's decimals\"\n\
             The market's pair decimals are {pair}; {} has {} and {} has {}.\n\
             Pair units equal token units here, so a status amount scaled by the wrong\n\
             decimals reads the same and this suite CANNOT catch a scaling bug.\n\
             (Not a failure: the rig has no market that meets it. Open item.)\n{bar}\n",
            base.0, base.1, quote.0, quote.1
        );
    });
}

/// Panic unless `url`'s host is loopback.
fn assert_loopback(what: &str, url: &str) {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    let ok = matches!(
        host.as_deref(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    assert!(
        ok,
        "refusing to run the rig e2e: {what} is {url:?}, whose host is not loopback. This \
         suite sends real orders; point it at a local rig only"
    );
}

async fn venue_config(stack: &str) -> GetConfigResponse {
    // The rig's endpoint intermittently answers UNAVAILABLE; reads are retried.
    let mut last = None;
    for _ in 0..5 {
        match get_config(stack.to_string()).await {
            Ok(c) => return c,
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("GetConfig at {stack} failed: {:#}", last.unwrap());
}

impl Rig {
    async fn load() -> Rig {
        assert_eq!(std::env::var("RIG").as_deref(), Ok("1"), "{HELP}");
        let market = required("ASPENS_MCP_RIG_MARKET");
        let (trader, k1, tvars, trader_env) = wallet_from(&required("ASPENS_MCP_RIG_TRADER_ENV"));
        let (trader2, k2, _, _) = wallet_from(&required("ASPENS_MCP_RIG_TRADER2_ENV"));
        let (counter, k3, _, _) = wallet_from(&required("ASPENS_MCP_RIG_COUNTER_ENV"));
        let addrs = [trader.address(), trader2.address(), counter.address()];
        assert!(
            addrs[0] != addrs[1] && addrs[1] != addrs[2] && addrs[0] != addrs[2],
            "the trader, its second wallet and the counterparty must be three wallets: {addrs:?}"
        );
        let stack = std::env::var("ASPENS_MCP_RIG_STACK")
            .ok()
            .or_else(|| tvars.get("ASPENS_MARKET_STACK_URL").cloned())
            .unwrap_or_else(|| "http://localhost:50051".to_string());
        assert_loopback("the stack URL", &stack);
        for (k, v) in tvars
            .iter()
            .filter(|(k, _)| k.starts_with("ASPENS_RPC_URL_"))
        {
            assert_loopback(k, v);
        }
        let cfg = venue_config(&stack).await;
        let m = cfg
            .get_market_by_id(&market)
            .unwrap_or_else(|| panic!("market {market} is not in the venue config"))
            .clone();
        assert_ne!(
            m.base_chain_network, m.quote_chain_network,
            "scenario 7 needs a market whose legs are on two networks"
        );
        let token_decimals = |network: &str, symbol: &str| {
            let t = cfg
                .get_token(network, symbol)
                .unwrap_or_else(|| panic!("{symbol}@{network} is not in the venue config"));
            (format!("{symbol}@{network}"), t.decimals)
        };
        warn_unmet_decimals(
            m.pair_decimals.into(),
            token_decimals(&m.base_chain_network, &m.base_chain_token_symbol),
            token_decimals(&m.quote_chain_network, &m.quote_chain_token_symbol),
        );
        let rig = Rig {
            stack,
            market,
            trader_env,
            trader,
            trader2,
            counter,
            keys: [k1, k2, k3],
            placed: std::sync::Mutex::new(Vec::new()),
            d: u32::try_from(m.pair_decimals).expect("pair decimals"),
            base_network: m.base_chain_network.clone(),
            quote_network: m.quote_chain_network.clone(),
        };
        if !SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
            rig.sweep().await;
        }
        rig
    }

    fn wallets(&self) -> [&Wallet; 3] {
        [&self.trader, &self.trader2, &self.counter]
    }

    /// Pre-flight: cancel every open order the suite's own three wallets have
    /// on the venue (any market), as `ListMyOrders` under each wallet's own
    /// delegation reports them, so a run that panicked earlier leaves the next
    /// one the book it expects. Touches nothing any other wallet owns.
    async fn sweep(&self) {
        let cfg = self.config().await;
        let mut open = Vec::new();
        for w in self.wallets() {
            let mut session = self.session(&[w]).await;
            let listed = list_my_orders_from_config_with_wallet(
                &self.stack,
                &cfg,
                &mut session,
                &[],
                None,
                false,
            )
            .await
            .expect("pre-flight ListMyOrders");
            for o in listed.into_iter().filter(|o| {
                o.found && matches!(state(o), MyOrderState::Open | MyOrderState::PartiallyFilled)
            }) {
                let id = <[u8; 32]>::try_from(o.order_id.as_slice()).expect("32-byte id");
                open.push((w, o.market_id.clone(), side_name(o.side), id));
            }
        }
        eprintln!(
            "    pre-flight sweep: {} open order(s) of the suite's wallets",
            open.len()
        );
        assert!(
            open.len() <= MAX_CLEANUP,
            "the suite's wallets have {} open orders; refusing to sweep more than {MAX_CLEANUP}",
            open.len()
        );
        cancel_best_effort(&self.stack, &open).await;
    }

    /// Record an order this test placed, for the panic guard.
    fn track(&self, wallet: &Wallet, side: WireSide, id: [u8; 32]) {
        let i = self
            .wallets()
            .iter()
            .position(|w| w.address() == wallet.address())
            .expect("a suite wallet");
        self.placed
            .lock()
            .unwrap()
            .push((i, side_name(side as i32), id));
    }

    /// One base unit, in pair units.
    fn unit(&self) -> u128 {
        10u128.pow(self.d)
    }

    fn s(&self, raw: u128) -> String {
        format_decimal_amount(raw, self.d)
    }

    async fn config(&self) -> GetConfigResponse {
        venue_config(&self.stack).await
    }

    /// `(best bid, best ask)` of the public book, pair units.
    async fn top(&self) -> (Option<u128>, Option<u128>) {
        let book = fetch_book_snapshot(
            self.stack.clone(),
            self.market.clone(),
            None,
            Duration::from_secs(2),
        )
        .await
        .expect("orderbook snapshot");
        (book.best_bid(), book.best_ask())
    }

    /// Send a limit order signed by `wallet`; returns its 32-byte order id.
    async fn send(
        &self,
        wallet: &Wallet,
        side: WireSide,
        qty: u128,
        price: u128,
        hidden: bool,
    ) -> [u8; 32] {
        self.send_settling(wallet, side, qty, price, hidden, None)
            .await
    }

    /// [`Rig::send`], settling the quote leg to `quote_address` when given.
    async fn send_settling(
        &self,
        wallet: &Wallet,
        side: WireSide,
        qty: u128,
        price: u128,
        hidden: bool,
        quote_address: Option<String>,
    ) -> [u8; 32] {
        let resp = send_order_with_wallets(
            self.stack.clone(),
            self.market.clone(),
            side as i32,
            self.s(qty),
            Some(self.s(price)),
            &[wallet],
            self.config().await,
            false,
            hidden,
            None,
            vec![],
            None,
            quote_address,
        )
        .await
        .unwrap_or_else(|e| panic!("send_order from {} failed: {e:#}", wallet.address()));
        let id = <[u8; 32]>::try_from(resp.order_id.as_slice()).expect("32-byte order id");
        self.track(wallet, side, id);
        id
    }

    async fn cancel(&self, wallet: &Wallet, side: &str, id: [u8; 32]) {
        call_cancel_order_from_config_with_wallet(
            self.stack.clone(),
            self.market.clone(),
            side.to_string(),
            id,
            wallet,
            self.config().await,
        )
        .await
        .unwrap_or_else(|e| panic!("cancel {} failed: {e:#}", hex::encode(id)));
    }

    /// A read session for `wallets`, one delegation per chain family.
    async fn session(&self, wallets: &[&Wallet]) -> ReadSession {
        open_read_session_with_wallets(
            &self.config().await,
            wallets,
            DelegateKey::generate().unwrap(),
            TTL,
        )
        .await
        .expect("open read session")
    }

    async fn get(&self, session: &mut ReadSession, ids: &[[u8; 32]]) -> Vec<MyOrder> {
        get_my_orders_from_config_with_wallet(&self.stack, &self.config().await, session, &[], ids)
            .await
            .expect("GetMyOrders")
    }

    async fn get_one(&self, session: &mut ReadSession, id: [u8; 32]) -> MyOrder {
        let mut v = self.get(session, &[id]).await;
        assert_eq!(
            v.len(),
            1,
            "GetMyOrders answered {} entries for one id",
            v.len()
        );
        let o = v.pop().unwrap();
        assert_eq!(
            o.order_id,
            id.to_vec(),
            "GetMyOrders answered for another id"
        );
        o
    }

    async fn list(&self, session: &mut ReadSession, include_closed: bool) -> Vec<MyOrder> {
        list_my_orders_from_config_with_wallet(
            &self.stack,
            &self.config().await,
            session,
            &[],
            Some(&self.market),
            include_closed,
        )
        .await
        .expect("ListMyOrders")
    }

    /// Poll `GetMyOrders` for `id` until `want` holds, up to 30 s.
    async fn await_order(
        &self,
        session: &mut ReadSession,
        id: [u8; 32],
        what: &str,
        want: impl Fn(&MyOrder) -> bool,
    ) -> MyOrder {
        let mut last = None;
        for _ in 0..30 {
            let o = self.get_one(session, id).await;
            if want(&o) {
                return o;
            }
            last = Some(o);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        panic!(
            "order {} never became {what}: last {last:?}",
            hex::encode(id)
        );
    }
}

/// The panic guard: if a test panics, cancel every order it placed, on a
/// thread of its own (a test's runtime cannot be re-entered from `drop`),
/// bounded by [`MAX_CLEANUP`] and [`CANCEL_DEADLINE`] per order.
impl Drop for Rig {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let placed = std::mem::take(&mut *self.placed.lock().unwrap_or_else(|e| e.into_inner()));
        if placed.is_empty() {
            return;
        }
        let (stack, market, keys) = (self.stack.clone(), self.market.clone(), self.keys.clone());
        let t = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            rt.block_on(async {
                let wallets: Vec<Wallet> = keys
                    .iter()
                    .map(|k| Wallet::from_evm_hex(k).expect("suite key"))
                    .collect();
                let orders: Vec<_> = placed
                    .iter()
                    .map(|(i, side, id)| (&wallets[*i], market.clone(), *side, *id))
                    .collect();
                eprintln!(
                    "    panic guard: cancelling {} placed order(s)",
                    orders.len()
                );
                cancel_best_effort(&stack, &orders).await;
            });
        });
        let _ = t.join();
    }
}

fn random_id() -> [u8; 32] {
    let mut id = [0u8; 32];
    getrandom::fill(&mut id).unwrap();
    id
}

fn state(o: &MyOrder) -> MyOrderState {
    MyOrderState::try_from(o.state).unwrap_or(MyOrderState::Unspecified)
}

// ---- prices ----

/// A maker bid price strictly inside the spread and above every public bid,
/// and a taker sell price strictly between that and the best public bid, so
/// the counterparty's sell can only match the trader's bid, at the maker's
/// price.
struct Prices {
    maker: u128,
    taker: u128,
}

async fn prices(rig: &Rig) -> Prices {
    let (bid, ask) = rig.top().await;
    let floor = bid.unwrap_or(0);
    let maker = match ask {
        Some(a) => floor + (a - floor) / 2,
        None => (floor + floor / 10).max(rig.unit() * 16 / 10),
    };
    let taker = maker - (maker - floor) / 3;
    assert!(
        floor < taker && taker < maker && ask.is_none_or(|a| maker < a),
        "the book ({bid:?}, {ask:?}) has no room for a maker {maker} and a taker {taker}"
    );
    Prices { maker, taker }
}

// ---- the wire, read raw ----

/// A codec that moves message bytes untouched, so a test sees exactly the
/// fields the venue encoded, unknown ones included.
#[derive(Clone, Copy, Default)]
struct RawCodec;

impl tonic::codec::Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = RawCodec;
    type Decoder = RawCodec;
    fn encoder(&mut self) -> RawCodec {
        RawCodec
    }
    fn decoder(&mut self) -> RawCodec {
        RawCodec
    }
}

impl tonic::codec::Encoder for RawCodec {
    type Item = Vec<u8>;
    type Error = tonic::Status;
    fn encode(
        &mut self,
        item: Vec<u8>,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), tonic::Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl tonic::codec::Decoder for RawCodec {
    type Item = Vec<u8>;
    type Error = tonic::Status;
    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Vec<u8>>, tonic::Status> {
        let mut v = vec![0u8; src.remaining()];
        src.copy_to_slice(&mut v);
        Ok(Some(v))
    }
}

/// Call `method` (`GetMyOrders` or `ListMyOrders`) and return the response's
/// bytes as the venue sent them.
async fn raw_call(rig: &Rig, method: &str, req: MyOrdersRequest) -> Vec<u8> {
    let channel = aspens::grpc::create_channel(&rig.stack)
        .await
        .expect("channel");
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.expect("channel ready");
    let path = format!("/xyz.aspens.arborter.v1.ArborterService/{method}");
    let path = tonic::codegen::http::uri::PathAndQuery::try_from(path).unwrap();
    grpc.unary(tonic::Request::new(req.encode_to_vec()), path, RawCodec)
        .await
        .unwrap_or_else(|s| panic!("{method}: {s}"))
        .into_inner()
}

fn varint(b: &mut &[u8]) -> u64 {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let (byte, rest) = b.split_first().expect("truncated varint");
        *b = rest;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return v;
        }
    }
    panic!("varint too long")
}

/// The top-level fields of a protobuf message: `(field number, bytes)`, where
/// a length-delimited field's bytes are its payload and any other field's
/// are its raw value.
fn fields(mut b: &[u8]) -> Vec<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let tag = varint(&mut b);
        let (field, wire) = (tag >> 3, tag & 7);
        let len = match wire {
            0 => {
                let start = b;
                varint(&mut b);
                let used = start.len() - b.len();
                out.push((field, start[..used].to_vec()));
                continue;
            }
            1 => 8,
            2 => usize::try_from(varint(&mut b)).unwrap(),
            5 => 4,
            w => panic!("wire type {w} on field {field}"),
        };
        out.push((field, b[..len].to_vec()));
        b = &b[len..];
    }
    out
}

/// The raw `MyOrder` entries of a raw `MyOrdersResponse`.
fn raw_orders(resp: &[u8]) -> Vec<Vec<u8>> {
    fields(resp)
        .into_iter()
        .filter(|(f, _)| *f == 1)
        .map(|(_, v)| v)
        .collect()
}

/// The raw entry answering for `id`.
fn raw_entry(entries: &[Vec<u8>], id: &[u8; 32]) -> Vec<u8> {
    entries
        .iter()
        .find(|e| {
            fields(e)
                .iter()
                .any(|(f, v)| *f == 1 && v.as_slice() == id.as_slice())
        })
        .unwrap_or_else(|| panic!("no entry answers for {}", hex::encode(id)))
        .clone()
}

/// Exactly what `found = false` for `id` encodes to: the id and nothing else.
fn not_found_bytes(id: &[u8; 32]) -> Vec<u8> {
    let mut v = vec![0x0a, 0x20];
    v.extend_from_slice(id);
    v
}

// ---- docker ----

fn arborter_service() -> String {
    std::env::var("ASPENS_MCP_RIG_ARBORTER_SERVICE").unwrap_or_else(|_| "local_arborter".into())
}

/// Restart the arborter in place (same image) and wait until it answers
/// order-status reads again.
async fn restart_arborter(rig: &Rig, session: &mut ReadSession, probe: [u8; 32]) {
    let service = arborter_service();
    let image = |service: &str| {
        let out = std::process::Command::new("docker")
            .args([
                "service",
                "inspect",
                service,
                "--format",
                "{{.Spec.TaskTemplate.ContainerSpec.Image}}",
            ])
            .output()
            .expect("docker service inspect");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let before = image(&service);
    let out = tokio::process::Command::new("docker")
        .args(["service", "update", "--force", "--quiet", &service])
        .output()
        .await
        .expect("docker service update");
    assert!(
        out.status.success(),
        "docker service update --force {service} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(image(&service), before, "the restart changed the image");
    eprintln!("    restarted {service} ({before})");
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Ok(cfg) = get_config(rig.stack.clone()).await else {
            continue;
        };
        let r =
            get_my_orders_from_config_with_wallet(&rig.stack, &cfg, session, &[], &[probe]).await;
        if r.is_ok() {
            return;
        }
    }
    panic!("the arborter did not answer GetMyOrders within 120 s of the restart");
}

// ---- shared setup ----

/// The trader's hidden resting bid, partly filled by the counterparty.
struct Partial {
    /// The trader's order.
    id: [u8; 32],
    /// The counterparty's crossing sell, filled in full.
    counter_id: [u8; 32],
    prices: Prices,
    initial: u128,
    crossed: u128,
}

/// The trader rests a hidden bid of 3 units; returns its id and prices.
async fn rest_hidden_bid(rig: &Rig) -> ([u8; 32], Prices, u128) {
    let p = prices(rig).await;
    let initial = rig.unit() * 3;
    let id = rig
        .send(&rig.trader, WireSide::Bid, initial, p.maker, true)
        .await;
    (id, p, initial)
}

/// [`rest_hidden_bid`], then the counterparty sells 1 unit into it.
async fn partial(rig: &Rig, session: &mut ReadSession) -> Partial {
    let (id, prices, initial) = rest_hidden_bid(rig).await;
    rig.await_order(session, id, "open", |o| state(o) == MyOrderState::Open)
        .await;
    let crossed = rig.unit();
    let counter_id = rig
        .send(&rig.counter, WireSide::Ask, crossed, prices.taker, false)
        .await;
    rig.await_order(session, id, "partially_filled", |o| {
        state(o) == MyOrderState::PartiallyFilled
    })
    .await;
    Partial {
        id,
        counter_id,
        prices,
        initial,
        crossed,
    }
}

/// The partial's fill, exactly: one maker fill of `crossed` at the maker's price.
fn assert_one_maker_fill(o: &MyOrder, p: &Partial) {
    assert_eq!(o.fills.len(), 1, "fills: {:?}", o.fills);
    let f = &o.fills[0];
    assert_eq!(f.role, FillRole::Maker as i32, "{f:?}");
    assert_eq!(f.qty, p.crossed.to_string(), "{f:?}");
    assert_eq!(f.price, p.prices.maker.to_string(), "{f:?}");
    assert_ne!(
        f.price,
        p.prices.taker.to_string(),
        "filled at the taker's price"
    );
    assert_eq!(f.fill_id.len(), 32, "{f:?}");
    assert!(f.match_ts_ms > 0, "{f:?}");
    assert_eq!(o.filled_qty, p.crossed.to_string(), "{o:?}");
}

// ---- 1-5: one hidden order through its life ----

/// 1. A hidden resting order is `OPEN`, hidden, with nothing filled.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s1_hidden_resting_order_is_open() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let (id, p, initial) = rest_hidden_bid(&rig).await;

    let o = rig.await_order(&mut session, id, "open", |o| o.found).await;
    eprintln!("    s1: {}", aspens::my_order_json(&o));
    assert_eq!(state(&o), MyOrderState::Open, "{o:?}");
    assert!(o.is_hidden, "{o:?}");
    assert_eq!(o.filled_qty, "0", "{o:?}");
    assert!(o.initial_qty_known, "{o:?}");
    assert_eq!(o.initial_qty, initial.to_string(), "{o:?}");
    assert_eq!(o.remaining_qty, initial.to_string(), "{o:?}");
    assert_eq!(o.price, p.maker.to_string(), "{o:?}");
    assert_eq!(o.side, WireSide::Bid as i32, "{o:?}");
    assert_eq!(o.market_id, rig.market, "{o:?}");
    assert_eq!(o.closed_at_ms, 0, "{o:?}");
    assert!(o.fills.is_empty(), "{o:?}");
    assert!(
        o.quote_address.eq_ignore_ascii_case(&rig.trader.address()),
        "the bid's owner is its quote leg: {o:?}"
    );

    rig.cancel(&rig.trader, "buy", id).await;
}

/// 2. A partial cross → `PARTIALLY_FILLED`, one MAKER fill, and the fill on
///    the wire carries no field beyond the five `MyFill` declares.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s2_partial_fill_is_partially_filled() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let p = partial(&rig, &mut session).await;

    let o = rig.get_one(&mut session, p.id).await;
    eprintln!("    s2: {}", aspens::my_order_json(&o));
    assert_eq!(state(&o), MyOrderState::PartiallyFilled, "{o:?}");
    assert!(o.is_hidden, "{o:?}");
    assert_one_maker_fill(&o, &p);
    let remaining = p.initial - p.crossed;
    assert!(p.initial != p.crossed && p.crossed != remaining && p.initial != remaining);
    assert_eq!(o.remaining_qty, remaining.to_string(), "{o:?}");
    assert_eq!(o.initial_qty, p.initial.to_string(), "{o:?}");

    // The raw response: the order carries only `MyOrder`'s 18 fields and its
    // fill only `MyFill`'s 5 (no counterparty, no order_hit, nothing else).
    let req = build_my_orders_request(
        &session,
        MyOrdersQuery {
            order_ids: vec![p.id.to_vec()],
            ..Default::default()
        },
    )
    .unwrap();
    let resp = raw_call(&rig, "GetMyOrders", req).await;
    let entry = raw_entry(&raw_orders(&resp), &p.id);
    let order_fields = fields(&entry);
    for (f, _) in &order_fields {
        assert!((1..=18).contains(f), "MyOrder carries field {f}");
    }
    let raw_fills: Vec<&Vec<u8>> = order_fields
        .iter()
        .filter(|(f, _)| *f == 18)
        .map(|(_, v)| v)
        .collect();
    assert_eq!(raw_fills.len(), 1, "raw fills: {raw_fills:?}");
    let fill_fields: Vec<u64> = fields(raw_fills[0]).into_iter().map(|(f, _)| f).collect();
    assert_eq!(
        fill_fields,
        vec![1, 2, 3, 4, 5],
        "MyFill carries exactly fill_id, price, qty, match_ts_ms, role"
    );
    let decoded = MyOrdersResponse::decode(resp.as_slice()).unwrap();
    assert_eq!(decoded.orders.len(), 1);
    assert_eq!(decoded.orders[0], o, "raw and SDK reads disagree");

    rig.cancel(&rig.trader, "buy", p.id).await;
}

/// 3. Cancel → `CANCELLED` with the crossed amount filled.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s3_cancel_keeps_filled_amount() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let p = partial(&rig, &mut session).await;

    rig.cancel(&rig.trader, "buy", p.id).await;
    let o = rig
        .await_order(&mut session, p.id, "cancelled", |o| {
            state(o) == MyOrderState::Cancelled
        })
        .await;
    eprintln!("    s3: {}", aspens::my_order_json(&o));
    assert_eq!(o.filled_qty, p.crossed.to_string(), "{o:?}");
    assert_ne!(o.filled_qty, p.initial.to_string(), "{o:?}");
    assert!(
        o.closed_at_ms >= o.created_at_ms && o.closed_at_ms > 0,
        "{o:?}"
    );
    assert_one_maker_fill(&o, &p);
}

/// 4. `ListMyOrders` lists the cancelled order only with `include_closed`;
///    while it rested, the open list had it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s4_list_include_closed() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let p = partial(&rig, &mut session).await;
    let has = |v: &[MyOrder]| v.iter().find(|o| o.order_id == p.id.to_vec()).cloned();

    let open = rig.list(&mut session, false).await;
    let o = has(&open).expect("the open list lacks the resting order");
    assert_eq!(state(&o), MyOrderState::PartiallyFilled, "{o:?}");

    rig.cancel(&rig.trader, "buy", p.id).await;
    rig.await_order(&mut session, p.id, "cancelled", |o| {
        state(o) == MyOrderState::Cancelled
    })
    .await;

    let all = rig.list(&mut session, true).await;
    let o = has(&all).expect("ListMyOrders { include_closed } lacks the cancelled order");
    assert_eq!(state(&o), MyOrderState::Cancelled, "{o:?}");
    assert_eq!(o.filled_qty, p.crossed.to_string(), "{o:?}");
    let open = rig.list(&mut session, false).await;
    assert!(
        has(&open).is_none(),
        "ListMyOrders without include_closed lists a cancelled order"
    );
    assert!(
        open.iter()
            .all(|o| matches!(state(o), MyOrderState::Open | MyOrderState::PartiallyFilled)),
        "the open list holds a closed order: {open:?}"
    );
    eprintln!(
        "    s4: {} listed with include_closed, {} without",
        all.len(),
        open.len()
    );
}

/// 5. The counterparty, which filled against the trader's order, asks for it
///    under its own delegation → `found = false`, the same bytes as a random id.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s5_foreign_id_is_not_found() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let p = partial(&rig, &mut session).await;
    let counter = rig.session(&[&rig.counter]).await;
    let random = random_id();

    let req = build_my_orders_request(
        &counter,
        MyOrdersQuery {
            order_ids: vec![p.id.to_vec(), random.to_vec(), p.counter_id.to_vec()],
            ..Default::default()
        },
    )
    .unwrap();
    let resp = raw_call(&rig, "GetMyOrders", req).await;
    let entries = raw_orders(&resp);
    assert_eq!(entries.len(), 3, "one entry per id");
    let foreign = raw_entry(&entries, &p.id);
    let unknown = raw_entry(&entries, &random);
    assert_eq!(foreign, not_found_bytes(&p.id), "the foreign id's entry");
    assert_eq!(unknown, not_found_bytes(&random), "the random id's entry");
    assert_eq!(
        foreign[2..].len(),
        unknown[2..].len(),
        "entries differ in more than the id"
    );
    assert_eq!(foreign[..2], unknown[..2]);

    // Not vacuous: the same session sees the counterparty's own crossing sell.
    let own = MyOrder::decode(raw_entry(&entries, &p.counter_id).as_slice()).unwrap();
    assert!(own.found, "{own:?}");
    assert_eq!(state(&own), MyOrderState::Filled, "{own:?}");
    assert_eq!(own.fills.len(), 1, "{own:?}");
    assert_eq!(own.fills[0].role, FillRole::Taker as i32, "{own:?}");
    assert_eq!(own.fills[0].price, p.prices.maker.to_string(), "{own:?}");
    eprintln!(
        "    s5: foreign {} / random {} both 0a20<id>; own sell {}",
        hex::encode(&foreign),
        hex::encode(&unknown),
        aspens::my_order_json(&own)
    );

    rig.cancel(&rig.trader, "buy", p.id).await;
}

// ---- 6: restart ----

/// 6. A second resting order is `DROPPED_BY_RESTART` after an arborter
///    restart; the first, closed before it, keeps its fill and is unchanged.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s6_restart_drops_resting_keeps_fills() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let mut session = rig.session(&[&rig.trader]).await;
    let p = partial(&rig, &mut session).await;
    rig.cancel(&rig.trader, "buy", p.id).await;
    let first = rig
        .await_order(&mut session, p.id, "cancelled", |o| {
            state(o) == MyOrderState::Cancelled
        })
        .await;

    let second_qty = rig.unit() * 4;
    let second = rig
        .send(&rig.trader, WireSide::Bid, second_qty, p.prices.taker, true)
        .await;
    rig.await_order(&mut session, second, "open", |o| {
        state(o) == MyOrderState::Open
    })
    .await;

    restart_arborter(&rig, &mut session, second).await;

    let after = rig.get(&mut session, &[p.id, second]).await;
    assert_eq!(after.len(), 2, "{after:?}");
    let first_after = after.iter().find(|o| o.order_id == p.id.to_vec()).unwrap();
    let dropped = after
        .iter()
        .find(|o| o.order_id == second.to_vec())
        .unwrap();
    eprintln!("    s6: dropped {}", aspens::my_order_json(dropped));
    assert_eq!(
        state(dropped),
        MyOrderState::DroppedByRestart,
        "{dropped:?}"
    );
    assert_eq!(dropped.filled_qty, "0", "{dropped:?}");
    assert_eq!(dropped.initial_qty, second_qty.to_string(), "{dropped:?}");
    assert!(dropped.fills.is_empty(), "{dropped:?}");
    assert!(dropped.closed_at_ms > 0, "{dropped:?}");
    // The first order closed before the restart: the restart must not touch it.
    assert_eq!(
        state(first_after),
        MyOrderState::Cancelled,
        "{first_after:?}"
    );
    assert_one_maker_fill(first_after, &p);
    assert_eq!(
        first_after, &first,
        "the restart changed a closed order's record"
    );

    // And the open list no longer has the dropped order.
    let open = rig.list(&mut session, false).await;
    assert!(
        !open.iter().any(|o| o.order_id == second.to_vec()),
        "a dropped order is still listed as open"
    );
}

// ---- 7: two wallets, two networks ----

/// 7. The trader's two wallets each own one side of the market: its first
///    wallet a sell (collateral on the base network), its second a buy
///    (collateral on the quote network). One `ListMyOrders` under both
///    delegations lists both; each delegation alone lists only its own.
///
/// The rig has no market whose legs are on chains of different curves, so
/// this runs on two EVM networks with two distinct wallets.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s7_two_delegations_list_both_sides() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let (bid, ask) = rig.top().await;
    let hi = [bid, ask, Some(rig.unit() * 16 / 10)]
        .into_iter()
        .flatten()
        .max()
        .unwrap()
        * 2;
    let lo = match (bid, ask) {
        (_, Some(a)) => a.min(bid.unwrap_or(a)) / 2,
        (Some(b), None) => b / 2,
        (None, None) => rig.unit() * 8 / 10,
    };
    assert!(lo > 0 && lo < hi);
    let sell_qty = rig.unit() * 5;
    let buy_qty = rig.unit() * 7;
    // The sell settles its receive (quote) leg to the second wallet, so an
    // owner taken from the receive leg would list it under the wrong wallet.
    let sell = rig
        .send_settling(
            &rig.trader,
            WireSide::Ask,
            sell_qty,
            hi,
            false,
            Some(rig.trader2.address()),
        )
        .await;
    let buy = rig
        .send(&rig.trader2, WireSide::Bid, buy_qty, lo, false)
        .await;

    let key = DelegateKey::generate().unwrap();
    let d_base = create_read_delegation_with_wallet(
        &rig.trader,
        &rig.base_network,
        Architecture::Evm,
        &venue_id(&rig).await,
        &key,
        TTL,
    )
    .await
    .unwrap();
    let d_quote = create_read_delegation_with_wallet(
        &rig.trader2,
        &rig.quote_network,
        Architecture::Evm,
        &venue_id(&rig).await,
        &key,
        TTL,
    )
    .await
    .unwrap();
    let both = ReadSession::new(vec![d_base, d_quote], key).unwrap();
    assert_eq!(both.delegations().len(), 2);

    // One call.
    let mut listed = None;
    for _ in 0..30 {
        let req = build_my_orders_request(
            &both,
            MyOrdersQuery {
                market_id: rig.market.clone(),
                limit: 500,
                ..Default::default()
            },
        )
        .unwrap();
        let resp =
            MyOrdersResponse::decode(raw_call(&rig, "ListMyOrders", req).await.as_slice()).unwrap();
        assert!(
            resp.next_cursor.is_empty(),
            "more than one page of open orders"
        );
        let has = |id: &[u8; 32]| resp.orders.iter().any(|o| o.order_id == id.to_vec());
        if has(&sell) && has(&buy) {
            listed = Some(resp.orders);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let listed = listed.expect("one ListMyOrders under both delegations never listed both sides");
    let find = |id: &[u8; 32]| {
        listed
            .iter()
            .find(|o| o.order_id == id.to_vec())
            .unwrap()
            .clone()
    };
    let s = find(&sell);
    let b = find(&buy);
    eprintln!(
        "    s7: sell {} / buy {}",
        aspens::my_order_json(&s),
        aspens::my_order_json(&b)
    );
    assert_eq!(s.side, WireSide::Ask as i32, "{s:?}");
    assert_eq!(b.side, WireSide::Bid as i32, "{b:?}");
    assert_eq!(s.initial_qty, sell_qty.to_string(), "{s:?}");
    assert_eq!(b.initial_qty, buy_qty.to_string(), "{b:?}");
    assert!(
        s.base_address.eq_ignore_ascii_case(&rig.trader.address()),
        "{s:?}"
    );
    assert!(
        s.quote_address.eq_ignore_ascii_case(&rig.trader2.address()),
        "the sell's receive leg is the second wallet: {s:?}"
    );
    assert!(
        b.quote_address.eq_ignore_ascii_case(&rig.trader2.address()),
        "{b:?}"
    );

    // Each delegation alone lists only the side its wallet owns.
    let mut first_only = rig.session(&[&rig.trader]).await;
    let mut second_only = rig.session(&[&rig.trader2]).await;
    let ids = |v: Vec<MyOrder>| -> Vec<Vec<u8>> { v.into_iter().map(|o| o.order_id).collect() };
    let a = ids(rig.list(&mut first_only, false).await);
    let z = ids(rig.list(&mut second_only, false).await);
    assert!(a.contains(&sell.to_vec()) && !a.contains(&buy.to_vec()));
    assert!(z.contains(&buy.to_vec()) && !z.contains(&sell.to_vec()));

    rig.cancel(&rig.trader, "sell", sell).await;
    rig.cancel(&rig.trader2, "buy", buy).await;
}

async fn venue_id(rig: &Rig) -> String {
    rig.config().await.config.expect("configuration").venue_id
}

// ---- 8: aspens-mcp ----

struct Session {
    dir: tempfile::TempDir,
    cfg: PathBuf,
    log: PathBuf,
}

fn write_private(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl Session {
    /// A config allowing orders up to `max_qty` at prices up to `max_price`
    /// on the rig's market.
    fn new(rig: &Rig, max_qty: u128, max_price: u128) -> Session {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(dir.path().join("state")).unwrap();
        write_private(&dir.path().join("trader.env"), &rig.trader_env);
        let order_quote = (max_qty * max_price).div_ceil(rig.unit());
        let toml = format!(
            r#"stack_url = "{stack}"
env_file = "{env}"
state_dir = "{state}"

[markets."{market}"]
max_order_qty = "{oq}"
max_daily_qty = "{dq}"
max_order_quote = "{oqu}"
max_daily_quote = "{dqu}"
max_deviation_bps = 5000
"#,
            stack = rig.stack,
            env = dir.path().join("trader.env").display(),
            state = dir.path().join("state").display(),
            market = rig.market,
            oq = rig.s(max_qty),
            dq = rig.s(max_qty * 7),
            oqu = rig.s(order_quote),
            dqu = rig.s(order_quote * 9),
        );
        let cfg = dir.path().join("aspens-mcp.toml");
        write_private(&cfg, &toml);
        Session {
            cfg,
            log: dir.path().join("aspens-mcp.log"),
            dir,
        }
    }

    async fn start(&self) -> Mcp {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_aspens-mcp"));
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("RUST_LOG", "info")
            .arg("--config")
            .arg(&self.cfg);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let (proc, _) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawning aspens-mcp");
        let client = ().serve(proc).await.unwrap_or_else(|e| {
            panic!(
                "aspens-mcp did not start ({e}); its log:\n{}",
                std::fs::read_to_string(&self.log).unwrap_or_default()
            )
        });
        Mcp { client }
    }
}

struct Mcp {
    client: RunningService<RoleClient, ()>,
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Mcp {
    async fn ok(&self, tool: &str, args: Value) -> Value {
        let mut p = CallToolRequestParams::new(tool.to_string());
        if let Value::Object(m) = args {
            p = p.with_arguments(m);
        }
        let r = self
            .client
            .call_tool(p)
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e}"));
        let t = text(&r);
        assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
        serde_json::from_str(&t).unwrap_or_else(|e| panic!("{tool}: not JSON ({e}): {t}"))
    }

    async fn stop(self) {
        let _ = self.client.cancel().await;
    }
}

fn raw(v: &Value, key: &str) -> Option<u128> {
    v.get(key)?.as_str()?.parse().ok()
}

async fn poll<T, F, Fut>(secs: u64, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    for _ in 0..secs.max(1) {
        if let Some(v) = f().await {
            return Some(v);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    None
}

/// 8. aspens-mcp places a hidden limit order; the counterparty fills it;
///    `get_order` tracks it to `filled`, with the fill.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s8_mcp_hidden_order_tracked_to_filled() {
    let _g = RIG_LOCK.lock().await;
    let rig = Rig::load().await;
    let p = prices(&rig).await;
    let qty = rig.unit() * 2;
    let s = Session::new(&rig, qty * 2, p.maker * 2);
    let mcp = s.start().await;

    let o = mcp
        .ok(
            "place_limit_order",
            json!({"market_id": rig.market, "side": "buy", "qty": rig.s(qty),
                   "price": rig.s(p.maker), "post_only": true, "hidden": true}),
        )
        .await;
    assert_eq!(o["state"], "open", "{o}");
    let id = o["orderId"].as_str().expect("orderId").to_string();
    let raw_id: [u8; 32] = hex::decode(id.trim_start_matches("0x"))
        .ok()
        .and_then(|v| v.try_into().ok())
        .expect("orderId is 0x + 64 hex");
    rig.track(&rig.trader, WireSide::Bid, raw_id);

    let g = mcp.ok("get_order", json!({"order_id": id})).await;
    assert_eq!(g["state"], "open", "{g}");
    assert_eq!(g["hidden"], true, "{g}");

    rig.send(&rig.counter, WireSide::Ask, qty, p.taker, false)
        .await;
    let m = &mcp;
    let i = id.as_str();
    let g = poll(30, move || async move {
        let g = m.ok("get_order", json!({"order_id": i})).await;
        (g["state"] == "filled").then_some(g)
    })
    .await
    .expect("get_order never reported filled");
    eprintln!("    s8: {g}");
    assert_eq!(g["hidden"], true, "{g}");
    assert_eq!(raw(&g, "filledRaw"), Some(qty), "{g}");
    assert_eq!(raw(&g, "remainingRaw"), Some(0), "{g}");
    let fills = g["fills"].as_array().expect("fills");
    assert_eq!(fills.len(), 1, "{g}");
    assert_eq!(fills[0]["role"], "maker", "{g}");
    assert_eq!(raw(&fills[0], "qtyRaw"), Some(qty), "{g}");
    assert_eq!(raw(&fills[0], "priceRaw"), Some(p.maker), "{g}");
    assert!(
        fills[0]["fillId"]
            .as_str()
            .is_some_and(|f| f.len() == 66 && f.starts_with("0x")),
        "{g}"
    );

    let mine = mcp.ok("my_fills", json!({"market_id": rig.market})).await;
    assert!(
        mine["fills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["orderId"] == id.as_str() && f["role"] == "maker"),
        "my_fills lacks the hidden order's fill: {mine}"
    );
    mcp.stop().await;
}
