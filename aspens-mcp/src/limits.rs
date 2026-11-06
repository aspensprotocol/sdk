//! The limits engine: decides whether an order may be signed. Pure, no I/O.
//!
//! Every qty, price and quote is a `u128` in the market's pair-decimal units.
//! All arithmetic is checked; an overflow is a [`Refusal::Overflow`], never a
//! wrap and never a panic.

use std::collections::BTreeMap;
use std::fmt;

use crate::config::MarketLimits;
use crate::ledger::Side;

const BPS_DENOM: u128 = 10_000;

/// An order the agent wants to place.
#[derive(Debug, Clone, Copy)]
pub struct OrderRequest<'a> {
    pub market_id: &'a str,
    pub side: Side,
    pub qty: u128,
    pub price: u128,
    pub settle_base: Option<&'a str>,
    pub settle_quote: Option<&'a str>,
}

/// Best bid and ask of the venue book at decision time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookTop {
    pub best_bid: Option<u128>,
    pub best_ask: Option<u128>,
}

/// What an accepted order commits against the daily budgets.
/// `quote` is 0 for sells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub qty: u128,
    pub quote: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Halted(Option<String>),
    ZeroQty,
    ZeroPrice,
    MarketNotTradable(String),
    OrderQty {
        qty: u128,
        max: u128,
    },
    DailyQty {
        used: u128,
        qty: u128,
        max: u128,
    },
    OrderQuote {
        quote: u128,
        max: u128,
    },
    DailyQuote {
        used: u128,
        quote: u128,
        max: u128,
    },
    PriceDeviation {
        price: u128,
        reference: u128,
        max_bps: u32,
    },
    NoReference {
        side: Side,
    },
    SettleAddressNotAllowed(String),
    Overflow,
    VenuePin(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Halted(Some(reason)) => write!(f, "trading is halted: {reason}"),
            Refusal::Halted(None) => write!(f, "trading is halted (no reason recorded)"),
            Refusal::ZeroQty => write!(f, "qty must be greater than zero"),
            Refusal::ZeroPrice => write!(f, "price must be greater than zero"),
            Refusal::MarketNotTradable(id) => write!(
                f,
                "market \"{id}\" is not configured for trading (market ids match exactly)"
            ),
            Refusal::OrderQty { qty, max } => {
                write!(f, "max_order_qty exceeded: order qty {qty} > limit {max}")
            }
            Refusal::DailyQty { used, qty, max } => write!(
                f,
                "max_daily_qty exceeded: used today {used} + order qty {qty} > limit {max}"
            ),
            Refusal::OrderQuote { quote, max } => write!(
                f,
                "max_order_quote exceeded: order quote {quote} > limit {max}"
            ),
            Refusal::DailyQuote { used, quote, max } => write!(
                f,
                "max_daily_quote exceeded: used today {used} + order quote {quote} > limit {max}"
            ),
            Refusal::PriceDeviation {
                price,
                reference,
                max_bps,
            } => write!(
                f,
                "max_deviation_bps exceeded: price {price} deviates from book reference {reference} by more than {max_bps} bps"
            ),
            Refusal::NoReference { side } => write!(
                f,
                "no price reference for {side:?}: the opposite side of the book is empty and the price is not inside the visible side (max_deviation_bps cannot be checked)"
            ),
            Refusal::SettleAddressNotAllowed(addr) => write!(
                f,
                "settle address {addr} is not in the settle_addresses allowlist"
            ),
            Refusal::Overflow => write!(f, "arithmetic overflow computing order limits"),
            Refusal::VenuePin(msg) => write!(f, "venue pin mismatch: {msg}"),
        }
    }
}

/// `ceil(qty * price / 10^pair_decimals)`; `None` on any overflow.
pub fn quote_of(qty: u128, price: u128, pair_decimals: u32) -> Option<u128> {
    let scale = 10u128.checked_pow(pair_decimals)?;
    let product = qty.checked_mul(price)?;
    let q = product / scale;
    if product % scale == 0 {
        Some(q)
    } else {
        q.checked_add(1)
    }
}

