//! The MCP surface, driven end to end: an rmcp client talks to `Server<FakeVenue>`
//! over an in-process duplex pipe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use aspens::commands::config::config_pb::{
    Chain, Configuration, GetConfigResponse, Market, Token, TradeContract,
};
use aspens::commands::trading::arborter_pb::{self, OrderbookEntry};
use aspens::commands::trading::balance::{
    BalanceValue, Balances, ChainBalanceView, NativeBalanceView, TokenBalances,
};
use aspens::commands::trading::book_snapshot::BookState;
use aspens_mcp::config::{Config, DepositsConfig, MarketLimits, VenuePin};
use aspens_mcp::fake::{Call, FakeVenue};
use aspens_mcp::ledger::{Fill, Ledger, Record, SentOutcome, SentRecord, Side};
use aspens_mcp::limits::utc_day;
use aspens_mcp::server::Server;
use aspens_mcp::venue::{Addresses, AttestationView, CancelResult, SendResult, TradeFill};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

const MARKET: &str = "base::0xaaa::base::0xbbb";
const AGENT: &str = "0xagent";
const PAIR_DECIMALS: u32 = 6;
/// The test venue config's session: the identity a write records.
const SESSION: u64 = 41;
/// Token addresses as the venue spells them: mixed case, so a case-sensitive
/// comparison is caught.
const AAA_ADDR: &str = "0xAaAa00000000000000000000000000000000Aa01";
const BBB_ADDR: &str = "0xBbBb00000000000000000000000000000000Bb02";
const TRADE_CONTRACT: &str = "0xC0c0000000000000000000000000000000C0c003";

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

const WRITE_TOOLS: [&str; 5] = [
    "place_limit_order",
    "place_marketable_order",
    "cancel_order",
    "deposit",
    "halt",
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
                trade_contract: Some(TradeContract {
                    address: TRADE_CONTRACT.into(),
                }),
                tokens: [("AAA", AAA_ADDR, 18), ("BBB", BBB_ADDR, 8)]
                    .into_iter()
                    .map(|(symbol, address, decimals)| {
                        let t = Token {
                            name: symbol.into(),
                            symbol: symbol.into(),
                            address: address.into(),
                            decimals,
                        };
                        (symbol.to_string(), t)
                    })
                    .collect(),
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
            session: SESSION,
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
    dir: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Close the client and wait for the server to exit, releasing the ledger.
    async fn stop(self) -> tempfile::TempDir {
        self.client.cancel().await.unwrap();
        self.server.await.unwrap();
        self.dir
    }

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
    start_with(venue, |_| {}, seed).await
}

/// Start the server with `edit` applied to the config and `seed` to a fresh ledger.
async fn start_with(
    venue: FakeVenue,
    edit: impl FnOnce(&mut Config),
    seed: impl FnOnce(&mut Ledger),
) -> Harness {
    start_in(tempfile::tempdir().unwrap(), venue, edit, seed).await
}

/// Start the server on the ledger in `dir` (kept as it is, then `seed`ed).
async fn start_in(
    dir: tempfile::TempDir,
    venue: FakeVenue,
    edit: impl FnOnce(&mut Config),
    seed: impl FnOnce(&mut Ledger),
) -> Harness {
    let mut ledger = Ledger::open(dir.path()).unwrap();
    seed(&mut ledger);
    let mut cfg = config(dir.path());
    edit(&mut cfg);
    let server = Server::new(venue, cfg, limits(), ledger);
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server = tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await.unwrap();
    Harness {
        client,
        dir,
        server,
    }
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
    let want: BTreeSet<String> = READ_TOOLS
        .iter()
        .chain(&WRITE_TOOLS)
        .map(|s| s.to_string())
        .collect();
    assert_eq!(names.len(), 15);
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
            price: 2_500_000,
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
    // Remaining differs from the order's qty, so echoing qty is caught.
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[book_entry(0xaa, arborter_pb::Side::Bid, 2_500_000, 900_000)]),
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
    assert_eq!(a["remainingRaw"], "900000");
    assert_eq!(a["remaining"], "0.900000");
    assert_eq!(a["qtyRaw"], "1200000");
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

#[tokio::test]
async fn order_read_errors_are_redacted() {
    let v = fake();
    v.set_read_error(Some("transport error: https://rpc.example/KEY failed"));
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_200_000, 2_500_000, accepted(0xaa)))
            .unwrap();
    })
    .await;
    for (tool, args) in [
        ("my_orders", json!({"status": "all"})),
        ("get_order", json!({"order_id": id_hex(0xaa)})),
    ] {
        let r = h.call(tool, args).await;
        let t = text(&r);
        assert_eq!(r.is_error, Some(true), "{tool}: {t}");
        assert!(t.contains("<url>"), "{tool}: {t}");
        assert!(!t.contains("KEY"), "{tool}: {t}");
    }
}

#[tokio::test]
async fn health_error_is_redacted() {
    let v = fake();
    v.set_health_error("connect to https://venue.example/KEY refused");
    let h = start(v, |_| {}).await;
    let s = h.ok("account_status", json!({})).await;
    assert_eq!(s["venue"]["reachable"], false);
    assert_eq!(s["venue"]["error"], "connect to <url> refused");
}

