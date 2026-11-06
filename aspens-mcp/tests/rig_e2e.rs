//! aspens-mcp end to end on the local rig: the built `aspens-mcp` binary is
//! spawned over stdio and driven by an rmcp client, exactly as an agent
//! harness would drive it. The crossing counterparty signs with the `aspens`
//! SDK directly, not through MCP.
//!
//! Every test is `#[ignore]` and also requires `RIG=1`; run them in order, one
//! at a time:
//!
//! ```text
//! RIG=1 \
//! ASPENS_MCP_RIG_MARKET='<market_id from aspens-cli config>' \
//! ASPENS_MCP_RIG_TRADER_ENV=/path/to/sdk/.env.local.1 \
//! ASPENS_MCP_RIG_COUNTER_ENV=/path/to/sdk/.env.local.2 \
//!   cargo test -p aspens-mcp --test rig_e2e -- --ignored --test-threads=1
//! ```
//!
//! The rig is the evm-evm-local scenario (recipe: `otc/e2e/README.md`): two
//! anvil forks of Coston2, both chain id 114, so chains are addressed by RPC
//! URL. Only anvil dev keys belong in the env files; this suite only ever runs
//! against a local rig.
//!
//! # Env vars read
//!
//! - `RIG` — must be `1`.
//! - `ASPENS_MCP_RIG_MARKET` — the market id to trade, exactly as the venue
//!   lists it.
//! - `ASPENS_MCP_RIG_TRADER_ENV` — env file of the trader that aspens-mcp
//!   drives: `TRADER_PRIVKEY` and `ASPENS_RPC_URL_<NETWORK>` for both chains.
//!   Each run copies it into a fresh temp dir with mode 0600, next to a 0600
//!   config and an isolated `state_dir`; the file itself is never modified.
//! - `ASPENS_MCP_RIG_COUNTER_ENV` — env file of the crossing counterparty; only
//!   its `TRADER_PRIVKEY` is read. It must be a different wallet.
//! - `ASPENS_MCP_RIG_STACK` — the venue's native gRPC endpoint. Default: the
//!   trader env file's `ASPENS_MARKET_STACK_URL`, else `http://localhost:50051`.
//! - `ASPENS_MCP_RIG_QTY` — base quantity per order, market units (default `1`).
//! - `ASPENS_MCP_RIG_PRICE` — the price used only when the book is empty
//!   (default `1.6`).
//! - `ASPENS_MCP_RIG_TRANSFER` — the deposit and withdraw amount, in the quote
//!   token's units (default `5`).
//!
//! # Funding
//!
//! The trader needs quote token in its wallet (deposits) and on the venue
//! (buys); the counterparty needs base token on the venue (its sells). The
//! suite mines blocks on every `ASPENS_RPC_URL_*` in the trader env file so
//! deposits and settlement clear finality.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aspens::Wallet;
use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::commands::config::get_config;
use aspens::commands::trading::arborter_pb::Side as WireSide;
use aspens::commands::trading::cancel_order::call_cancel_order_from_config_with_wallet;
use aspens::commands::trading::send_order::send_order_with_wallets;
use aspens::decimals::{format_decimal_amount, parse_decimal_amount};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

const HELP: &str = "the rig e2e needs RIG=1, ASPENS_MCP_RIG_MARKET, ASPENS_MCP_RIG_TRADER_ENV and \
                    ASPENS_MCP_RIG_COUNTER_ENV; see the header of aspens-mcp/tests/rig_e2e.rs";

// ---- rig ----

