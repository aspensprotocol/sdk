//! The MCP surface, driven end to end: an rmcp client talks to `Server<FakeVenue>`
//! over an in-process duplex pipe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use aspens::commands::config::config_pb::{Chain, Configuration, GetConfigResponse, Market};
use aspens::commands::trading::arborter_pb::{self, OrderbookEntry};
use aspens::commands::trading::balance::{
    BalanceValue, Balances, ChainBalanceView, NativeBalanceView, TokenBalances,
};
use aspens::commands::trading::book_snapshot::BookState;
use aspens_mcp::config::{Config, DepositsConfig, MarketLimits};
use aspens_mcp::fake::{Call, FakeVenue};
use aspens_mcp::ledger::{Fill, Ledger, Record, SentOutcome, SentRecord, Side};
use aspens_mcp::limits::utc_day;
use aspens_mcp::server::Server;
use aspens_mcp::venue::{Addresses, TradeFill};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

const MARKET: &str = "base::0xaaa::base::0xbbb";
const AGENT: &str = "0xagent";
const PAIR_DECIMALS: u32 = 6;

const READ_TOOLS: [&str; 10] = [
    "venue_config",
    "top_of_book",
    "orderbook",
    "balances",
    "my_orders",
    "get_order",
    "my_fills",
    "account_status",
    "limits",
    "venue_attestation",
];

fn id(b: u8) -> [u8; 32] {
    [b; 32]
}

fn id_hex(b: u8) -> String {
    format!("0x{}", hex::encode(id(b)))
}

fn venue_config() -> GetConfigResponse {
    GetConfigResponse {
        config: Some(Configuration {
            chains: vec![Chain {
                network: "base".into(),
                architecture: "evm".into(),
                chain_id: 8453,
                ..Default::default()
            }],
            markets: vec![Market {
                name: "AAA/BBB".into(),
                market_id: MARKET.into(),
                base_chain_network: "base".into(),
                quote_chain_network: "base".into(),
                base_chain_token_symbol: "AAA".into(),
                quote_chain_token_symbol: "BBB".into(),
                base_chain_token_decimals: 18,
                quote_chain_token_decimals: 8,
                pair_decimals: PAIR_DECIMALS as i32,
            }],
            session: 1,
        }),
    }
}

fn fake() -> FakeVenue {
    let v = FakeVenue::new(venue_config());
    v.set_addresses(Addresses {
        evm: Some(AGENT.into()),
        solana: None,
    });
    v
}

fn limits() -> BTreeMap<String, MarketLimits> {
    BTreeMap::from([(
        MARKET.to_string(),
        MarketLimits {
            pair_decimals: PAIR_DECIMALS,
            max_order_qty: 1_500_000,
            max_daily_qty: 7_000_000,
            max_order_quote: 25_000_000,
            max_daily_quote: 250_000_000,
            max_deviation_bps: 100,
        },
    )])
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

struct Harness {
    client: RunningService<RoleClient, ()>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let mut p = CallToolRequestParams::new(tool.to_string());
        if let Value::Object(m) = args {
            p = p.with_arguments(m);
        }
        self.client.call_tool(p).await.unwrap()
    }

    /// Call `tool` and parse its JSON text, asserting it is not an error.
    async fn ok(&self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args).await;
        let t = text(&r);
        assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
        serde_json::from_str(&t).unwrap_or_else(|e| panic!("{tool}: not JSON ({e}): {t}"))
    }
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Start the server with `seed` applied to a fresh ledger.
async fn start(venue: FakeVenue, seed: impl FnOnce(&mut Ledger)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(dir.path()).unwrap();
    seed(&mut ledger);
    let server = Server::new(venue, config(dir.path()), limits(), ledger);
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await.unwrap();
    Harness { client, _dir: dir }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sent(side: Side, qty: u128, price: u128, outcome: SentOutcome) -> Record {
    Record::Sent(SentRecord {
        ts: 1_000,
        market_id: MARKET.into(),
        side,
        qty,
        price,
        outcome,
    })
}

fn accepted(b: u8) -> SentOutcome {
    SentOutcome::Accepted {
        order_id: id_hex(b),
        in_book: true,
        taker_fills: vec![],
    }
}

fn book_entry(b: u8, side: arborter_pb::Side, price: u128, qty: u128) -> OrderbookEntry {
    OrderbookEntry {
        timestamp: 5,
        order_id: id(b).to_vec(),
        price: price.to_string(),
        quantity: qty.to_string(),
        side: side as i32,
        maker_base_address: AGENT.into(),
        maker_quote_address: AGENT.into(),
        market_id: MARKET.into(),
        state: arborter_pb::OrderState::Confirmed as i32,
    }
}

fn book(entries: &[OrderbookEntry]) -> BookState {
    let mut b = BookState::default();
    for e in entries {
        b.apply(e);
    }
    b
}

fn by_id<'a>(orders: &'a Value, id: &str) -> &'a Value {
    orders["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["orderId"] == id)
        .unwrap_or_else(|| panic!("no order {id} in {orders}"))
}