#[tokio::test]
async fn stale_market_does_not_break_order_reads() {
    const GONE: &str = "gone::0xccc::gone::0xddd";
    let v = fake();
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[book_entry(0xaa, arborter_pb::Side::Bid, 2_500_000, 900_000)]),
    );
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_200_000, 2_500_000, accepted(0xaa)))
            .unwrap();
        let on_gone = |outcome| {
            Record::Sent(SentRecord {
                ts: 2_000,
                market_id: GONE.into(),
                side: Side::Sell,
                qty: 3_300_000,
                price: 4_400_000,
                outcome,
            })
        };
        l.append(on_gone(accepted(0xc1))).unwrap();
        l.append(on_gone(SentOutcome::Unknown)).unwrap();
        l.append(on_gone(SentOutcome::Failed {
            code: "InvalidArgument".into(),
            message: "bad".into(),
        }))
        .unwrap();
    })
    .await;

    let all = h.ok("my_orders", json!({"status": "all"})).await;
    let orders = all["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 4, "{all}");
    let a = by_id(&all, &id_hex(0xaa));
    assert_eq!(a["state"], "open");
    assert_eq!(a["remainingRaw"], "900000");

    let stale = by_id(&all, &id_hex(0xc1));
    assert_eq!(stale["state"], "unknown");
    assert_eq!(stale["qtyRaw"], "3300000");
    assert_eq!(stale["priceRaw"], "4400000");
    // No decimals to scale by: no market-unit strings, never a guess.
    assert_eq!(stale["qty"], Value::Null);
    assert_eq!(stale["price"], Value::Null);
    assert_eq!(stale["filledRaw"], Value::Null);
    let note = |o: &Value| o["note"].as_str().unwrap_or_default().to_string();
    assert!(
        note(stale).contains("market no longer in venue config"),
        "{stale}"
    );
    let unknown = &orders[2];
    assert_eq!(unknown["state"], "unknown");
    assert_eq!(unknown["orderId"], Value::Null);
    assert!(note(unknown).contains("market no longer in venue config"));
    let failed = &orders[3];
    assert_eq!(failed["state"], "rejected");
    assert_eq!(failed["qtyRaw"], "3300000");
    assert!(note(failed).contains("market no longer in venue config"));

    let one = h.ok("get_order", json!({"order_id": id_hex(0xc1)})).await;
    assert_eq!(one["state"], "unknown");
    assert_eq!(one["marketId"], GONE);
    let one = h.ok("get_order", json!({"order_id": id_hex(0xaa)})).await;
    assert_eq!(one["state"], "open");
}

#[tokio::test]
async fn keyless_base_leg_does_not_break_order_reads() {
    // The agent's EVM key was removed: the market resolves, its base leg has no trader.
    let v = fake();
    v.set_addresses(Addresses {
        evm: None,
        solana: Some("SoLagent".into()),
    });
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_200_000, 2_500_000, accepted(0xaa)))
            .unwrap();
    })
    .await;
    let one = h.ok("get_order", json!({"order_id": id_hex(0xaa)})).await;
    assert_eq!(one["state"], "unknown");
    assert_eq!(one["qtyRaw"], "1200000");
    assert!(
        one["note"].as_str().unwrap().contains("no trader key"),
        "{one}"
    );
}

// ---- write tools ----

const BID: u128 = 2_400_000;
const ASK: u128 = 2_600_000;

/// A venue whose public book has a bid at 2.4 and an ask at 2.6.
fn market_venue() -> FakeVenue {
    let v = fake();
    v.set_book(
        MARKET,
        None,
        book(&[
            book_entry(0x01, arborter_pb::Side::Bid, BID, 500_000),
            book_entry(0x02, arborter_pb::Side::Ask, ASK, 400_000),
        ]),
    );
    v
}

fn accepted_send(b: u8) -> SendResult {
    SendResult::Accepted {
        order_id: id_hex(b),
        in_book: true,
        taker_fills: vec![],
    }
}

/// Every ledger line, parsed.
fn ledger_lines(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn count_kind(dir: &std::path::Path, kind: &str) -> usize {
    ledger_lines(dir)
        .iter()
        .filter(|l| l["kind"] == kind)
        .count()
}

/// The venue saw no send, deposit or withdrawal.
fn assert_nothing_signed(v: &FakeVenue) {
    let calls = v.calls();
    assert!(
        !calls.iter().any(|c| matches!(
            c,
            Call::SendLimit { .. } | Call::Deposit { .. } | Call::Withdraw { .. }
        )),
        "{calls:?}"
    );
}

/// `r` is an error naming `needle`.
fn assert_refused(r: &CallToolResult, needle: &str) {
    let t = text(r);
    assert_eq!(r.is_error, Some(true), "not refused: {t}");
    assert!(t.contains(needle), "refusal lacks {needle:?}: {t}");
}

fn limit_buy(qty: &str, price: &str) -> Value {
    json!({"market_id": MARKET, "side": "buy", "qty": qty, "price": price})
}

fn send_call(qty: &str, price: &str, side: Side) -> Call {
    Call::SendLimit {
        config_session: SESSION,
        market_id: MARKET.into(),
        side,
        qty: qty.into(),
        price: price.into(),
        post_only: false,
        settle_base: None,
        settle_quote: None,
    }
}

/// The resume subcommand, run as the real binary against `dir`.
fn run_resume(dir: &std::path::Path) -> std::process::Output {
    let cfg = dir.join("resume.toml");
    std::fs::write(
        &cfg,
        format!(
            "stack_url = \"https://venue.example\"\nstate_dir = {:?}\n",
            dir.to_str().unwrap()
        ),
    )
    .unwrap();
    std::process::Command::new(env!("CARGO_BIN_EXE_aspens-mcp"))
        .args(["resume", "--config", cfg.to_str().unwrap()])
        .output()
        .unwrap()
}

#[tokio::test]
async fn tool_list_with_withdraw() {
    let h = start_with(fake(), |c| c.allow_withdraw = true, |_| {}).await;
    let names: BTreeSet<String> = h
        .client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names.len(), 16);
    assert!(names.contains("withdraw"));
}

