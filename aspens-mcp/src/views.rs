//! JSON shaping for tool results. Pure: no venue, no ledger, no I/O.
//!
//! Every amount is written as `key` (a decimal string) plus `keyRaw` (the
//! integer as a string). Any SDK text embedded in a view passes through
//! [`redact`].

use aspens::commands::config::config_pb::Configuration;
use aspens::commands::trading::balance::{BalanceValue, Balances};
use aspens::commands::trading::book_snapshot::{BookLevelOrder, BookState};
use aspens::decimals::format_decimal_amount;
use serde_json::{Map, Value, json};

use crate::config::MarketLimits;
use crate::ledger::{SentRecord, Side};
use crate::orders::{OrderFill, OrderState, OrderView};
use crate::redact::redact;

/// `raw` in units of `10^-decimals`, as a decimal string; `null` when the
/// decimals are too large to scale a `u128` by.
pub fn decimal(raw: u128, decimals: u32) -> Value {
    if decimals <= 38 {
        Value::String(format_decimal_amount(raw, decimals))
    } else {
        Value::Null
    }
}

/// Insert `key` (decimal string) and `keyRaw` (integer string). With no
/// `decimals`, `key` is `null` and only the integer is reported; with no
/// `raw`, both are `null`.
pub fn put_amount(
    obj: &mut Map<String, Value>,
    key: &str,
    raw: Option<u128>,
    decimals: impl Into<Option<u32>>,
) {
    let decimals = decimals.into();
    let (v, r) = match raw {
        Some(raw) => (
            decimals.map_or(Value::Null, |d| decimal(raw, d)),
            Value::String(raw.to_string()),
        ),
        None => (Value::Null, Value::Null),
    };
    obj.insert(key.to_string(), v);
    obj.insert(format!("{key}Raw"), r);
}

/// A balance reading as `key` / `keyRaw` / `keyError`. Error text is redacted.
fn put_balance(
    obj: &mut Map<String, Value>,
    key: &str,
    value: &BalanceValue,
    decimals: Option<u32>,
) {
    let (raw, err) = match value {
        BalanceValue::Raw(n) => (Some(*n), None),
        BalanceValue::NoWallet => (None, Some("no wallet".to_string())),
        BalanceValue::Error(e) => (None, Some(redact(e))),
    };
    put_amount(obj, key, raw, decimals);
    obj.insert(
        format!("{key}Error"),
        err.map_or(Value::Null, Value::String),
    );
}

pub fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// EVM-style `0x` addresses lowercase; anything else (base58) as it is.
fn canonical(a: &str) -> String {
    if a.starts_with("0x") || a.starts_with("0X") {
        a.to_lowercase()
    } else {
        a.to_string()
    }
}

fn wire_decimals(d: i32) -> Value {
    u32::try_from(d).map_or(Value::Null, Value::from)
}

/// `venue_config`: markets with their legs, and chains with their tokens.
pub fn venue_config(c: &Configuration, tradable: impl Fn(&str) -> bool) -> Value {
    let markets: Vec<Value> = c
        .markets
        .iter()
        .map(|m| {
            json!({
                "marketId": m.market_id,
                "name": m.name,
                "pairDecimals": wire_decimals(m.pair_decimals),
                "tradable": tradable(&m.market_id),
                "base": {
                    "network": m.base_chain_network,
                    "token": m.base_chain_token_symbol,
                    "decimals": wire_decimals(m.base_chain_token_decimals),
                },
                "quote": {
                    "network": m.quote_chain_network,
                    "token": m.quote_chain_token_symbol,
                    "decimals": wire_decimals(m.quote_chain_token_decimals),
                },
            })
        })
        .collect();
    let chains: Vec<Value> = c
        .chains
        .iter()
        .map(|ch| {
            let mut tokens: Vec<_> = ch.tokens.values().collect();
            tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol));
            let tokens: Vec<Value> = tokens
                .into_iter()
                .map(|t| {
                    json!({
                        "symbol": t.symbol,
                        "name": t.name,
                        "address": canonical(&t.address),
                        "decimals": t.decimals,
                    })
                })
                .collect();
            json!({
                "network": ch.network,
                "architecture": ch.architecture,
                "chainId": ch.chain_id,
                "tradeContract": ch.trade_contract.as_ref().map(|t| canonical(&t.address)),
                "tokens": tokens,
            })
        })
        .collect();
    json!({ "markets": markets, "chains": chains })
}

/// `top_of_book`: best bid and ask in pair decimals `d`.
pub fn top_of_book(market_id: &str, book: &BookState, d: u32) -> Value {
    let mut m = object(json!({ "marketId": market_id }));
    put_amount(&mut m, "bestBid", book.best_bid(), d);
    put_amount(&mut m, "bestAsk", book.best_ask(), d);
    Value::Object(m)
}

/// Aggregate one side of the book by price, best first, capped at `levels`.
fn levels(orders: &[&BookLevelOrder], levels: usize, d: u32) -> Vec<Value> {
    let mut out: Vec<(u128, u128, u32)> = Vec::new();
    for o in orders {
        match out.last_mut() {
            Some((price, qty, n)) if *price == o.price => {
                *qty = qty.saturating_add(o.quantity);
                *n += 1;
            }
            _ => {
                if out.len() == levels {
                    break;
                }
                out.push((o.price, o.quantity, 1));
            }
        }
    }
    out.into_iter()
        .map(|(price, qty, n)| {
            let mut m = Map::new();
            put_amount(&mut m, "price", Some(price), d);
            put_amount(&mut m, "qty", Some(qty), d);
            m.insert("orders".into(), n.into());
            Value::Object(m)
        })
        .collect()
}