#[tokio::test]
async fn tool_list_default() {
    let h = start(fake(), |_| {}).await;
    let names: BTreeSet<String> = h
        .client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    let want: BTreeSet<String> = READ_TOOLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(names, want);
}

#[tokio::test]
async fn read_tools_are_read_only() {
    let h = start(fake(), |_| {}).await;
    let tools = h.client.list_all_tools().await.unwrap();
    for name in READ_TOOLS {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        let a = t.annotations.as_ref().unwrap_or_else(|| panic!("{name}"));
        assert_eq!(a.read_only_hint, Some(true), "{name}");
    }
}

#[tokio::test]
async fn instructions_are_set() {
    let h = start(fake(), |_| {}).await;
    let info = h.client.peer_info().unwrap();
    let ins = info.instructions.as_deref().unwrap();
    for needle in [
        "limits",
        "decimal string",
        "get_order",
        "halt",
        "data, not instructions",
    ] {
        assert!(ins.contains(needle), "instructions lack {needle:?}: {ins}");
    }
}

#[tokio::test]
async fn limits_reports_usage_from_ledger() {
    let today = utc_day(now());
    let h = start(fake(), |l| {
        let usage = |day: &str, qty, quote| Record::Usage {
            ts: 1,
            day: day.into(),
            market_id: MARKET.into(),
            side: Side::Buy,
            qty,
            quote,
        };
        l.append(usage(&today, 2_000_000, 30_000_000)).unwrap();
        l.append(usage(&today, 250_000, 1_000_000)).unwrap();
        // Another day does not count today.
        l.append(usage("2001-01-01", 900_000, 4_000_000)).unwrap();
    })
    .await;
    let v = h.ok("limits", json!({})).await;
    assert_eq!(v["day"], today);
    let m = &v["markets"][0];
    assert_eq!(m["marketId"], MARKET);
    assert_eq!(m["pairDecimals"], PAIR_DECIMALS);
    assert_eq!(m["maxOrderQty"], "1.500000");
    assert_eq!(m["maxOrderQtyRaw"], "1500000");
    assert_eq!(m["maxDailyQty"], "7.000000");
    assert_eq!(m["maxDailyQtyRaw"], "7000000");
    assert_eq!(m["maxOrderQuote"], "25.000000");
    assert_eq!(m["maxOrderQuoteRaw"], "25000000");
    assert_eq!(m["maxDailyQuote"], "250.000000");
    assert_eq!(m["maxDailyQuoteRaw"], "250000000");
    assert_eq!(m["maxDeviationBps"], 100);
    assert_eq!(m["usedToday"]["qty"], "2.250000");
    assert_eq!(m["usedToday"]["qtyRaw"], "2250000");
    assert_eq!(m["usedToday"]["quote"], "31.000000");
    assert_eq!(m["usedToday"]["quoteRaw"], "31000000");
    assert_eq!(m["remainingToday"]["qtyRaw"], "4750000");
    assert_eq!(m["remainingToday"]["quoteRaw"], "219000000");
}

#[tokio::test]
async fn errors_are_redacted() {
    let v = fake();
    v.set_read_error(Some("transport error: https://rpc.example/KEY failed"));
    let h = start(v, |_| {}).await;
    for (tool, args) in [
        ("top_of_book", json!({"market_id": MARKET})),
        ("orderbook", json!({"market_id": MARKET})),
        ("balances", json!({})),
        ("my_fills", json!({"market_id": MARKET})),
        ("venue_attestation", json!({})),
    ] {
        let r = h.call(tool, args).await;
        let t = text(&r);
        assert_eq!(r.is_error, Some(true), "{tool}: {t}");
        assert!(t.contains("<url>"), "{tool}: {t}");
        assert!(!t.contains("KEY"), "{tool}: {t}");
        assert!(!t.contains("rpc.example"), "{tool}: {t}");
    }
}