#[tokio::test]
async fn no_schema_accepts_hidden_dealroom_or_market() {
    let h = start_with(fake(), |c| c.allow_withdraw = true, |_| {}).await;
    let tools = h.client.list_all_tools().await.unwrap();
    for t in &tools {
        let schema = serde_json::to_string(&t.input_schema).unwrap();
        for banned in ["hidden", "match_order_ids", "quote_budget"] {
            assert!(!schema.contains(banned), "{}: {banned} in {schema}", t.name);
        }
    }
    let schema = |name: &str| {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        serde_json::to_value(&t.input_schema).unwrap()
    };
    let limit = schema("place_limit_order");
    let required: Vec<&str> = limit["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for f in ["market_id", "side", "qty", "price"] {
        assert!(required.contains(&f), "{f} not required: {limit}");
    }
    let marketable = schema("place_marketable_order");
    assert!(
        marketable["properties"]["slippage_bps"].is_object(),
        "{marketable}"
    );
    assert!(marketable["properties"]["price"].is_null(), "{marketable}");
}

#[tokio::test]
async fn write_tools_annotations() {
    let h = start_with(fake(), |c| c.allow_withdraw = true, |_| {}).await;
    let tools = h.client.list_all_tools().await.unwrap();
    let ann = |name: &str| {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        t.annotations.clone().unwrap_or_else(|| panic!("{name}"))
    };
    for name in [
        "place_limit_order",
        "place_marketable_order",
        "cancel_order",
        "deposit",
        "withdraw",
    ] {
        let a = ann(name);
        assert_eq!(a.destructive_hint, Some(true), "{name}");
        assert_eq!(a.idempotent_hint, Some(false), "{name}");
        assert_eq!(a.read_only_hint, Some(false), "{name}");
    }
    let a = ann("halt");
    assert_eq!(a.read_only_hint, Some(false));
    assert_eq!(a.destructive_hint, Some(false));
    assert_eq!(a.idempotent_hint, Some(true));
}

#[tokio::test]
async fn limit_order_reaches_venue_and_ledger() {
    let v = market_venue();
    v.push_send(accepted_send(0xa1));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let o = h.ok("place_limit_order", limit_buy("1.2", "2.5")).await;
    assert_eq!(o["orderId"], id_hex(0xa1));
    assert_eq!(o["state"], "open");
    assert_eq!(o["qtyRaw"], "1200000");
    assert_eq!(o["priceRaw"], "2500000");
    assert_eq!(o["remainingRaw"], "1200000");
    assert_eq!(
        probe.sends(),
        vec![send_call("1.200000", "2.500000", Side::Buy)]
    );

    let lines = ledger_lines(h.dir());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["kind"], "usage");
    assert_eq!(lines[0]["qty"], "1200000");
    // ceil(1.2 * 2.5) in pair units.
    assert_eq!(lines[0]["quote"], "3000000");
    assert_eq!(lines[0]["day"], utc_day(now()));
    assert_eq!(lines[1]["kind"], "sent");
    assert_eq!(lines[1]["outcome"]["status"], "accepted");

    let one = h.ok("get_order", json!({"order_id": id_hex(0xa1)})).await;
    assert_eq!(one["side"], "buy");
    let l = h.ok("limits", json!({})).await;
    assert_eq!(l["markets"][0]["usedToday"]["qtyRaw"], "1200000");
}

#[tokio::test]
async fn sell_commits_no_quote() {
    let v = market_venue();
    v.push_send(accepted_send(0xa2));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let args = json!({"market_id": MARKET, "side": "sell", "qty": "0.75", "price": "2.45"});
    h.ok("place_limit_order", args).await;
    assert_eq!(
        probe.sends(),
        vec![send_call("0.750000", "2.450000", Side::Sell)]
    );
    let usage = &ledger_lines(h.dir())[0];
    assert_eq!(usage["side"], "sell");
    assert_eq!(usage["qty"], "750000");
    assert_eq!(usage["quote"], "0");
}

#[tokio::test]
async fn post_only_passes_through() {
    let v = market_venue();
    v.push_send(accepted_send(0xa3));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let mut args = limit_buy("1", "2.5");
    args["post_only"] = json!(true);
    h.ok("place_limit_order", args).await;
    let sends = probe.sends();
    assert!(
        matches!(
            &sends[..],
            [Call::SendLimit {
                post_only: true,
                ..
            }]
        ),
        "{sends:?}"
    );
}

#[tokio::test]
async fn settle_override_allowlisted_passes_and_reaches_venue() {
    const SETTLE: &str = "0x5e77000000000000000000000000000000005E77";
    let v = market_venue();
    v.push_send(accepted_send(0xa4));
    let probe = v.clone();
    let h = start_with(
        v,
        // The allowlist spells it in another case: 0x hex compares case-insensitively.
        |c| c.settle_address_allowlist = vec![SETTLE.to_lowercase()],
        |_| {},
    )
    .await;

    let mut bad = limit_buy("1", "2.5");
    bad["settle_quote_address"] = json!("0x0000000000000000000000000000000000000bad");
    assert_refused(&h.call("place_limit_order", bad).await, "allowlist");
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);

    let mut args = limit_buy("1", "2.5");
    args["settle_base_address"] = json!(SETTLE);
    h.ok("place_limit_order", args).await;
    let sends = probe.sends();
    assert!(
        matches!(&sends[..], [Call::SendLimit { settle_base: Some(b), settle_quote: None, .. }] if b == SETTLE),
        "{sends:?}"
    );
}

#[tokio::test]
async fn refusal_signs_nothing() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    // max_order_qty is 1.5.
    let r = h.call("place_limit_order", limit_buy("1.6", "2.5")).await;
    assert_refused(&r, "max_order_qty");
    assert!(!text(&r).contains("max_daily_qty"), "{}", text(&r));
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn refusal_lists_every_failure() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    // Over max_order_qty, and priced far above the ask.
    let r = h.call("place_limit_order", limit_buy("1.6", "9")).await;
    assert_refused(&r, "max_order_qty");
    assert_refused(&r, "max_deviation_bps");
    assert_nothing_signed(&probe);
}