/// Everything a scenario needs to know about the rig, read once per test.
struct Rig {
    stack: String,
    market: String,
    /// The trader env file's contents, copied into each session.
    trader_env: String,
    counter: Wallet,
    /// Every `ASPENS_RPC_URL_*` in the trader env file: the chains to mine.
    rpcs: Vec<String>,
    /// Pair decimals of the market.
    d: u32,
    /// Base quantity per order, pair units.
    qty: u128,
    /// The price used when the book is empty, pair units.
    default_price: u128,
    quote_network: String,
    quote_token: String,
    quote_token_decimals: u32,
    base_token: String,
    base_network: String,
    /// The deposit / withdraw amount, as given (quote token units).
    transfer: String,
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

/// Panic unless `url`'s host is loopback (`localhost`, `127.0.0.1` or `::1`).
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
        "refusing to run the rig e2e: {what} is {url:?}, whose host is not loopback \
         (localhost, 127.0.0.1 or ::1). This suite sends real orders and transfers; point it \
         at a local rig only"
    );
}
impl Rig {
    async fn load() -> Rig {
        assert_eq!(std::env::var("RIG").as_deref(), Ok("1"), "{HELP}");
        let market = required("ASPENS_MCP_RIG_MARKET");
        let trader_path = required("ASPENS_MCP_RIG_TRADER_ENV");
        let counter_path = required("ASPENS_MCP_RIG_COUNTER_ENV");
        let trader_env = std::fs::read_to_string(&trader_path)
            .unwrap_or_else(|e| panic!("reading {trader_path}: {e}"));
        let tvars = env_file_vars(&trader_env);
        let counter_text = std::fs::read_to_string(&counter_path)
            .unwrap_or_else(|e| panic!("reading {counter_path}: {e}"));
        let cvars = env_file_vars(&counter_text);
        let counter_key = cvars
            .get("TRADER_PRIVKEY")
            .unwrap_or_else(|| panic!("{counter_path} has no TRADER_PRIVKEY"));
        let counter = Wallet::from_evm_hex(counter_key).expect("counterparty key");
        let trader_key = tvars
            .get("TRADER_PRIVKEY")
            .unwrap_or_else(|| panic!("{trader_path} has no TRADER_PRIVKEY"));
        let trader = Wallet::from_evm_hex(trader_key).expect("trader key");
        assert_ne!(
            trader.address(),
            counter.address(),
            "the trader and the counterparty must be different wallets"
        );
        let rpcs: Vec<String> = tvars
            .iter()
            .filter(|(k, _)| k.starts_with("ASPENS_RPC_URL_"))
            .map(|(_, v)| v.clone())
            .collect();
        assert!(!rpcs.is_empty(), "{trader_path} has no ASPENS_RPC_URL_*");

        let stack = std::env::var("ASPENS_MCP_RIG_STACK")
            .ok()
            .or_else(|| tvars.get("ASPENS_MARKET_STACK_URL").cloned())
            .unwrap_or_else(|| "http://localhost:50051".to_string());
        // This suite signs and sends real orders and transfers: only ever
        // against a local rig, never a deployed venue or a remote chain.
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
        let d = u32::try_from(m.pair_decimals).expect("pair decimals");
        let quote_token_decimals = cfg
            .get_token(&m.quote_chain_network, &m.quote_chain_token_symbol)
            .expect("quote token in config")
            .decimals;
        let qty_s = std::env::var("ASPENS_MCP_RIG_QTY").unwrap_or_else(|_| "1".into());
        let price_s = std::env::var("ASPENS_MCP_RIG_PRICE").unwrap_or_else(|_| "1.6".into());
        Rig {
            stack,
            market,
            trader_env,
            counter,
            rpcs,
            d,
            qty: parse_decimal_amount(&qty_s, d).expect("ASPENS_MCP_RIG_QTY"),
            default_price: parse_decimal_amount(&price_s, d).expect("ASPENS_MCP_RIG_PRICE"),
            quote_network: m.quote_chain_network.clone(),
            quote_token: m.quote_chain_token_symbol.clone(),
            quote_token_decimals,
            base_token: m.base_chain_token_symbol.clone(),
            base_network: m.base_chain_network.clone(),
            transfer: std::env::var("ASPENS_MCP_RIG_TRANSFER").unwrap_or_else(|_| "5".into()),
        }
    }

    /// A pair-unit amount as the decimal string a tool takes.
    fn s(&self, raw: u128) -> String {
        format_decimal_amount(raw, self.d)
    }

    /// The quote a buy commits: `ceil(qty × price / 10^d)`, as the server computes it.
    fn quote(&self, qty: u128, price: u128) -> u128 {
        let scale = 10u128.pow(self.d);
        qty.checked_mul(price)
            .expect("quote overflow")
            .div_ceil(scale)
    }

    /// Mine 5 blocks on every chain, so deposits and settlement clear finality.
    async fn mine(&self) {
        let http = reqwest::Client::new();
        for url in &self.rpcs {
            let body =
                json!({"jsonrpc": "2.0", "id": 1, "method": "anvil_mine", "params": ["0x5"]});
            let r = http
                .post(url)
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
                .await;
            assert!(
                r.as_ref().is_ok_and(|r| r.status().is_success()),
                "anvil_mine on an ASPENS_RPC_URL_* endpoint failed: {r:?}"
            );
        }
    }

    // ---- the counterparty, over the SDK ----

    /// Send a limit order as the counterparty; returns its 32-byte order id.
    async fn counter_send(
        &self,
        side: WireSide,
        qty: u128,
        price: u128,
        post_only: bool,
    ) -> [u8; 32] {
        let cfg = venue_config(&self.stack).await;
        let resp = send_order_with_wallets(
            self.stack.clone(),
            self.market.clone(),
            side as i32,
            self.s(qty),
            Some(self.s(price)),
            &[&self.counter],
            cfg,
            post_only,
            false,
            None,
            vec![],
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("counterparty send_order failed: {e:#}"));
        <[u8; 32]>::try_from(resp.order_id.as_slice()).expect("32-byte order id")
    }

    /// Cancel a counterparty order; one that is already gone is fine.
    async fn counter_cancel(&self, side: &str, id: [u8; 32]) {
        let cfg = venue_config(&self.stack).await;
        let r = call_cancel_order_from_config_with_wallet(
            self.stack.clone(),
            self.market.clone(),
            side.to_string(),
            id,
            &self.counter,
            cfg,
        )
        .await;
        if let Err(e) = r {
            eprintln!("counterparty cleanup cancel: {e:#}");
        }
    }
}

// ---- limits ----

#[derive(Clone)]
struct Limits {
    order_qty: u128,
    daily_qty: u128,
    order_quote: u128,
    daily_quote: u128,
    dev_bps: u32,
}