#[tokio::test]
async fn balance_error_text_is_redacted() {
    let v = fake();
    v.set_balances(Balances {
        tokens: vec![TokenBalances {
            symbol: "BBB".into(),
            chains: vec![ChainBalanceView {
                network: "base".into(),
                decimals: 8,
                wallet: BalanceValue::Error("rpc https://rpc.example/KEY: 429".into()),
                available: BalanceValue::Raw(1_500_000_000),
                locked: BalanceValue::NoWallet,
            }],
        }],
        native: vec![NativeBalanceView {
            network: "base".into(),
            decimals: Some(18),
            balance: BalanceValue::Error("wss://rpc.example/OTHERKEY closed".into()),
        }],
    });
    let h = start(v, |_| {}).await;
    let r = h.call("balances", json!({})).await;
    let t = text(&r);
    assert!(!t.contains("KEY"), "{t}");
    assert!(t.contains("<url>"), "{t}");
    let b: Value = serde_json::from_str(&t).unwrap();
    let c = &b["tokens"][0]["chains"][0];
    assert_eq!(c["walletError"], "rpc <url> 429");
    assert_eq!(c["wallet"], Value::Null);
    assert_eq!(c["available"], "15.00000000");
    assert_eq!(c["availableRaw"], "1500000000");
    assert_eq!(c["lockedError"], "no wallet");
    assert_eq!(b["native"][0]["balanceError"], "<url> closed");
}

#[tokio::test]
async fn my_orders_derives_states() {
    let v = fake();
    // A rests in the agent's book; B was filled as maker; C is gone with no fills.
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[book_entry(
            0xaa,
            arborter_pb::Side::Bid,
            2_500_000,
            1_200_000,
        )]),
    );
    v.set_fills(
        MARKET,
        AGENT,
        vec![
            TradeFill {
                order_hit: Some(id_hex(0xbb)),
                qty: 400_000,
                price: 2_600_000,
                ts: 7,
                maker: true,
            },
            TradeFill {
                order_hit: Some(id_hex(0xbb)),
                qty: 600_000,
                price: 2_600_000,
                ts: 8,
                maker: true,
            },
        ],
    );
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_200_000, 2_500_000, accepted(0xaa)))
            .unwrap();
        l.append(sent(Side::Sell, 1_000_000, 2_600_000, accepted(0xbb)))
            .unwrap();
        l.append(sent(Side::Buy, 800_000, 2_400_000, accepted(0xcc)))
            .unwrap();
    })
    .await;

    let all = h.ok("my_orders", json!({"status": "all"})).await;
    let a = by_id(&all, &id_hex(0xaa));
    assert_eq!(a["state"], "open");
    assert_eq!(a["remainingRaw"], "1200000");
    assert_eq!(a["remaining"], "1.200000");
    assert_eq!(a["price"], "2.500000");
    assert_eq!(a["side"], "buy");
    let b = by_id(&all, &id_hex(0xbb));
    assert_eq!(b["state"], "filled");
    assert_eq!(b["filledRaw"], "1000000");
    assert_eq!(b["fills"].as_array().unwrap().len(), 2);
    let c = by_id(&all, &id_hex(0xcc));
    assert_eq!(c["state"], "closed_unattributed");

    // The default filter is open orders only.
    let open = h.ok("my_orders", json!({})).await;
    let ids: Vec<&str> = open["orders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["orderId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![id_hex(0xaa)]);

    let closed = h.ok("my_orders", json!({"status": "closed"})).await;
    assert_eq!(closed["orders"].as_array().unwrap().len(), 2);

    let one = h.ok("get_order", json!({"order_id": id_hex(0xbb)})).await;
    assert_eq!(one["state"], "filled");
    // Upper-case input names the same order.
    let upper = format!("0x{}", "BB".repeat(32));
    let one = h.ok("get_order", json!({"order_id": upper})).await;
    assert_eq!(one["orderId"], id_hex(0xbb));
}