#[tokio::test]
async fn untradable_market_refused() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let args =
        json!({"market_id": "base::0xAAA::base::0xbbb", "side": "buy", "qty": "1", "price": "2.5"});
    assert_refused(&h.call("place_limit_order", args).await, "not configured");
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn excess_precision_qty_refused_signs_nothing() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    // Pair decimals are 6: a 7th digit would be truncated silently by the SDK.
    let r = h
        .call("place_limit_order", limit_buy("1.0000001", "2.5"))
        .await;
    assert_refused(&r, "too many decimal places");
    let r = h
        .call("place_limit_order", limit_buy("1", "2.5000001"))
        .await;
    assert_refused(&r, "too many decimal places");
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn unknown_fields_are_rejected() {
    let v = market_venue();
    v.push_send(accepted_send(0xa5));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let mut args = limit_buy("1", "2.5");
    args["hidden"] = json!(true);
    let r = h.call("place_limit_order", args).await;
    assert_refused(&r, "hidden");
    assert_nothing_signed(&probe);
}

#[tokio::test]
async fn usage_written_before_send() {
    let v = market_venue();
    v.push_send(accepted_send(0xa6));
    let h = start(v.clone(), |_| {}).await;
    let path = h.dir().join("ledger.jsonl");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let s = seen.clone();
    v.set_on_send(std::sync::Arc::new(move || {
        *s.lock().unwrap() = Some(std::fs::read_to_string(&path).unwrap_or_default());
    }));
    h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    let at_send = seen.lock().unwrap().clone().expect("send hook did not run");
    let lines: Vec<Value> = at_send
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{at_send}");
    assert_eq!(lines[0]["kind"], "usage");
    assert_eq!(lines[0]["qty"], "1000000");
}

#[tokio::test]
async fn unknown_is_not_resent() {
    let v = market_venue();
    v.push_send(SendResult::Unknown);
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let o = h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    assert_eq!(o["state"], "unknown");
    assert_eq!(o["orderId"], Value::Null);
    assert_eq!(probe.sends().len(), 1);
    h.ok("my_orders", json!({"status": "all"})).await;
    let _ = h.call("get_order", json!({"order_id": id_hex(0x77)})).await;
    h.ok("my_orders", json!({})).await;
    assert_eq!(probe.sends().len(), 1, "{:?}", probe.calls());
}

#[tokio::test]
async fn unknown_send_is_recorded_and_counted() {
    let v = market_venue();
    v.push_send(SendResult::Unknown);
    let h = start(v, |_| {}).await;
    h.ok("place_limit_order", limit_buy("1.25", "2.5")).await;
    let lines = ledger_lines(h.dir());
    assert_eq!(lines[1]["kind"], "sent");
    assert_eq!(lines[1]["outcome"]["status"], "unknown");
    let l = h.ok("limits", json!({})).await;
    assert_eq!(l["markets"][0]["usedToday"]["qtyRaw"], "1250000");
    assert_eq!(l["markets"][0]["usedToday"]["quoteRaw"], "3125000");
    // An unresolved unknown has no id to look up by, so `my_orders` shows it.
    let open = h.ok("my_orders", json!({})).await;
    assert_eq!(open["orders"][0]["state"], "unknown");
    assert_eq!(open["orders"][0]["qtyRaw"], "1250000");
}

#[tokio::test]
async fn venue_rejection_is_recorded_and_counted() {
    let v = market_venue();
    v.push_send(SendResult::Failed {
        code: "InvalidArgument".into(),
        message: "insufficient balance".into(),
    });
    let h = start(v, |_| {}).await;
    let o = h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    assert_eq!(o["state"], "rejected");
    assert!(o["note"].as_str().unwrap().contains("insufficient balance"));
    let l = h.ok("limits", json!({})).await;
    assert_eq!(l["markets"][0]["usedToday"]["qtyRaw"], "1000000");
}

#[tokio::test]
async fn daily_cap_burned_by_cancelled_orders() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    // 5 x 1.4 = 7.0 = max_daily_qty, each cancelled straight away.
    for b in 1..=5u8 {
        probe.push_send(accepted_send(b));
        let o = h.ok("place_limit_order", limit_buy("1.4", "2.5")).await;
        assert_eq!(o["orderId"], id_hex(b));
        let c = h.ok("cancel_order", json!({"order_id": id_hex(b)})).await;
        assert_eq!(c["result"], "cancelled");
    }
    let r = h
        .call("place_limit_order", limit_buy("0.000001", "2.5"))
        .await;
    assert_refused(&r, "max_daily_qty");
    assert!(!text(&r).contains("max_daily_quote"), "{}", text(&r));
    assert_eq!(probe.sends().len(), 5);
    assert_eq!(count_kind(h.dir(), "usage"), 5);
    assert_eq!(count_kind(h.dir(), "cancelled"), 5);
}

#[tokio::test]
async fn halt_blocks_orders_deposits_withdraws_not_cancels() {
    let v = market_venue();
    let probe = v.clone();
    let h = start_with(
        v,
        |c| c.allow_withdraw = true,
        |l| {
            l.append(sent(Side::Buy, 1_000_000, 2_500_000, accepted(0xaa)))
                .unwrap();
        },
    )
    .await;
    let r = h.ok("halt", json!({"reason": "drawdown"})).await;
    assert_eq!(r["halted"], true);
    assert_eq!(r["haltReason"], "drawdown");

    let place = h.call("place_limit_order", limit_buy("1", "2.5")).await;
    assert_refused(&place, "halted: drawdown");
    let args = json!({"market_id": MARKET, "side": "buy", "qty": "1"});
    assert_refused(&h.call("place_marketable_order", args).await, "halted");
    let transfer = json!({"network": "base", "token": "BBB", "amount": "10"});
    assert_refused(&h.call("deposit", transfer.clone()).await, "halted");
    assert_refused(&h.call("withdraw", transfer).await, "halted");
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);

    let c = h
        .ok("cancel_order", json!({"order_id": id_hex(0xaa)}))
        .await;
    assert_eq!(c["result"], "cancelled");
    assert!(
        probe
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Cancel { .. }))
    );
}