impl Limits {
    /// Limits no scenario order reaches, around reference price `r`. The
    /// four amounts differ from each other, so a swapped field is caught.
    fn wide(rig: &Rig, r: u128) -> Limits {
        let order_quote = rig.quote(rig.qty * 5, r * 2);
        Limits {
            order_qty: rig.qty * 5,
            daily_qty: rig.qty * 40,
            order_quote,
            daily_quote: order_quote * 9,
            dev_bps: 500,
        }
    }
}

struct Opts {
    limits: Limits,
    allow_withdraw: bool,
    allowlist: Vec<String>,
}

impl Opts {
    fn new(limits: Limits) -> Opts {
        Opts {
            limits,
            allow_withdraw: false,
            allowlist: Vec::new(),
        }
    }
}

// ---- a session: one temp dir, one state_dir, any number of process runs ----

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
    fn new(rig: &Rig, opts: &Opts) -> Session {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(dir.path().join("state")).unwrap();
        write_private(&dir.path().join("trader.env"), &rig.trader_env);
        let s = Session {
            cfg: dir.path().join("aspens-mcp.toml"),
            log: dir.path().join("aspens-mcp.log"),
            dir,
        };
        s.configure(rig, opts);
        s
    }

    /// (Re)write the config; the state dir is kept.
    fn configure(&self, rig: &Rig, opts: &Opts) {
        let l = &opts.limits;
        let allow: Vec<String> = opts.allowlist.iter().map(|a| format!("{a:?}")).collect();
        let toml = format!(
            r#"stack_url = "{stack}"
env_file = "{env}"
state_dir = "{state}"
allow_withdraw = {withdraw}
settle_address_allowlist = [{allow}]

[deposits]
enabled = true

[markets."{market}"]
max_order_qty = "{oq}"
max_daily_qty = "{dq}"
max_order_quote = "{oqu}"
max_daily_quote = "{dqu}"
max_deviation_bps = {dev}
"#,
            stack = rig.stack,
            env = self.dir.path().join("trader.env").display(),
            state = self.dir.path().join("state").display(),
            withdraw = opts.allow_withdraw,
            allow = allow.join(", "),
            market = rig.market,
            oq = rig.s(l.order_qty),
            dq = rig.s(l.daily_qty),
            oqu = rig.s(l.order_quote),
            dqu = rig.s(l.daily_quote),
            dev = l.dev_bps,
        );
        write_private(&self.cfg, &toml);
    }

    /// The binary, with nothing inherited but PATH: keys come from the env file only.
    fn command(&self) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_aspens-mcp"));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("RUST_LOG", "info");
        c
    }

    async fn start(&self) -> Mcp {
        let mut cmd = self.command();
        cmd.arg("--config").arg(&self.cfg);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let (proc, _) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawning aspens-mcp");
        let pid = proc.id();
        let client = ().serve(proc).await.unwrap_or_else(|e| {
            panic!(
                "aspens-mcp did not start ({e}); its log:\n{}",
                std::fs::read_to_string(&self.log).unwrap_or_default()
            )
        });
        Mcp { client, pid }
    }

    /// `aspens-mcp resume --config …`, as the human runs it.
    async fn resume(&self) -> std::process::Output {
        let mut cmd = self.command();
        cmd.arg("resume").arg("--config").arg(&self.cfg);
        cmd.output().await.expect("running aspens-mcp resume")
    }
}

// ---- the MCP client ----

struct Mcp {
    client: RunningService<RoleClient, ()>,
    pid: Option<u32>,
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Mcp {
    async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult, rmcp::ServiceError> {
        let mut p = CallToolRequestParams::new(tool.to_string());
        if let Value::Object(m) = args {
            p = p.with_arguments(m);
        }
        self.client.call_tool(p).await
    }

    /// Call `tool`, asserting success, and parse its JSON.
    async fn ok(&self, tool: &str, args: Value) -> Value {
        let r = self
            .call(tool, args)
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e}"));
        let t = text(&r);
        assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
        serde_json::from_str(&t).unwrap_or_else(|e| panic!("{tool}: not JSON ({e}): {t}"))
    }

    /// Call `tool`, asserting an error result; returns its text.
    async fn err(&self, tool: &str, args: Value) -> String {
        let r = self
            .call(tool, args)
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e}"));
        let t = text(&r);
        assert_eq!(
            r.is_error,
            Some(true),
            "{tool} should have been refused: {t}"
        );
        t
    }

    async fn tools(&self) -> Vec<String> {
        let tools = self.client.list_all_tools().await.unwrap();
        tools.into_iter().map(|t| t.name.to_string()).collect()
    }

    /// Close stdio and wait for the process to exit, so its ledger lock is free.
    async fn stop(self) {
        let pid = self.pid;
        let _ = self.client.cancel().await;
        let Some(pid) = pid else { return };
        let gone = poll(15, || async move {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            (!alive).then_some(())
        })
        .await;
        assert!(gone.is_some(), "aspens-mcp (pid {pid}) did not exit");
    }
}