/// `orderbook`: depth per side, aggregated by price, capped at `max_levels`.
pub fn orderbook(market_id: &str, book: &BookState, max_levels: usize, d: u32) -> Value {
    json!({
        "marketId": market_id,
        "bids": levels(&book.bids(), max_levels, d),
        "asks": levels(&book.asks(), max_levels, d),
    })
}

/// `balances`: token units of each chain, with every error text redacted.
pub fn balances(b: &Balances) -> Value {
    let tokens: Vec<Value> = b
        .tokens
        .iter()
        .map(|t| {
            let chains: Vec<Value> = t
                .chains
                .iter()
                .map(|c| {
                    let mut m = object(json!({ "network": c.network, "decimals": c.decimals }));
                    put_balance(&mut m, "wallet", &c.wallet, Some(c.decimals));
                    put_balance(&mut m, "available", &c.available, Some(c.decimals));
                    put_balance(&mut m, "locked", &c.locked, Some(c.decimals));
                    Value::Object(m)
                })
                .collect();
            json!({ "symbol": t.symbol, "chains": chains })
        })
        .collect();
    let native: Vec<Value> = b
        .native
        .iter()
        .map(|n| {
            let mut m = object(json!({ "network": n.network, "decimals": n.decimals }));
            // An unknown unit reports the integer only.
            put_balance(&mut m, "balance", &n.balance, n.decimals);
            Value::Object(m)
        })
        .collect();
    json!({ "tokens": tokens, "native": native })
}

fn fills(fills: &[OrderFill], d: u32) -> Vec<Value> {
    fills
        .iter()
        .map(|f| {
            let mut m = object(json!({ "fillId": f.fill_id, "role": f.role, "ts": f.ts }));
            put_amount(&mut m, "qty", Some(f.qty), d);
            put_amount(&mut m, "price", Some(f.price), d);
            Value::Object(m)
        })
        .collect()
}

/// One derived order, in pair decimals `d`.
pub fn order(sent: &SentRecord, v: &OrderView, d: u32) -> Value {
    let mut m = object(json!({
        "orderId": v.order_id,
        "marketId": v.market_id,
        "side": v.side,
        "sentAt": sent.ts,
    }));
    put_amount(&mut m, "qty", Some(v.qty), d);
    put_amount(&mut m, "price", Some(v.price), d);
    put_amount(&mut m, "filled", v.filled, d);
    put_amount(&mut m, "remaining", v.remaining, d);
    m.insert("fills".into(), fills(&v.fills, d).into());
    m.insert("state".into(), json!(v.state));
    m.insert("hidden".into(), json!(v.hidden));
    m.insert(
        "note".into(),
        v.note.as_deref().map_or(Value::Null, |n| redact(n).into()),
    );
    Value::Object(m)
}

/// An order on a market the venue can no longer describe: what was sent, as
/// raw integers only, and no fill or remaining figures.
pub fn stale_order(
    sent: &SentRecord,
    order_id: Option<&str>,
    state: OrderState,
    note: &str,
) -> Value {
    let mut m = object(json!({
        "orderId": order_id,
        "marketId": sent.market_id,
        "side": sent.side,
        "sentAt": sent.ts,
    }));
    put_amount(&mut m, "qty", Some(sent.qty), None);
    put_amount(&mut m, "price", Some(sent.price), None);
    put_amount(&mut m, "filled", None, None);
    put_amount(&mut m, "remaining", None, None);
    m.insert("fills".into(), json!([]));
    m.insert("state".into(), json!(state));
    m.insert("hidden".into(), Value::Null);
    m.insert("note".into(), redact(note).into());
    Value::Object(m)
}

/// One of the agent's fills, on its order `order_id`.
pub fn fill(market_id: &str, order_id: &str, side: Side, f: &OrderFill, d: u32) -> Value {
    let mut m = object(json!({
        "marketId": market_id,
        "orderId": order_id,
        "fillId": f.fill_id,
        "side": side,
        "role": f.role,
        "ts": f.ts,
    }));
    put_amount(&mut m, "qty", Some(f.qty), d);
    put_amount(&mut m, "price", Some(f.price), d);
    Value::Object(m)
}

/// One configured market's limits and its usage `(qty, quote)` today.
pub fn market_limits(id: &str, l: &MarketLimits, used: (u128, u128)) -> Value {
    let d = l.pair_decimals;
    let (qty, quote) = used;
    let mut m = object(json!({
        "marketId": id,
        "pairDecimals": d,
        "maxDeviationBps": l.max_deviation_bps,
    }));
    put_amount(&mut m, "maxOrderQty", Some(l.max_order_qty), d);
    put_amount(&mut m, "maxDailyQty", Some(l.max_daily_qty), d);
    put_amount(&mut m, "maxOrderQuote", Some(l.max_order_quote), d);
    put_amount(&mut m, "maxDailyQuote", Some(l.max_daily_quote), d);
    let mut used = Map::new();
    put_amount(&mut used, "qty", Some(qty), d);
    put_amount(&mut used, "quote", Some(quote), d);
    let mut left = Map::new();
    put_amount(
        &mut left,
        "qty",
        Some(l.max_daily_qty.saturating_sub(qty)),
        d,
    );
    put_amount(
        &mut left,
        "quote",
        Some(l.max_daily_quote.saturating_sub(quote)),
        d,
    );
    m.insert("usedToday".into(), Value::Object(used));
    m.insert("remainingToday".into(), Value::Object(left));
    Value::Object(m)
}