#[tokio::test]
async fn halt_is_idempotent() {
    let h = start(fake(), |_| {}).await;
    h.ok("halt", json!({"reason": "first"})).await;
    let r = h.ok("halt", json!({"reason": "second"})).await;
    assert_eq!(r["halted"], true);
    assert_eq!(r["haltReason"], "first");
    assert_eq!(count_kind(h.dir(), "halt"), 1);
    let s = h.ok("account_status", json!({})).await;
    assert_eq!(s["haltReason"], "first");
}

#[tokio::test]
async fn halt_survives_restart() {
    let h = start(market_venue(), |_| {}).await;
    h.ok("halt", json!({})).await;
    let dir = h.stop().await;

    let v = market_venue();
    let probe = v.clone();
    let h = start_in(dir, v, |_| {}, |_| {}).await;
    let s = h.ok("account_status", json!({})).await;
    assert_eq!(s["halted"], true);
    assert_refused(
        &h.call("place_limit_order", limit_buy("1", "2.5")).await,
        "halted",
    );
    assert_nothing_signed(&probe);

    // resume refuses while a server holds the ledger.
    let out = run_resume(h.dir());
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("stop"), "{err}");
    let dir = h.stop().await;

    let out = run_resume(dir.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("resumed"));
    assert_eq!(count_kind(dir.path(), "resume"), 1);
    // A second resume has nothing to lift and appends nothing.
    let out = run_resume(dir.path());
    assert!(out.status.success());
    assert_eq!(count_kind(dir.path(), "resume"), 1);

    let v = market_venue();
    v.push_send(accepted_send(0xb0));
    let probe = v.clone();
    let h = start_in(dir, v, |_| {}, |_| {}).await;
    h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    assert_eq!(probe.sends().len(), 1);
}

#[tokio::test]
async fn marketable_order_prices_from_book() {
    let v = market_venue();
    v.push_send(accepted_send(0xc1));
    v.push_send(accepted_send(0xc2));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let buy = json!({"market_id": MARKET, "side": "buy", "qty": "0.5", "slippage_bps": 50});
    let o = h.ok("place_marketable_order", buy).await;
    // ask 2.6 + 50 bps.
    assert_eq!(o["priceRaw"], "2613000");
    // Default slippage is 50 bps: bid 2.4 - 50 bps.
    let sell = json!({"market_id": MARKET, "side": "sell", "qty": "0.5"});
    h.ok("place_marketable_order", sell).await;
    assert_eq!(
        probe.sends(),
        vec![
            send_call("0.500000", "2.613000", Side::Buy),
            send_call("0.500000", "2.388000", Side::Sell),
        ]
    );
    let books = probe
        .calls()
        .iter()
        .filter(|c| {
            matches!(
                c,
                Call::Book {
                    filter_by_trader: None,
                    ..
                }
            )
        })
        .count();
    assert_eq!(books, 2, "one book read per order");
}

#[tokio::test]
async fn marketable_slippage_beyond_deviation_refused() {
    let v = market_venue();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let args = json!({"market_id": MARKET, "side": "buy", "qty": "0.5", "slippage_bps": 150});
    assert_refused(
        &h.call("place_marketable_order", args).await,
        "max_deviation_bps",
    );
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn marketable_with_empty_side_refused() {
    let v = fake();
    v.set_book(
        MARKET,
        None,
        book(&[book_entry(0x01, arborter_pb::Side::Bid, BID, 500_000)]),
    );
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let args = json!({"market_id": MARKET, "side": "buy", "qty": "0.5"});
    assert_refused(
        &h.call("place_marketable_order", args).await,
        "no resting asks",
    );
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn cancel_unknown_id_requires_side() {
    let v = fake();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let r = h
        .call(
            "cancel_order",
            json!({"order_id": id_hex(0x99), "market_id": MARKET}),
        )
        .await;
    assert_refused(&r, "side");
    let r = h
        .call(
            "cancel_order",
            json!({"order_id": id_hex(0x99), "side": "sell"}),
        )
        .await;
    assert_refused(&r, "market_id");
    assert!(
        !probe
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Cancel { .. }))
    );

    // A market the venue does not list is refused too.
    let r = h
        .call(
            "cancel_order",
            json!({"order_id": id_hex(0x99), "market_id": "nope", "side": "sell"}),
        )
        .await;
    assert_refused(&r, "nope");
    assert!(
        !probe
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Cancel { .. }))
    );

    let upper = format!("0X{}", "99".repeat(32)).to_uppercase();
    let c = h
        .ok(
            "cancel_order",
            json!({"order_id": upper, "market_id": MARKET, "side": "sell"}),
        )
        .await;
    assert_eq!(c["orderId"], id_hex(0x99));
    let cancels: Vec<Call> = probe
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::Cancel { .. }))
        .collect();
    assert_eq!(
        cancels,
        vec![Call::Cancel {
            market_id: MARKET.into(),
            side: Side::Sell,
            order_id: id(0x99),
        }]
    );
}

#[tokio::test]
async fn cancel_ledger_order_uses_ledger_market_and_side() {
    let v = fake();
    let probe = v.clone();
    let h = start(v, |l| {
        l.append(sent(Side::Sell, 1_000_000, 2_500_000, accepted(0xab)))
            .unwrap();
    })
    .await;
    // The arguments disagree with the ledger and are ignored.
    let c = h
        .ok(
            "cancel_order",
            json!({"order_id": id_hex(0xab), "market_id": "nope", "side": "buy"}),
        )
        .await;
    assert_eq!(c["result"], "cancelled");
    assert_eq!(c["side"], "sell");
    let cancels: Vec<Call> = probe
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::Cancel { .. }))
        .collect();
    assert_eq!(
        cancels,
        vec![Call::Cancel {
            market_id: MARKET.into(),
            side: Side::Sell,
            order_id: id(0xab),
        }]
    );
    let lines = ledger_lines(h.dir());
    let last = lines.last().unwrap();
    assert_eq!(last["kind"], "cancelled");
    assert_eq!(last["order_id"], id_hex(0xab));
    let o = h.ok("get_order", json!({"order_id": id_hex(0xab)})).await;
    assert_eq!(o["state"], "cancelled");
}