/// Retry `f` once a second for up to `secs` until it yields `Some`.
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

fn raw(v: &Value, key: &str) -> Option<u128> {
    v.get(key)?.as_str()?.parse().ok()
}

// ---- book, balances and orders through the server ----

/// `(best bid, best ask)` in pair units.
async fn top(rig: &Rig, mcp: &Mcp) -> (Option<u128>, Option<u128>) {
    let t = mcp
        .ok("top_of_book", json!({"market_id": rig.market}))
        .await;
    (raw(&t, "bestBidRaw"), raw(&t, "bestAskRaw"))
}

/// A buy price that cannot cross: under the best ask, or at the best bid
/// when there are no asks.
fn rest_buy_price(rig: &Rig, (bid, ask): (Option<u128>, Option<u128>)) -> u128 {
    let p = match (bid, ask) {
        (_, Some(a)) => a * 9 / 10,
        (Some(b), None) => b,
        (None, None) => rig.default_price,
    };
    assert!(p > 0, "the book's prices are too small to step under");
    p
}

/// A sell price that cannot cross: twice the highest price in sight.
fn rest_sell_price(rig: &Rig, (bid, ask): (Option<u128>, Option<u128>)) -> u128 {
    [bid, ask, Some(rig.default_price)]
        .into_iter()
        .flatten()
        .max()
        .unwrap()
        * 2
}

/// The highest price any scenario order uses, for sizing wide limits.
fn reference(rig: &Rig, t: (Option<u128>, Option<u128>)) -> u128 {
    rest_sell_price(rig, t)
}

/// The trader's venue balance of the market's two tokens:
/// `"SYMBOL@network" -> (available, locked)`, raw.
async fn venue_balances(rig: &Rig, mcp: &Mcp) -> BTreeMap<String, (u128, u128)> {
    let b = mcp.ok("balances", json!({})).await;
    let mut out = BTreeMap::new();
    for t in b["tokens"].as_array().unwrap() {
        let sym = t["symbol"].as_str().unwrap();
        for c in t["chains"].as_array().unwrap() {
            let net = c["network"].as_str().unwrap();
            let ours = (sym == rig.base_token && net == rig.base_network)
                || (sym == rig.quote_token && net == rig.quote_network);
            if !ours {
                continue;
            }
            let a = raw(c, "availableRaw")
                .unwrap_or_else(|| panic!("{sym}@{net}: no available balance: {c}"));
            let l = raw(c, "lockedRaw")
                .unwrap_or_else(|| panic!("{sym}@{net}: no locked balance: {c}"));
            out.insert(format!("{sym}@{net}"), (a, l));
        }
    }
    assert_eq!(out.len(), 2, "balances lacks the market's tokens: {b}");
    out
}

/// The trader's balance of `token@network` in its wallet (on chain), raw.
async fn wallet_balance(mcp: &Mcp, network: &str, token: &str) -> u128 {
    let b = mcp.ok("balances", json!({})).await;
    for t in b["tokens"].as_array().unwrap() {
        if t["symbol"] != token {
            continue;
        }
        for c in t["chains"].as_array().unwrap() {
            if c["network"] == network {
                return raw(c, "walletRaw").unwrap_or_else(|| panic!("no wallet balance: {c}"));
            }
        }
    }
    panic!("balances has no {token}@{network}: {b}")
}

/// The venue balance once settlement has stopped moving it.
async fn stable_balances(rig: &Rig, mcp: &Mcp) -> BTreeMap<String, (u128, u128)> {
    let mut last = venue_balances(rig, mcp).await;
    for _ in 0..20 {
        rig.mine().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let now = venue_balances(rig, mcp).await;
        if now == last {
            return now;
        }
        last = now;
    }
    panic!("the trader's venue balance kept moving for a minute: {last:?}")
}

fn assert_open(o: &Value) -> String {
    assert_eq!(o["state"], "open", "{o}");
    o["orderId"].as_str().expect("orderId").to_string()
}

async fn place_resting_buy(rig: &Rig, mcp: &Mcp, price: u128) -> String {
    let o = mcp
        .ok(
            "place_limit_order",
            json!({"market_id": rig.market, "side": "buy", "qty": rig.s(rig.qty),
                   "price": rig.s(price), "post_only": true}),
        )
        .await;
    assert_eq!(raw(&o, "qtyRaw"), Some(rig.qty), "{o}");
    assert_eq!(raw(&o, "priceRaw"), Some(price), "{o}");
    assert_open(&o)
}

async fn cancel(mcp: &Mcp, id: &str) -> Value {
    mcp.ok("cancel_order", json!({"order_id": id})).await
}

/// `text` refuses `what` for exactly one reason, and that reason names `rule`.
fn assert_refused_for(text: &str, what: &str, rule: &str) {
    let prefix = format!("{what} refused, nothing was signed: ");
    let reasons = text
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("not a {what} refusal: {text}"));
    let reasons: Vec<&str> = reasons.split("; ").collect();
    assert_eq!(reasons.len(), 1, "refused for more than one reason: {text}");
    assert!(
        reasons[0].contains(rule),
        "refusal does not name {rule:?}: {text}"
    );
}