#[tokio::test]
async fn unknown_send_resolves_from_book() {
    let v = fake();
    // 0xaa is already a known order; 0xee is the only fresh same-side match.
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[
            book_entry(0xaa, arborter_pb::Side::Ask, 2_450_000, 700_000),
            book_entry(0xee, arborter_pb::Side::Ask, 2_450_000, 700_000),
            // Same price and size on the other side: not a candidate for a sell.
            book_entry(0xdd, arborter_pb::Side::Bid, 2_450_000, 700_000),
        ]),
    );
    let h = start(v, |l| {
        l.append(sent(Side::Sell, 700_000, 2_450_000, accepted(0xaa)))
            .unwrap();
        l.append(sent(Side::Sell, 700_000, 2_450_000, SentOutcome::Unknown))
            .unwrap();
    })
    .await;
    let all = h.ok("my_orders", json!({"status": "all"})).await;
    let e = by_id(&all, &id_hex(0xee));
    assert_eq!(e["state"], "open");
    assert_eq!(all["orders"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn unresolved_unknown_stays_unknown() {
    let h = start(fake(), |l| {
        l.append(sent(Side::Buy, 700_000, 2_450_000, SentOutcome::Unknown))
            .unwrap();
    })
    .await;
    let open = h.ok("my_orders", json!({})).await;
    let o = &open["orders"][0];
    assert_eq!(o["state"], "unknown");
    assert_eq!(o["orderId"], Value::Null);
}

#[tokio::test]
async fn my_fills_merges_ledger_taker_fills() {
    let v = fake();
    v.set_fills(
        MARKET,
        AGENT,
        vec![
            // The ledger's own taker fill, seen again on the venue: one fill.
            TradeFill {
                order_hit: Some(id_hex(0x11)),
                qty: 300_000,
                price: 2_490_000,
                ts: 20,
                maker: false,
            },
            TradeFill {
                order_hit: Some(id_hex(0xaa)),
                qty: 100_000,
                price: 2_500_000,
                ts: 10,
                maker: true,
            },
        ],
    );
    let h = start(v, |l| {
        l.append(sent(
            Side::Buy,
            1_200_000,
            2_500_000,
            SentOutcome::Accepted {
                order_id: id_hex(0xaa),
                in_book: true,
                taker_fills: vec![Fill {
                    qty: 300_000,
                    price: 2_490_000,
                    ts: 20,
                }],
            },
        ))
        .unwrap();
    })
    .await;
    let f = h.ok("my_fills", json!({"market_id": MARKET})).await;
    let fills = f["fills"].as_array().unwrap();
    assert_eq!(fills.len(), 2, "{f}");
    // Newest last.
    assert_eq!(fills[0]["ts"], 10);
    assert_eq!(fills[0]["role"], "maker");
    assert_eq!(fills[0]["side"], "buy");
    assert_eq!(fills[0]["orderId"], id_hex(0xaa));
    assert_eq!(fills[1]["ts"], 20);
    assert_eq!(fills[1]["role"], "taker");
    assert_eq!(fills[1]["side"], "buy");
    assert_eq!(fills[1]["qty"], "0.300000");
    assert_eq!(fills[1]["priceRaw"], "2490000");
}

#[tokio::test]
async fn top_of_book_and_orderbook() {
    let v = fake();
    v.set_book(
        MARKET,
        None,
        book(&[
            book_entry(0x01, arborter_pb::Side::Bid, 2_400_000, 100_000),
            book_entry(0x02, arborter_pb::Side::Bid, 2_400_000, 200_000),
            book_entry(0x03, arborter_pb::Side::Bid, 2_300_000, 50_000),
            book_entry(0x04, arborter_pb::Side::Ask, 2_600_000, 70_000),
        ]),
    );
    let h = start(v, |_| {}).await;
    let t = h.ok("top_of_book", json!({"market_id": MARKET})).await;
    assert_eq!(t["bestBid"], "2.400000");
    assert_eq!(t["bestBidRaw"], "2400000");
    assert_eq!(t["bestAsk"], "2.600000");
    assert_eq!(t["bestAskRaw"], "2600000");

    let b = h
        .ok("orderbook", json!({"market_id": MARKET, "levels": 1}))
        .await;
    let bids = b["bids"].as_array().unwrap();
    assert_eq!(bids.len(), 1);
    assert_eq!(bids[0]["priceRaw"], "2400000");
    assert_eq!(bids[0]["qtyRaw"], "300000");
    assert_eq!(bids[0]["orders"], 2);
    assert_eq!(b["asks"][0]["qty"], "0.070000");

    let unknown = h.call("top_of_book", json!({"market_id": "nope"})).await;
    assert_eq!(unknown.is_error, Some(true));
}

#[tokio::test]
async fn account_status_and_venue_config() {
    let v = fake();
    v.set_healthy(false);
    let h = start(v, |l| {
        l.append(Record::Halt {
            ts: 1,
            reason: Some("drawdown".into()),
        })
        .unwrap();
    })
    .await;
    let s = h.ok("account_status", json!({})).await;
    assert_eq!(s["halted"], true);
    assert_eq!(s["haltReason"], "drawdown");
    assert_eq!(s["venuePinned"], false);
    assert_eq!(s["addresses"]["evm"], AGENT);
    assert_eq!(s["addresses"]["solana"], Value::Null);
    assert_eq!(s["keys"]["evm"], true);
    assert_eq!(s["keys"]["solana"], false);
    assert_eq!(s["venue"]["reachable"], false);

    let c = h.ok("venue_config", json!({})).await;
    let m = &c["markets"][0];
    assert_eq!(m["marketId"], MARKET);
    assert_eq!(m["tradable"], true);
    assert_eq!(m["pairDecimals"], PAIR_DECIMALS);
    assert_eq!(m["base"]["decimals"], 18);
    assert_eq!(m["quote"]["decimals"], 8);
    assert_eq!(m["quote"]["token"], "BBB");
    assert_eq!(c["chains"][0]["network"], "base");
}

#[tokio::test]
async fn reads_never_send_or_cancel() {
    let v = fake();
    let probe = v.clone();
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_200_000, 2_500_000, accepted(0xaa)))
            .unwrap();
        l.append(sent(Side::Buy, 700_000, 2_450_000, SentOutcome::Unknown))
            .unwrap();
    })
    .await;
    for (tool, args) in [
        ("venue_config", json!({})),
        ("top_of_book", json!({"market_id": MARKET})),
        ("orderbook", json!({"market_id": MARKET})),
        ("balances", json!({})),
        ("my_orders", json!({"status": "all"})),
        ("get_order", json!({"order_id": id_hex(0xaa)})),
        ("my_fills", json!({"market_id": MARKET})),
        ("account_status", json!({})),
        ("limits", json!({})),
        ("venue_attestation", json!({})),
    ] {
        h.ok(tool, args).await;
    }
    let calls = probe.calls();
    assert!(calls.contains(&Call::Attestation));
    assert!(
        !calls.iter().any(|c| matches!(
            c,
            Call::SendLimit { .. }
                | Call::Cancel { .. }
                | Call::Deposit { .. }
                | Call::Withdraw { .. }
        )),
        "{calls:?}"
    );
}