/// Reject prices too far from the opposite side of the book.
pub fn price_guard(side: Side, price: u128, top: &BookTop, max_bps: u32) -> Result<(), Refusal> {
    let overflow = || Refusal::Overflow;
    match side {
        Side::Buy => match top.best_ask {
            Some(ask) => {
                let lhs = price.checked_mul(BPS_DENOM).ok_or_else(overflow)?;
                let factor = BPS_DENOM + u128::from(max_bps);
                let rhs = ask.checked_mul(factor).ok_or_else(overflow)?;
                if lhs <= rhs {
                    Ok(())
                } else {
                    Err(Refusal::PriceDeviation {
                        price,
                        reference: ask,
                        max_bps,
                    })
                }
            }
            None => match top.best_bid {
                Some(bid) if price > bid => Err(Refusal::NoReference { side }),
                _ => Ok(()),
            },
        },
        Side::Sell => match top.best_bid {
            Some(bid) => {
                let lhs = price.checked_mul(BPS_DENOM).ok_or_else(overflow)?;
                let factor = BPS_DENOM - u128::from(max_bps).min(BPS_DENOM);
                let rhs = bid.checked_mul(factor).ok_or_else(overflow)?;
                if lhs >= rhs {
                    Ok(())
                } else {
                    Err(Refusal::PriceDeviation {
                        price,
                        reference: bid,
                        max_bps,
                    })
                }
            }
            None => match top.best_ask {
                Some(ask) if price < ask => Err(Refusal::NoReference { side }),
                _ => Ok(()),
            },
        },
    }
}

fn is_hex_addr(s: &str) -> bool {
    s.starts_with("0x") || s.starts_with("0X")
}

/// `0x`-hex addresses compare case-insensitively; anything else (base58) exactly.
fn addr_matches(candidate: &str, allowed: &str) -> bool {
    if is_hex_addr(candidate) && is_hex_addr(allowed) {
        candidate.eq_ignore_ascii_case(allowed)
    } else {
        candidate == allowed
    }
}

fn push_refusal(out: &mut Vec<Refusal>, r: Refusal) {
    if r == Refusal::Overflow && out.contains(&Refusal::Overflow) {
        return;
    }
    out.push(r);
}