/// Call a write tool that must be refused for `rule` alone, and check that
/// nothing moved: the venue balance, the order list and today's usage.
async fn assert_refusal(rig: &Rig, mcp: &Mcp, tool: &str, args: Value, what: &str, rule: &str) {
    let before = stable_balances(rig, mcp).await;
    let orders = mcp.ok("my_orders", json!({"status": "all"})).await;
    let usage = mcp.ok("limits", json!({})).await;
    let t = mcp.err(tool, args).await;
    assert_refused_for(&t, what, rule);
    assert_eq!(
        mcp.ok("my_orders", json!({"status": "all"})).await["orders"],
        orders["orders"],
        "a refused {tool} changed my_orders"
    );
    assert_eq!(
        mcp.ok("limits", json!({})).await["markets"],
        usage["markets"],
        "a refused {tool} changed today's usage"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        venue_balances(rig, mcp).await,
        before,
        "a refused {tool} changed the trader's venue balance"
    );
}

/// A session with wide limits around the current book.
async fn wide_session(rig: &Rig) -> (Session, Mcp) {
    let probe = Session::new(rig, &Opts::new(Limits::wide(rig, rig.default_price.max(1))));
    let mcp = probe.start().await;
    let r = reference(rig, top(rig, &mcp).await);
    mcp.stop().await;
    probe.configure(rig, &Opts::new(Limits::wide(rig, r)));
    let mcp = probe.start().await;
    (probe, mcp)
}

// ---- the scenarios, in the spec's order ----

/// 1. `venue_config`, `balances` and `top_of_book` return sane data.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s1_reads_are_sane() {
    let rig = Rig::load().await;
    let (_s, mcp) = wide_session(&rig).await;

    let cfg = mcp.ok("venue_config", json!({})).await;
    let m = cfg["markets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["marketId"] == rig.market.as_str())
        .unwrap_or_else(|| panic!("venue_config lacks {}: {cfg}", rig.market))
        .clone();
    assert_eq!(m["tradable"], true, "{m}");
    assert_eq!(m["pairDecimals"], rig.d, "{m}");
    assert_eq!(m["quote"]["token"], rig.quote_token.as_str(), "{m}");
    assert_eq!(m["quote"]["decimals"], rig.quote_token_decimals, "{m}");
    let others_tradable = cfg["markets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["marketId"] != rig.market.as_str() && o["tradable"] == true)
        .count();
    assert_eq!(others_tradable, 0, "only the configured market is tradable");

    let bal = venue_balances(&rig, &mcp).await;
    assert_eq!(bal.len(), 2);

    let (bid, ask) = top(&rig, &mcp).await;
    if let (Some(b), Some(a)) = (bid, ask) {
        assert!(b < a, "crossed book: bid {b} >= ask {a}");
    }
    let status = mcp.ok("account_status", json!({})).await;
    assert_eq!(status["venue"]["reachable"], true, "{status}");
    assert_eq!(status["halted"], false, "{status}");
    mcp.stop().await;
}

/// 2. `deposit` lands and shows in `balances`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s2_deposit_lands() {
    let rig = Rig::load().await;
    let (_s, mcp) = wide_session(&rig).await;
    let amount = parse_decimal_amount(&rig.transfer, rig.quote_token_decimals).unwrap();
    let key = format!("{}@{}", rig.quote_token, rig.quote_network);
    let before = stable_balances(&rig, &mcp).await[&key].0;
    let wallet_before = wallet_balance(&mcp, &rig.quote_network, &rig.quote_token).await;

    let r = mcp
        .ok(
            "deposit",
            json!({"network": rig.quote_network, "token": rig.quote_token, "amount": rig.transfer}),
        )
        .await;
    assert_eq!(raw(&r, "amountRaw"), Some(amount), "{r}");

    let m = &mcp;
    let r2 = &rig;
    let k = &key;
    let landed = poll(90, move || async move {
        r2.mine().await;
        let now = venue_balances(r2, m).await[k].0;
        (now != before).then_some(now)
    })
    .await
    .expect("the deposit never showed in balances");
    assert_eq!(
        landed,
        before + amount,
        "venue balance moved by the wrong amount"
    );
    let wallet_after = wallet_balance(&mcp, &rig.quote_network, &rig.quote_token).await;
    assert_eq!(
        wallet_after,
        wallet_before - amount,
        "wallet moved by the wrong amount"
    );
    mcp.stop().await;
}

/// 3. A resting limit order is `open`, a cancel makes it `cancelled`, a
///    second cancel is `already_gone`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s3_rest_cancel_already_gone() {
    let rig = Rig::load().await;
    let (_s, mcp) = wide_session(&rig).await;
    let price = rest_buy_price(&rig, top(&rig, &mcp).await);
    let id = place_resting_buy(&rig, &mcp, price).await;

    let open = mcp.ok("my_orders", json!({"status": "open"})).await;
    let o = open["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["orderId"] == id.as_str())
        .unwrap_or_else(|| panic!("my_orders lacks {id}: {open}"))
        .clone();
    assert_eq!(o["state"], "open", "{o}");
    assert_eq!(raw(&o, "remainingRaw"), Some(rig.qty), "{o}");

    assert_eq!(cancel(&mcp, &id).await["result"], "cancelled");
    let g = mcp.ok("get_order", json!({"order_id": id})).await;
    assert_eq!(g["state"], "cancelled", "{g}");
    assert_eq!(cancel(&mcp, &id).await["result"], "already_gone");
    mcp.stop().await;
}