#[tokio::test]
async fn cancel_already_gone_appends_nothing() {
    let v = fake();
    v.push_cancel(Ok(CancelResult::AlreadyGone));
    let h = start(v, |l| {
        l.append(sent(Side::Buy, 1_000_000, 2_500_000, accepted(0xac)))
            .unwrap();
    })
    .await;
    let before = ledger_lines(h.dir()).len();
    let c = h
        .ok("cancel_order", json!({"order_id": id_hex(0xac)}))
        .await;
    assert_eq!(c["result"], "already_gone");
    assert_eq!(ledger_lines(h.dir()).len(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_orders_cannot_double_spend_daily_cap() {
    let today = utc_day(now());
    let v = market_venue();
    v.push_send(accepted_send(0xd1));
    v.push_send(accepted_send(0xd2));
    // A slow send widens any window between the check and the usage record.
    v.set_on_send(std::sync::Arc::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }));
    let probe = v.clone();
    let h = start(v, |l| {
        // 5.5 used: each 1.0 order alone fits under 7, the two together do not.
        l.append(Record::Usage {
            ts: 1,
            day: today.clone(),
            market_id: MARKET.into(),
            side: Side::Sell,
            qty: 5_500_000,
            quote: 0,
            price: 2_400_000,
        })
        .unwrap();
    })
    .await;
    let (a, b) = tokio::join!(
        h.call("place_limit_order", limit_buy("1", "2.5")),
        h.call("place_limit_order", limit_buy("1", "2.5")),
    );
    let passed = [&a, &b].iter().filter(|r| r.is_error != Some(true)).count();
    assert_eq!(passed, 1, "{} / {}", text(&a), text(&b));
    let refused = if a.is_error == Some(true) { &a } else { &b };
    assert_refused(refused, "max_daily_qty");
    assert_eq!(probe.sends().len(), 1);
    assert_eq!(count_kind(h.dir(), "usage"), 2);
}

#[tokio::test]
async fn deposit_reaches_venue_in_token_units() {
    let v = fake();
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    let r = h
        .ok(
            "deposit",
            json!({"network": "base", "token": "BBB", "amount": "12.5"}),
        )
        .await;
    assert_eq!(r["amountRaw"], "1250000000");
    assert_eq!(
        probe.calls(),
        vec![Call::Deposit {
            config_session: SESSION,
            network: "base".into(),
            token_symbol: "BBB".into(),
            amount: 1_250_000_000,
        }]
    );
    // Deposits are not usage.
    assert!(ledger_lines(h.dir()).is_empty());
}

#[tokio::test]
async fn deposit_over_max_per_call_refused() {
    let v = fake();
    let probe = v.clone();
    // Keyed by the lowercase address; the venue spells it mixed case.
    let key = format!("base:{}", BBB_ADDR.to_lowercase());
    let h = start_with(
        v,
        |c| c.deposits.max_per_call = BTreeMap::from([(key, "100".to_string())]),
        |_| {},
    )
    .await;
    let over = json!({"network": "base", "token": "BBB", "amount": "100.00000001"});
    let r = h.call("deposit", over).await;
    assert_refused(&r, "max_per_call");
    // Human units, the raw value in parentheses.
    assert_refused(
        &r,
        "amount 100.00000001 (10000000001) > limit 100.00000000 (10000000000)",
    );
    // BBB has 8 decimals: a 9th is refused, never truncated.
    let precise = json!({"network": "base", "token": "BBB", "amount": "1.000000001"});
    assert_refused(&h.call("deposit", precise).await, "too many decimal places");
    assert_nothing_signed(&probe);

    h.ok(
        "deposit",
        json!({"network": "base", "token": "BBB", "amount": "100"}),
    )
    .await;
    // The cap is per token: AAA has none.
    h.ok(
        "deposit",
        json!({"network": "base", "token": "AAA", "amount": "1000"}),
    )
    .await;
    let deposits: Vec<u128> = probe
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Deposit { amount, .. } => Some(amount),
            _ => None,
        })
        .collect();
    assert_eq!(deposits, vec![10_000_000_000, 1_000 * 10u128.pow(18)]);
}

#[tokio::test]
async fn deposits_disabled_refused() {
    let v = fake();
    let probe = v.clone();
    let h = start_with(v, |c| c.deposits.enabled = false, |_| {}).await;
    let args = json!({"network": "base", "token": "BBB", "amount": "1"});
    assert_refused(&h.call("deposit", args).await, "deposits are disabled");
    assert_nothing_signed(&probe);
}

#[tokio::test]
async fn withdraw_reaches_venue_in_token_units() {
    let v = fake();
    let probe = v.clone();
    let h = start_with(v, |c| c.allow_withdraw = true, |_| {}).await;
    h.ok(
        "withdraw",
        json!({"network": "base", "token": "AAA", "amount": "0.5"}),
    )
    .await;
    assert_eq!(
        probe.calls(),
        vec![Call::Withdraw {
            config_session: SESSION,
            network: "base".into(),
            token_symbol: "AAA".into(),
            amount: 5 * 10u128.pow(17),
        }]
    );
}

fn pinned(contract: &str, mrtd: Option<&str>) -> impl FnOnce(&mut Config) {
    let pin = VenuePin {
        expected_contracts: BTreeMap::from([("base".to_string(), contract.to_string())]),
        attestation_mrtd: mrtd.map(str::to_string),
    };
    move |c: &mut Config| {
        c.allow_withdraw = true;
        c.venue = Some(pin);
    }
}