/// Run every check and return every failure, ordered: halted, zero qty, zero
/// price, market, order
/// qty, daily qty, order quote, daily quote, price guard, settle. When the
/// market is not tradable the market-dependent checks cannot run, so only
/// halted, market and settle are evaluated.
///
/// `used_today` is `(qty, quote)` already committed today (UTC); see [`utc_day`].
pub fn check_order(
    markets: &BTreeMap<String, MarketLimits>,
    allowlist: &[String],
    halted: Option<&Option<String>>,
    used_today: (u128, u128),
    top: &BookTop,
    req: &OrderRequest,
) -> Result<Commit, Vec<Refusal>> {
    let mut out = Vec::new();
    let mut commit = Commit {
        qty: req.qty,
        quote: 0,
    };

    if let Some(reason) = halted {
        out.push(Refusal::Halted(reason.clone()));
    }

    if req.qty == 0 {
        out.push(Refusal::ZeroQty);
    }
    if req.price == 0 {
        out.push(Refusal::ZeroPrice);
    }

    match markets.get(req.market_id) {
        None => out.push(Refusal::MarketNotTradable(req.market_id.to_string())),
        Some(limits) => {
            if req.qty > limits.max_order_qty {
                out.push(Refusal::OrderQty {
                    qty: req.qty,
                    max: limits.max_order_qty,
                });
            }
            match used_today.0.checked_add(req.qty) {
                None => push_refusal(&mut out, Refusal::Overflow),
                Some(total) if total > limits.max_daily_qty => out.push(Refusal::DailyQty {
                    used: used_today.0,
                    qty: req.qty,
                    max: limits.max_daily_qty,
                }),
                Some(_) => {}
            }
            // Only buys commit quote (the asset a sell gives is the base).
            if req.side == Side::Buy {
                match quote_of(req.qty, req.price, limits.pair_decimals) {
                    None => push_refusal(&mut out, Refusal::Overflow),
                    Some(quote) => {
                        commit.quote = quote;
                        if quote > limits.max_order_quote {
                            out.push(Refusal::OrderQuote {
                                quote,
                                max: limits.max_order_quote,
                            });
                        }
                        match used_today.1.checked_add(quote) {
                            None => push_refusal(&mut out, Refusal::Overflow),
                            Some(total) if total > limits.max_daily_quote => {
                                out.push(Refusal::DailyQuote {
                                    used: used_today.1,
                                    quote,
                                    max: limits.max_daily_quote,
                                })
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
            if let Err(r) = price_guard(req.side, req.price, top, limits.max_deviation_bps) {
                push_refusal(&mut out, r);
            }
        }
    }

    for addr in [req.settle_base, req.settle_quote].into_iter().flatten() {
        if !allowlist.iter().any(|a| addr_matches(addr, a)) {
            out.push(Refusal::SettleAddressNotAllowed(addr.to_string()));
        }
    }

    if out.is_empty() { Ok(commit) } else { Err(out) }
}

/// `YYYY-MM-DD` of a Unix timestamp, in UTC. A timestamp outside chrono's
/// range yields a distinct sentinel bucket rather than a real day.
pub fn utc_day(ts_secs: u64) -> String {
    i64::try_from(ts_secs)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "out-of-range".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> MarketLimits {
        MarketLimits {
            pair_decimals: 3,
            max_order_qty: 10_000,
            max_daily_qty: 25_000,
            max_order_quote: 7_000_000,
            max_daily_quote: 9_000_000,
            max_deviation_bps: 100,
        }
    }

    fn markets_with(l: MarketLimits) -> BTreeMap<String, MarketLimits> {
        BTreeMap::from([("m1".to_string(), l)])
    }

    fn markets() -> BTreeMap<String, MarketLimits> {
        markets_with(limits())
    }

    fn book() -> BookTop {
        BookTop {
            best_bid: Some(1_990_000),
            best_ask: Some(2_010_000),
        }
    }

    fn req(side: Side, qty: u128, price: u128) -> OrderRequest<'static> {
        OrderRequest {
            market_id: "m1",
            side,
            qty,
            price,
            settle_base: None,
            settle_quote: None,
        }
    }

    fn run(used: (u128, u128), top: &BookTop, r: &OrderRequest) -> Result<Commit, Vec<Refusal>> {
        check_order(&markets(), &[], None, used, top, r)
    }

    #[test]
    fn happy_buy_commits_qty_and_quote() {
        let c = run((0, 0), &book(), &req(Side::Buy, 3_000, 2_000_000)).unwrap();
        assert_eq!(
            c,
            Commit {
                qty: 3_000,
                quote: 6_000_000
            }
        );
    }

    #[test]
    fn order_qty_over() {
        let e = run(
            (0, 0),
            &BookTop {
                best_bid: None,
                best_ask: None,
            },
            &req(Side::Buy, 10_001, 1_000),
        )
        .unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::OrderQty {
                qty: 10_001,
                max: 10_000
            }]
        );
        // at the limit passes
        assert!(
            run(
                (0, 0),
                &BookTop {
                    best_bid: None,
                    best_ask: None
                },
                &req(Side::Buy, 10_000, 1_000)
            )
            .is_ok()
        );
    }

    #[test]
    fn daily_qty_over_counts_prior_usage() {
        let top = BookTop {
            best_bid: None,
            best_ask: None,
        };
        let e = run((20_001, 0), &top, &req(Side::Buy, 5_000, 1_000)).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::DailyQty {
                used: 20_001,
                qty: 5_000,
                max: 25_000
            }]
        );
        assert!(run((20_000, 0), &top, &req(Side::Buy, 5_000, 1_000)).is_ok());
    }

    #[test]
    fn order_quote_over() {
        let e = run((0, 0), &book(), &req(Side::Buy, 3_501, 2_000_000)).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::OrderQuote {
                quote: 7_002_000,
                max: 7_000_000
            }]
        );
        let c = run((0, 0), &book(), &req(Side::Buy, 3_500, 2_000_000)).unwrap();
        assert_eq!(c.quote, 7_000_000);
    }

    #[test]
    fn daily_quote_over() {
        let e = run((0, 4_000_000), &book(), &req(Side::Buy, 3_000, 2_000_000)).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::DailyQuote {
                used: 4_000_000,
                quote: 6_000_000,
                max: 9_000_000
            }]
        );
        assert!(run((0, 3_000_000), &book(), &req(Side::Buy, 3_000, 2_000_000)).is_ok());
    }

    #[test]
    fn buy_beyond_ask_deviation() {
        let e = run((0, 0), &book(), &req(Side::Buy, 1, 2_030_101)).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::PriceDeviation {
                price: 2_030_101,
                reference: 2_010_000,
                max_bps: 100
            }]
        );
    }

    #[test]
    fn sell_beyond_bid_deviation() {
        let e = run((0, 0), &book(), &req(Side::Sell, 1, 1_970_099)).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::PriceDeviation {
                price: 1_970_099,
                reference: 1_990_000,
                max_bps: 100
            }]
        );
    }

    #[test]
    fn buy_at_exact_band_edge_passes() {
        // 2_010_000 * 10_100 / 10_000 = 2_030_100 exactly
        assert!(run((0, 0), &book(), &req(Side::Buy, 1, 2_030_100)).is_ok());
    }

    #[test]
    fn sell_at_exact_band_edge_passes() {
        // 1_990_000 * 9_900 / 10_000 = 1_970_100 exactly
        assert!(run((0, 0), &book(), &req(Side::Sell, 1, 1_970_100)).is_ok());
    }

    #[test]
    fn sell_bps_over_10000_clamps_to_zero_floor() {
        let l = MarketLimits {
            max_deviation_bps: 20_000,
            ..limits()
        };
        let r = check_order(
            &markets_with(l),
            &[],
            None,
            (0, 0),
            &book(),
            &req(Side::Sell, 1, 1),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn sell_has_no_quote_commit() {
        // As a buy this breaches max_order_quote and (with this usage) max_daily_quote.
        let r = req(Side::Sell, 3_501, 2_000_000);
        assert!(run((0, 0), &book(), &req(Side::Buy, 3_501, 2_000_000)).is_err());
        let c = run((0, 8_999_999), &book(), &r).unwrap();
        assert_eq!(
            c,
            Commit {
                qty: 3_501,
                quote: 0
            }
        );
    }

    #[test]
    fn no_asks_buy_above_best_bid_refused() {
        let top = BookTop {
            best_bid: Some(1_990_000),
            best_ask: None,
        };
        let e = run((0, 0), &top, &req(Side::Buy, 1, 1_990_001)).unwrap_err();
        assert_eq!(e, vec![Refusal::NoReference { side: Side::Buy }]);
    }

    #[test]
    fn no_asks_buy_at_best_bid_passes() {
        let top = BookTop {
            best_bid: Some(1_990_000),
            best_ask: None,
        };
        assert!(run((0, 0), &top, &req(Side::Buy, 1, 1_990_000)).is_ok());
    }

    #[test]
    fn no_bids_sell_below_best_ask_refused_and_at_ask_passes() {
        let top = BookTop {
            best_bid: None,
            best_ask: Some(2_010_000),
        };
        let e = run((0, 0), &top, &req(Side::Sell, 1, 2_009_999)).unwrap_err();
        assert_eq!(e, vec![Refusal::NoReference { side: Side::Sell }]);
        assert!(run((0, 0), &top, &req(Side::Sell, 1, 2_010_000)).is_ok());
    }

    #[test]
    fn empty_book_passes() {
        let top = BookTop {
            best_bid: None,
            best_ask: None,
        };
        assert!(run((0, 0), &top, &req(Side::Buy, 1, 5_000_000)).is_ok());
        assert!(run((0, 0), &top, &req(Side::Sell, 1, 5_000_000)).is_ok());
    }

    #[test]
    fn settle_override_not_in_allowlist_refused() {
        let allow = vec!["0xAbC0000000000000000000000000000000000001".to_string()];
        let mut r = req(Side::Buy, 1, 2_000_000);
        r.settle_base = Some("0xdead000000000000000000000000000000000002");
        let e = check_order(&markets(), &allow, None, (0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::SettleAddressNotAllowed(
                "0xdead000000000000000000000000000000000002".to_string()
            )]
        );
    }

    #[test]
    fn settle_override_hex_case_insensitive() {
        let allow = vec!["0xAbC0000000000000000000000000000000000001".to_string()];
        let mut r = req(Side::Buy, 1, 2_000_000);
        r.settle_base = Some("0xabc0000000000000000000000000000000000001");
        r.settle_quote = Some("0XABC0000000000000000000000000000000000001");
        assert!(check_order(&markets(), &allow, None, (0, 0), &book(), &r).is_ok());
    }

    #[test]
    fn settle_override_base58_is_exact() {
        let allow = vec!["SoLaNaAddr1".to_string()];
        let mut r = req(Side::Buy, 1, 2_000_000);
        r.settle_quote = Some("SoLaNaAddr1");
        assert!(check_order(&markets(), &allow, None, (0, 0), &book(), &r).is_ok());
        r.settle_quote = Some("solanaaddr1");
        let e = check_order(&markets(), &allow, None, (0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![Refusal::SettleAddressNotAllowed("solanaaddr1".into())]
        );
    }

    #[test]
    fn halted_refuses_with_reason() {
        let h = Some("breaker".to_string());
        let e = check_order(
            &markets(),
            &[],
            Some(&h),
            (0, 0),
            &book(),
            &req(Side::Buy, 1, 2_000_000),
        )
        .unwrap_err();
        assert_eq!(e, vec![Refusal::Halted(Some("breaker".to_string()))]);
        assert!(e[0].to_string().contains("breaker"));
        let none: Option<String> = None;
        let e = check_order(
            &markets(),
            &[],
            Some(&none),
            (0, 0),
            &book(),
            &req(Side::Buy, 1, 2_000_000),
        )
        .unwrap_err();
        assert_eq!(e, vec![Refusal::Halted(None)]);
    }

    #[test]
    fn all_failures_reported() {
        // Daily quote limit raised so exactly qty, quote and deviation break.
        let l = MarketLimits {
            max_daily_quote: 90_000_000,
            ..limits()
        };
        let r = req(Side::Buy, 10_001, 2_100_000);
        let e = check_order(&markets_with(l), &[], None, (0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![
                Refusal::OrderQty {
                    qty: 10_001,
                    max: 10_000
                },
                Refusal::OrderQuote {
                    quote: 21_002_100,
                    max: 7_000_000
                },
                Refusal::PriceDeviation {
                    price: 2_100_000,
                    reference: 2_010_000,
                    max_bps: 100
                },
            ]
        );
    }

    #[test]
    fn refusal_order_follows_check_order() {
        let h = Some("x".to_string());
        let allow: Vec<String> = vec![];
        let mut r = req(Side::Buy, 10_001, 2_100_000);
        r.settle_base = Some("0xbad");
        let e = check_order(
            &markets(),
            &allow,
            Some(&h),
            (20_000, 5_000_000),
            &book(),
            &r,
        )
        .unwrap_err();
        let kinds: Vec<&str> = e
            .iter()
            .map(|x| match x {
                Refusal::Halted(_) => "halted",
                Refusal::OrderQty { .. } => "oq",
                Refusal::DailyQty { .. } => "dq",
                Refusal::OrderQuote { .. } => "oqu",
                Refusal::DailyQuote { .. } => "dqu",
                Refusal::PriceDeviation { .. } => "pd",
                Refusal::SettleAddressNotAllowed(_) => "settle",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["halted", "oq", "dq", "oqu", "dqu", "pd", "settle"]);
    }

    #[test]
    fn unknown_market_returns_only_evaluable_refusals() {
        let h = Some("r".to_string());
        let mut r = req(Side::Buy, 999_999, 9_999_999);
        r.market_id = "nope";
        r.settle_quote = Some("0xbad");
        let e = check_order(&markets(), &[], Some(&h), (0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![
                Refusal::Halted(Some("r".to_string())),
                Refusal::MarketNotTradable("nope".to_string()),
                Refusal::SettleAddressNotAllowed("0xbad".to_string()),
            ]
        );
    }

    #[test]
    fn quote_overflow_is_refusal() {
        let r = req(Side::Buy, u128::MAX, u128::MAX);
        let e = run((0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![
                Refusal::OrderQty {
                    qty: u128::MAX,
                    max: 10_000
                },
                Refusal::DailyQty {
                    used: 0,
                    qty: u128::MAX,
                    max: 25_000
                },
                Refusal::Overflow,
            ]
        );
        assert_eq!(quote_of(u128::MAX, u128::MAX, 3), None);
        // 10^39 does not fit in u128
        assert_eq!(quote_of(1, 1, 39), None);
        // used + qty overflow
        let e = run((u128::MAX, 0), &book(), &req(Side::Sell, 1, 2_000_000)).unwrap_err();
        assert_eq!(e, vec![Refusal::Overflow]);
        // price guard multiplication overflow on a sell (no quote math)
        let e = run((0, 0), &book(), &req(Side::Sell, 1, u128::MAX)).unwrap_err();
        assert_eq!(e, vec![Refusal::Overflow]);
        // used quote + quote overflow
        let e = run((0, u128::MAX), &book(), &req(Side::Buy, 1, 2_000_000)).unwrap_err();
        assert_eq!(e, vec![Refusal::Overflow]);
    }

    #[test]
    fn zero_qty_refused() {
        let e = run((0, 0), &book(), &req(Side::Buy, 0, 2_000_000)).unwrap_err();
        assert_eq!(e, vec![Refusal::ZeroQty]);
        // untradable market: zero checks still run, after halted, before market
        let h = Some("r".to_string());
        let mut r = req(Side::Sell, 0, 2_000_000);
        r.market_id = "nope";
        let e = check_order(&markets(), &[], Some(&h), (0, 0), &book(), &r).unwrap_err();
        assert_eq!(
            e,
            vec![
                Refusal::Halted(Some("r".to_string())),
                Refusal::ZeroQty,
                Refusal::MarketNotTradable("nope".to_string()),
            ]
        );
        assert_eq!(
            Refusal::ZeroQty.to_string(),
            "qty must be greater than zero"
        );
    }

    #[test]
    fn zero_price_refused() {
        let empty = BookTop {
            best_bid: None,
            best_ask: None,
        };
        let e = run((0, 0), &empty, &req(Side::Sell, 5, 0)).unwrap_err();
        assert_eq!(e, vec![Refusal::ZeroPrice]);
        let e = run((0, 0), &empty, &req(Side::Buy, 5, 0)).unwrap_err();
        assert_eq!(e, vec![Refusal::ZeroPrice]);
        let mut r = req(Side::Sell, 0, 0);
        r.market_id = "nope";
        let e = check_order(&markets(), &[], None, (0, 0), &empty, &r).unwrap_err();
        assert_eq!(
            e,
            vec![
                Refusal::ZeroQty,
                Refusal::ZeroPrice,
                Refusal::MarketNotTradable("nope".to_string()),
            ]
        );
        assert_eq!(
            Refusal::ZeroPrice.to_string(),
            "price must be greater than zero"
        );
    }

    #[test]
    fn quote_rounds_up() {
        assert_eq!(quote_of(1, 1, 3), Some(1));
        assert_eq!(quote_of(1_000, 1, 3), Some(1));
        assert_eq!(quote_of(1_001, 1, 3), Some(2));
        assert_eq!(quote_of(0, 5, 3), Some(0));
    }

    #[test]
    fn market_id_must_match_exactly() {
        for id in ["M1", " m1", "m1 "] {
            let mut r = req(Side::Buy, 1, 2_000_000);
            r.market_id = id;
            let e = run((0, 0), &book(), &r).unwrap_err();
            assert_eq!(e, vec![Refusal::MarketNotTradable(id.to_string())]);
        }
    }

    #[test]
    fn daily_usage_resets_at_utc_midnight() {
        // 2026-11-06T23:59:59Z
        assert_eq!(utc_day(1_794_009_599), "2026-11-06");
        assert_eq!(utc_day(1_794_009_600), "2026-11-07");
        assert_eq!(utc_day(0), "1970-01-01");
        // 2028-02-29T12:00:00Z
        assert_eq!(utc_day(1_835_438_400), "2028-02-29");
        assert_eq!(utc_day(u64::MAX), "out-of-range");
    }

    #[test]
    fn display_names_the_rule_and_numbers() {
        let cases = [
            (
                Refusal::OrderQty { qty: 11, max: 10 },
                vec!["max_order_qty", "11", "10"],
            ),
            (
                Refusal::DailyQty {
                    used: 7,
                    qty: 5,
                    max: 9,
                },
                vec!["max_daily_qty", "7", "5", "9"],
            ),
            (
                Refusal::OrderQuote { quote: 21, max: 20 },
                vec!["max_order_quote", "21", "20"],
            ),
            (
                Refusal::DailyQuote {
                    used: 3,
                    quote: 4,
                    max: 6,
                },
                vec!["max_daily_quote", "3", "4", "6"],
            ),
            (
                Refusal::PriceDeviation {
                    price: 101,
                    reference: 90,
                    max_bps: 55,
                },
                vec!["max_deviation_bps", "101", "90", "55"],
            ),
        ];
        for (r, needles) in cases {
            let s = r.to_string();
            for n in needles {
                assert!(s.contains(n), "{s:?} missing {n:?}");
            }
        }
    }
}