/// 4. A crossing order from a second wallet fills a resting order (`filled`,
///    with the fill), and `place_marketable_order` takes liquidity and shows
///    its taker fill.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s4_crossing_fill_and_marketable() {
    let rig = Rig::load().await;
    let (_s, mcp) = wide_session(&rig).await;
    let mut cleanup: Vec<[u8; 32]> = Vec::new();

    // An ask must exist: the resting bid goes under it, the marketable buy takes it.
    let (bid, ask) = top(&rig, &mcp).await;
    if ask.is_none() {
        let a = bid.map_or(rig.default_price, |b| b * 11 / 10);
        cleanup.push(rig.counter_send(WireSide::Ask, rig.qty, a, true).await);
    }
    let (bid, ask) = top(&rig, &mcp).await;
    let ask = ask.expect("no ask after the counterparty posted one");
    // Strictly the best bid, so the counterparty's sell fills this order.
    let price = match bid {
        Some(b) => {
            let p = b + (ask - b) / 2;
            assert!(
                p > b && p < ask,
                "spread {b}..{ask} has no room for a best bid"
            );
            p
        }
        None => ask * 9 / 10,
    };
    let id = place_resting_buy(&rig, &mcp, price).await;

    rig.counter_send(WireSide::Ask, rig.qty, price, false).await;
    let m = &mcp;
    let i = id.as_str();
    let g = poll(60, move || async move {
        let g = m.ok("get_order", json!({"order_id": i})).await;
        (g["state"] == "filled").then_some(g)
    })
    .await
    .expect("the resting order was never filled");
    assert_eq!(raw(&g, "filledRaw"), Some(rig.qty), "{g}");
    let fills = g["fills"].as_array().unwrap();
    assert!(!fills.is_empty(), "filled with no fills: {g}");
    let sum: u128 = fills.iter().map(|f| raw(f, "qtyRaw").unwrap()).sum();
    assert_eq!(sum, rig.qty, "{g}");
    assert!(
        fills.iter().all(|f| raw(f, "priceRaw") == Some(price)),
        "{g}"
    );
    let my = mcp.ok("my_fills", json!({"market_id": rig.market})).await;
    assert!(
        my["fills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["role"] == "maker" && f["orderId"] == id.as_str()),
        "my_fills lacks the maker fill of {id}: {my}"
    );

    // Liquidity to take: the counterparty joins the best ask.
    let (_, ask) = top(&rig, &mcp).await;
    let ask = ask.expect("no ask to take");
    cleanup.push(rig.counter_send(WireSide::Ask, rig.qty, ask, true).await);
    let o = mcp
        .ok(
            "place_marketable_order",
            json!({"market_id": rig.market, "side": "buy", "qty": rig.s(rig.qty), "slippage_bps": 50}),
        )
        .await;
    let taker_id = o["orderId"].as_str().expect("orderId").to_string();
    let tf = o["fills"].as_array().unwrap();
    assert!(!tf.is_empty(), "the marketable order took nothing: {o}");
    assert!(
        matches!(o["state"].as_str(), Some("filled" | "partially_filled")),
        "{o}"
    );
    assert!(raw(&o, "priceRaw").unwrap() >= ask, "{o}");
    let my = mcp.ok("my_fills", json!({"market_id": rig.market})).await;
    assert!(
        my["fills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["role"] == "taker" && f["orderId"] == taker_id.as_str()),
        "my_fills lacks the taker fill of {taker_id}: {my}"
    );

    // Leave nothing resting.
    if o["state"] == "partially_filled" {
        cancel(&mcp, &taker_id).await;
    }
    for c in cleanup {
        rig.counter_cancel("sell", c).await;
    }
    mcp.stop().await;
}

/// 5. Refusals, each for its own reason only, with the trader's venue balance unchanged.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s5_refusals_each_for_its_own_reason() {
    let rig = Rig::load().await;
    let (s, mcp) = wide_session(&rig).await;
    let t = top(&rig, &mcp).await;
    let r = reference(&rig, t);
    let buy_p = rest_buy_price(&rig, t);
    let sell_p = rest_sell_price(&rig, t);
    let a = rig.quote(rig.qty, buy_p);
    let wide = Limits::wide(&rig, r);
    mcp.stop().await;
    let order = |side: &str, qty: u128, price: u128| {
        json!({"market_id": rig.market, "side": side, "qty": rig.s(qty), "price": rig.s(price),
               "post_only": true})
    };

    // Each case gets its own process and state_dir, so usage starts at zero.
    // a) Order over max_order_qty (a sell: no quote rule applies).
    let s_a = Session::new(
        &rig,
        &Opts::new(Limits {
            order_qty: rig.qty,
            ..wide.clone()
        }),
    );
    let mcp = s_a.start().await;
    let args = order("sell", rig.qty * 2, sell_p);
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "max_order_qty exceeded",
    )
    .await;
    mcp.stop().await;

    // b) Daily qty exhausted by an order that was then cancelled.
    let s_b = Session::new(
        &rig,
        &Opts::new(Limits {
            order_qty: rig.qty * 2,
            daily_qty: rig.qty * 3,
            ..wide.clone()
        }),
    );
    let mcp = s_b.start().await;
    let o = mcp
        .ok("place_limit_order", order("sell", rig.qty * 2, sell_p))
        .await;
    let id = assert_open(&o);
    assert_eq!(cancel(&mcp, &id).await["result"], "cancelled");
    let args = order("sell", rig.qty * 2, sell_p);
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "max_daily_qty exceeded",
    )
    .await;
    mcp.stop().await;

    // c) Buy over max_order_quote.
    let s_c = Session::new(
        &rig,
        &Opts::new(Limits {
            order_quote: a - 1,
            ..wide.clone()
        }),
    );
    let mcp = s_c.start().await;
    let args = order("buy", rig.qty, buy_p);
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "max_order_quote exceeded",
    )
    .await;
    mcp.stop().await;

    // d) Daily quote exhausted by a cancelled buy: 1.5 orders' worth.
    let s_d = Session::new(
        &rig,
        &Opts::new(Limits {
            daily_quote: a + a / 2,
            ..wide.clone()
        }),
    );
    let mcp = s_d.start().await;
    let id = place_resting_buy(&rig, &mcp, buy_p).await;
    assert_eq!(cancel(&mcp, &id).await["result"], "cancelled");
    let args = order("buy", rig.qty, buy_p);
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "max_daily_quote exceeded",
    )
    .await;
    mcp.stop().await;

    // e) Price beyond max_deviation_bps: a buy 2x the band above the best ask.
    let s_e = Session::new(
        &rig,
        &Opts::new(Limits {
            dev_bps: 100,
            ..wide.clone()
        }),
    );
    let mcp = s_e.start().await;
    let mut posted = None;
    if top(&rig, &mcp).await.1.is_none() {
        posted = Some(rig.counter_send(WireSide::Ask, rig.qty, sell_p, true).await);
    }
    let ask = top(&rig, &mcp).await.1.expect("no ask for the price guard");
    let through = ask + ask * 200 / 10_000 + 1;
    let args = json!({"market_id": rig.market, "side": "buy", "qty": rig.s(rig.qty),
                      "price": rig.s(through)});
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "max_deviation_bps exceeded",
    )
    .await;
    if let Some(id) = posted {
        rig.counter_cancel("sell", id).await;
    }
    mcp.stop().await;

    // f) A settle address that is not in the allowlist.
    let s_f = Session::new(
        &rig,
        &Opts {
            allowlist: vec!["0x000000000000000000000000000000000000bEEF".into()],
            ..Opts::new(wide.clone())
        },
    );
    let mcp = s_f.start().await;
    let mut args = order("buy", rig.qty, buy_p);
    args["settle_base_address"] = json!("0x000000000000000000000000000000000000dEaD");
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "is not in settle_address_allowlist",
    )
    .await;

    // g) A market with no [markets] block.
    let cfg = mcp.ok("venue_config", json!({})).await;
    let unlisted = cfg["markets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["marketId"].as_str())
        .find(|id| *id != rig.market)
        .map_or_else(|| format!("{}-unlisted", rig.market), str::to_string);
    let mut args = order("buy", rig.qty, buy_p);
    args["market_id"] = json!(unlisted);
    assert_refusal(
        &rig,
        &mcp,
        "place_limit_order",
        args,
        "order",
        "is not configured for trading",
    )
    .await;
    mcp.stop().await;
    drop(s);
}