#[tokio::test]
async fn venue_pin_contract_mismatch_refuses_writes() {
    let v = market_venue();
    v.push_send(accepted_send(0xe1));
    let probe = v.clone();
    let wrong = "0x00000000000000000000000000000000000BAD01";
    let h = start_with(v, pinned(wrong, None), |_| {}).await;
    assert_refused(
        &h.call("place_limit_order", limit_buy("1", "2.5")).await,
        "venue pin",
    );
    let transfer = json!({"network": "base", "token": "BBB", "amount": "1"});
    assert_refused(&h.call("deposit", transfer.clone()).await, "venue pin");
    assert_refused(&h.call("withdraw", transfer).await, "venue pin");
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

#[tokio::test]
async fn venue_pin_contract_match_passes() {
    let v = market_venue();
    v.push_send(accepted_send(0xe2));
    let probe = v.clone();
    // The pin spells the contract lowercase; the venue mixed case.
    let h = start_with(v, pinned(&TRADE_CONTRACT.to_lowercase(), None), |_| {}).await;
    h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    h.ok(
        "deposit",
        json!({"network": "base", "token": "BBB", "amount": "1"}),
    )
    .await;
    assert_eq!(probe.sends().len(), 1);
    assert!(!probe.calls().contains(&Call::Attestation));
}

#[tokio::test]
async fn venue_pin_unverified_attestation_fails_closed() {
    let v = market_venue();
    v.push_send(accepted_send(0xe3));
    let probe = v.clone();
    let h = start_with(v, pinned(TRADE_CONTRACT, Some("ab12")), |_| {}).await;
    let r = h.call("place_limit_order", limit_buy("1", "2.5")).await;
    assert_refused(&r, "attestation");
    assert_refused(&r, "fail");
    let transfer = json!({"network": "base", "token": "BBB", "amount": "1"});
    assert_refused(&h.call("deposit", transfer).await, "attestation");
    assert_nothing_signed(&probe);
    // A failure is never cached: each write asks again.
    let asked = probe
        .calls()
        .iter()
        .filter(|c| **c == Call::Attestation)
        .count();
    assert_eq!(asked, 2);
}

#[tokio::test]
async fn venue_pin_verified_attestation_is_cached() {
    let v = market_venue();
    v.set_attestation(AttestationView {
        report: Value::Null,
        verified: Some(true),
        note: None,
    });
    v.push_send(accepted_send(0xe4));
    v.push_send(accepted_send(0xe5));
    let probe = v.clone();
    let h = start_with(v, pinned(TRADE_CONTRACT, Some("ab12")), |_| {}).await;
    h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    h.ok("place_limit_order", limit_buy("1", "2.5")).await;
    assert_eq!(probe.sends().len(), 2);
    let asked = probe
        .calls()
        .iter()
        .filter(|c| **c == Call::Attestation)
        .count();
    assert_eq!(asked, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_wait_for_an_inflight_send() {
    let v = market_venue();
    // The order rests in the agent's book as soon as the venue has it.
    v.set_book(
        MARKET,
        Some(AGENT),
        book(&[book_entry(
            0xf1,
            arborter_pb::Side::Bid,
            2_500_000,
            1_000_000,
        )]),
    );
    v.push_send(accepted_send(0xf1));
    v.set_on_send(std::sync::Arc::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }));
    let h = start(v, |_| {}).await;
    let read = async {
        // Issued while the send is in flight.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        h.ok("my_orders", json!({"status": "all"})).await
    };
    let (placed, orders) = tokio::join!(h.call("place_limit_order", limit_buy("1", "2.5")), read);
    assert_ne!(placed.is_error, Some(true), "{}", text(&placed));
    // The read waited for the outcome record: it sees the order, not nothing.
    assert_eq!(by_id(&orders, &id_hex(0xf1))["state"], "open", "{orders}");
}

#[tokio::test]
async fn pair_decimals_change_refuses_signs_nothing() {
    let v = market_venue();
    v.push_send(accepted_send(0xf2));
    let probe = v.clone();
    let h = start(v, |_| {}).await;
    // The venue restarts with the market in 8 pair decimals; limits were
    // resolved at 6. A marketable sell would price at 1/100 of the bid.
    let mut changed = venue_config();
    changed.config.as_mut().unwrap().markets[0].pair_decimals = 8;
    probe.set_config(changed);
    let sell = json!({"market_id": MARKET, "side": "sell", "qty": "0.5"});
    let r = h.call("place_marketable_order", sell).await;
    assert_refused(
        &r,
        "venue changed this market's pair decimals (6 \u{2192} 8); restart aspens-mcp to re-resolve limits",
    );
    assert_refused(
        &h.call("place_limit_order", limit_buy("1", "2.5")).await,
        "pair decimals",
    );
    assert_nothing_signed(&probe);
    assert_eq!(count_kind(h.dir(), "usage"), 0);
}

/// A venue config in `session` whose trade contract on base is `contract`.
fn config_in(session: u64, contract: &str) -> GetConfigResponse {
    let mut c = venue_config();
    let inner = c.config.as_mut().unwrap();
    inner.session = session;
    inner.chains[0].trade_contract = Some(TradeContract {
        address: contract.into(),
    });
    c
}

#[tokio::test]
async fn write_uses_the_checked_config_snapshot() {
    const OTHER: &str = "0x00000000000000000000000000000000000BAD02";
    let transfer = json!({"network": "base", "token": "BBB", "amount": "1"});
    for (tool, args) in [
        ("place_limit_order", limit_buy("1", "2.5")),
        ("deposit", transfer.clone()),
        ("withdraw", transfer.clone()),
    ] {
        let v = market_venue();
        v.push_send(accepted_send(0xf3));
        v.set_attestation(AttestationView {
            report: Value::Null,
            verified: Some(true),
            note: None,
        });
        // The venue restarts onto another contract after the pin is checked.
        let flip = v.clone();
        v.set_on_attestation(std::sync::Arc::new(move || {
            flip.set_config(config_in(SESSION + 1, OTHER));
        }));
        let probe = v.clone();
        let h = start_with(v, pinned(TRADE_CONTRACT, Some("ab12")), |_| {}).await;
        h.ok(tool, args).await;
        let writes: Vec<u64> = probe
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::SendLimit { config_session, .. }
                | Call::Deposit { config_session, .. }
                | Call::Withdraw { config_session, .. } => Some(config_session),
                _ => None,
            })
            .collect();
        assert_eq!(
            writes,
            vec![SESSION],
            "{tool}: signed against another config"
        );

        // The next write snapshots the new config, whose contract the pin refuses.
        assert_refused(&h.call("deposit", transfer.clone()).await, "venue pin");
        assert_eq!(
            probe
                .calls()
                .iter()
                .filter(|c| matches!(c, Call::Deposit { .. }))
                .count(),
            usize::from(tool == "deposit"),
            "{tool}"
        );
    }
}