#[tokio::test]
async fn unknown_buy_resolves_from_bids_only() {
    let v = fake();
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[
            book_entry(0xa1, arborter_pb::Side::Ask, 2_450_000, 700_000),
            book_entry(0xb1, arborter_pb::Side::Bid, 2_450_000, 700_000),
        ]),
    );
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 700_000, 2_450_000, SentOutcome::Unknown))
            .unwrap();
    })
    .await;
    let all = h.ok("my_orders", json!({"status": "all"})).await;
    assert_eq!(all["orders"][0]["orderId"], id_hex(0xb1));
    assert_eq!(all["orders"][0]["state"], "open");
}

#[tokio::test]
async fn two_unknowns_never_share_one_order() {
    let v = fake();
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[book_entry(0xb1, arborter_pb::Side::Bid, 2_450_000, 700_000)]),
    );
    let h = start(v, |l| {
        for _ in 0..2 {
            l.append(sent(Side::Buy, 700_000, 2_450_000, SentOutcome::Unknown))
                .unwrap();
        }
    })
    .await;
    let all = h.ok("my_orders", json!({"status": "all"})).await;
    let orders = all["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 2);
    for o in orders {
        assert_eq!(o["state"], "unknown", "{o}");
        assert_eq!(o["orderId"], Value::Null);
    }
}