/// 6. `halt` refuses an order and a deposit but not a cancel, survives a
///    restart, and `aspens-mcp resume` lifts it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s6_halt_survives_restart_until_resume() {
    let rig = Rig::load().await;
    let (s, mcp) = wide_session(&rig).await;
    let price = rest_buy_price(&rig, top(&rig, &mcp).await);
    let resting = place_resting_buy(&rig, &mcp, price).await;
    let buy = json!({"market_id": rig.market, "side": "buy", "qty": rig.s(rig.qty),
                     "price": rig.s(price), "post_only": true});
    let dep =
        json!({"network": rig.quote_network, "token": rig.quote_token, "amount": rig.transfer});

    let h = mcp.ok("halt", json!({"reason": "e2e halt"})).await;
    assert_eq!(h["halted"], true, "{h}");
    let rule = "trading is halted: e2e halt";
    assert_refusal(&rig, &mcp, "place_limit_order", buy.clone(), "order", rule).await;
    assert_refusal(&rig, &mcp, "deposit", dep, "deposit", rule).await;
    assert_eq!(cancel(&mcp, &resting).await["result"], "cancelled");
    mcp.stop().await;

    // Restart: still halted, with its reason.
    let mcp = s.start().await;
    let st = mcp.ok("account_status", json!({})).await;
    assert_eq!(st["halted"], true, "{st}");
    assert_eq!(st["haltReason"], "e2e halt", "{st}");
    assert_refusal(&rig, &mcp, "place_limit_order", buy.clone(), "order", rule).await;

    // resume refuses while the server holds the ledger.
    let out = s.resume().await;
    assert!(!out.status.success(), "resume ran beside a live server");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stop the running aspens-mcp"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    mcp.stop().await;

    let out = s.resume().await;
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "resume failed: {err}");
    assert!(err.contains("resumed trading"), "{err}");

    let mcp = s.start().await;
    let st = mcp.ok("account_status", json!({})).await;
    assert_eq!(st["halted"], false, "{st}");
    let id = place_resting_buy(&rig, &mcp, price).await;
    assert_eq!(cancel(&mcp, &id).await["result"], "cancelled");
    mcp.stop().await;
}