#[tokio::test]
async fn halted_transfer_says_halted_when_venue_is_down() {
    let v = fake();
    v.set_config_error(Some("connect to https://venue.example/KEY refused"));
    let probe = v.clone();
    let h = start_with(v, |c| c.allow_withdraw = true, |_| {}).await;
    let transfer = json!({"network": "base", "token": "BBB", "amount": "1"});
    // Not halted: the venue error comes through (redacted).
    let r = h.call("deposit", transfer.clone()).await;
    assert_refused(&r, "<url>");
    assert!(!text(&r).contains("KEY"));
    h.ok("halt", json!({"reason": "ops"})).await;
    for tool in ["deposit", "withdraw"] {
        assert_refused(&h.call(tool, transfer.clone()).await, "halted: ops");
    }
    assert_nothing_signed(&probe);
}

#[tokio::test]
async fn unknown_guidance_agrees_everywhere() {
    let h = start(fake(), |_| {}).await;
    let guidance = "check it later with `my_orders` (which works without an id), or with \
                    `get_order` once an id is known";
    let ins = h.client.peer_info().unwrap().instructions.clone().unwrap();
    assert!(ins.contains(guidance), "{ins}");
    let tools = h.client.list_all_tools().await.unwrap();
    let desc = |name: &str| {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        t.description.as_deref().unwrap_or_default().to_string()
    };
    for name in ["place_limit_order", "place_marketable_order"] {
        assert!(desc(name).contains(guidance), "{name}: {}", desc(name));
    }
    let cancel = desc("cancel_order");
    assert!(
        cancel.contains("whose id this server's ledger does not hold"),
        "{cancel}"
    );
    assert!(cancel.contains("pass market_id and side"), "{cancel}");
}

/// The client goes away (stdin closes) while a send is in flight: the server's
/// session ends, the send task still runs, and waiting on `in_flight` (as
/// `serve` does before exiting) sees its outcome recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_client_mid_send_still_records_the_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let v = market_venue();
    v.push_send(accepted_send(0xc8));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = std::sync::Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    v.set_on_send(std::sync::Arc::new(move || {
        if let Some(tx) = entered_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        // The hook is synchronous: hand this worker's other tasks (the
        // session loop among them) to the rest of the runtime while it blocks.
        tokio::task::block_in_place(|| {
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(30));
        });
    }));

    let server = Server::new(
        v,
        config(dir.path()),
        limits(),
        Ledger::open(dir.path()).unwrap(),
    );
    let in_flight = server.in_flight();
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let served = tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();
    let peer = client.peer().clone();
    let call = tokio::spawn(async move {
        let mut p = CallToolRequestParams::new("place_limit_order");
        if let Value::Object(m) = limit_buy("1", "2.5") {
            p = p.with_arguments(m);
        }
        let _ = peer.call_tool(p).await;
    });
    entered_rx.await.unwrap();
    // Closing the client closes the transport, as a closed stdin does.
    client.cancel().await.unwrap();
    call.abort();
    // rmcp gives in-flight handlers 5 s to answer after the transport
    // closes, then ends the session with them still running.
    tokio::time::timeout(std::time::Duration::from_secs(20), served)
        .await
        .expect("the session did not end when the client went away")
        .unwrap();
    assert_eq!(in_flight.count(), 1);
    assert_eq!(count_kind(dir.path(), "usage"), 1);
    assert_eq!(count_kind(dir.path(), "sent"), 0);

    release_tx.send(()).unwrap();
    assert!(in_flight.wait_idle(std::time::Duration::from_secs(5)).await);
    assert_eq!(in_flight.count(), 0);
    let lines = ledger_lines(dir.path());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["kind"], "sent");
    // The usage has its outcome right after it, so replay adds nothing.
    assert_eq!(lines[1]["outcome"]["status"], "accepted");
}

/// A usage record left without its outcome (the process died mid-send) shows
/// in `my_orders` as an unknown order after a restart, and still counts.
#[tokio::test]
async fn orphan_usage_shows_as_unknown_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let today = utc_day(now());
    {
        let mut l = Ledger::open(dir.path()).unwrap();
        l.append(Record::Usage {
            ts: now(),
            day: today.clone(),
            market_id: MARKET.into(),
            side: Side::Buy,
            qty: 1_250_000,
            quote: 3_125_000,
            price: 2_500_000,
        })
        .unwrap();
    }
    let h = start_in(dir, fake(), |_| {}, |_| {}).await;
    let open = h.ok("my_orders", json!({})).await;
    let orders = open["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 1, "{open}");
    assert_eq!(orders[0]["state"], "unknown");
    assert_eq!(orders[0]["orderId"], Value::Null);
    assert_eq!(orders[0]["qtyRaw"], "1250000");
    assert_eq!(orders[0]["priceRaw"], "2500000");
    let l = h.ok("limits", json!({})).await;
    assert_eq!(l["markets"][0]["usedToday"]["qtyRaw"], "1250000");
    assert_eq!(l["markets"][0]["usedToday"]["quoteRaw"], "3125000");
    // The file still holds the one usage line.
    assert_eq!(ledger_lines(h.dir()).len(), 1);
}