/// 7. A restart leaves today's usage unchanged.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s7_restart_keeps_usage() {
    let rig = Rig::load().await;
    let (s, mcp) = wide_session(&rig).await;
    let price = rest_buy_price(&rig, top(&rig, &mcp).await);
    let id = place_resting_buy(&rig, &mcp, price).await;
    assert_eq!(cancel(&mcp, &id).await["result"], "cancelled");

    let usage = |l: &Value| {
        let m = l["markets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["marketId"] == rig.market.as_str())
            .unwrap()
            .clone();
        (
            raw(&m["usedToday"], "qtyRaw"),
            raw(&m["usedToday"], "quoteRaw"),
        )
    };
    let before = usage(&mcp.ok("limits", json!({})).await);
    assert_eq!(before, (Some(rig.qty), Some(rig.quote(rig.qty, price))));
    mcp.stop().await;

    let mcp = s.start().await;
    assert_eq!(usage(&mcp.ok("limits", json!({})).await), before);
    let all = mcp.ok("my_orders", json!({"status": "all"})).await;
    assert!(
        all["orders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["orderId"] == id.as_str() && o["state"] == "cancelled"),
        "{all}"
    );
    mcp.stop().await;
}

/// 8. `withdraw` is absent by default; with `allow_withdraw = true` a
///    withdrawal lands on chain.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the local rig; see the header"]
async fn s8_withdraw_gated_and_lands() {
    let rig = Rig::load().await;
    let (s, mcp) = wide_session(&rig).await;
    assert!(!mcp.tools().await.iter().any(|t| t == "withdraw"));
    let args =
        json!({"network": rig.quote_network, "token": rig.quote_token, "amount": rig.transfer});
    let r = mcp.call("withdraw", args.clone()).await;
    assert!(
        r.as_ref().map_or(true, |r| r.is_error == Some(true)),
        "withdraw ran while not registered: {r:?}"
    );
    let r = reference(&rig, top(&rig, &mcp).await);
    mcp.stop().await;

    s.configure(
        &rig,
        &Opts {
            allow_withdraw: true,
            ..Opts::new(Limits::wide(&rig, r))
        },
    );
    let mcp = s.start().await;
    assert!(mcp.tools().await.iter().any(|t| t == "withdraw"));
    let amount = parse_decimal_amount(&rig.transfer, rig.quote_token_decimals).unwrap();
    let key = format!("{}@{}", rig.quote_token, rig.quote_network);
    let venue_before = stable_balances(&rig, &mcp).await[&key].0;
    let wallet_before = wallet_balance(&mcp, &rig.quote_network, &rig.quote_token).await;

    let w = mcp.ok("withdraw", args).await;
    assert_eq!(raw(&w, "amountRaw"), Some(amount), "{w}");
    let m = &mcp;
    let r2 = &rig;
    let landed = poll(90, move || async move {
        r2.mine().await;
        let now = wallet_balance(m, &r2.quote_network, &r2.quote_token).await;
        (now != wallet_before).then_some(now)
    })
    .await
    .expect("the withdrawal never landed in the wallet");
    assert_eq!(
        landed,
        wallet_before + amount,
        "wallet moved by the wrong amount"
    );
    let venue_after = stable_balances(&rig, &mcp).await[&key].0;
    assert_eq!(
        venue_after,
        venue_before - amount,
        "venue balance moved by the wrong amount"
    );
    mcp.stop().await;
}

/// Runs without the rig: the loopback guard accepts local hosts only.
#[test]
fn loopback_guard_accepts_only_local_hosts() {
    for ok in [
        "http://localhost:50051",
        "http://127.0.0.1:8545",
        "http://[::1]:8545",
    ] {
        assert_loopback("url", ok);
    }
    for bad in [
        "https://beta-1.axby.fi",
        "http://10.0.0.5:8545",
        "https://base-mainnet.g.alchemy.com/v2/KEY",
        "http://localhost.evil.example:50051",
        "not a url",
    ] {
        // Quiet the expected panics' messages.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = std::panic::catch_unwind(|| assert_loopback("url", bad));
        std::panic::set_hook(hook);
        assert!(r.is_err(), "{bad} passed the loopback guard");
    }
}
