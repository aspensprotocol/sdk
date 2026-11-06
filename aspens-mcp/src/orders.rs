//! Order-state derivation: what the agent sent (local ledger) plus what the venue
//! shows (trader-filtered book and fills) becomes one state per order. Pure.
//! Rule: report what is known and never guess.

use std::collections::{BTreeMap, BTreeSet};

use aspens::commands::trading::book_snapshot::BookLevelOrder;
use serde::Serialize;

use crate::ledger::{Fill, SentOutcome, SentRecord, Side, u128_str};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Unknown,
    ClosedUnattributed,
}

/// What the venue currently shows for this trader. Keys are `order_id_hex` strings.
#[derive(Debug, Clone, Default)]
pub struct VenueView {
    /// Order id -> remaining quantity of the order resting in the book.
    pub in_book: BTreeMap<String, u128>,
    /// Order id -> fills the venue reports for that order.
    pub fills: BTreeMap<String, Vec<Fill>>,
    /// Fills that could not be tied to an order id.
    pub unattributed_fills: Vec<(Side, Fill)>,
}

mod opt_u128_str {
    use serde::Serializer;

    pub fn serialize<S: Serializer>(v: &Option<u128>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_str(&v.to_string()),
            None => s.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderView {
    pub order_id: Option<String>,
    pub market_id: String,
    pub side: Side,
    #[serde(with = "u128_str")]
    pub qty: u128,
    #[serde(with = "u128_str")]
    pub price: u128,
    #[serde(with = "u128_str")]
    pub filled: u128,
    #[serde(serialize_with = "opt_u128_str::serialize")]
    pub remaining: Option<u128>,
    pub fills: Vec<Fill>,
    pub state: OrderState,
    pub note: Option<String>,
}

/// `0x` + 64 lowercase hex: the one spelling used for every id comparison.
pub fn order_id_hex(id: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(id))
}

/// Union of `a` and `b`, de-duplicated on `(ts, qty, price)`, first occurrence kept.
fn union_fills(a: &[Fill], b: &[Fill]) -> Vec<Fill> {
    let mut seen = BTreeSet::new();
    a.iter()
        .chain(b)
        .filter(|f| seen.insert((f.ts, f.qty, f.price)))
        .cloned()
        .collect()
}

/// Derive one order's state. `resolved_id` is the id the caller found for an
/// `Unknown` send (see `resolve_unknown`); it is ignored for any other outcome.
pub fn derive(
    sent: &SentRecord,
    resolved_id: Option<&str>,
    cancelled: bool,
    venue: &VenueView,
) -> OrderView {
    let mut view = OrderView {
        order_id: None,
        market_id: sent.market_id.clone(),
        side: sent.side,
        qty: sent.qty,
        price: sent.price,
        filled: 0,
        remaining: None,
        fills: Vec::new(),
        state: OrderState::Unknown,
        note: None,
    };
    let (id, taker_fills): (&str, &[Fill]) = match &sent.outcome {
        SentOutcome::Failed { code, message } => {
            view.state = OrderState::Rejected;
            view.remaining = Some(0);
            view.note = Some(format!("{code}: {message}"));
            return view;
        }
        SentOutcome::Accepted {
            order_id,
            taker_fills,
            ..
        } => (order_id, taker_fills),
        SentOutcome::Unknown => match resolved_id {
            Some(id) => (id, &[]),
            None => {
                view.note = Some("no answer from the venue; not re-sent".into());
                return view;
            }
        },
    };

    view.order_id = Some(id.to_string());
    view.fills = union_fills(
        taker_fills,
        venue.fills.get(id).map_or(&[][..], Vec::as_slice),
    );
    view.filled = view
        .fills
        .iter()
        .fold(0u128, |acc, f| acc.saturating_add(f.qty));

    if let Some(remaining) = venue.in_book.get(id) {
        view.remaining = Some(*remaining);
        view.state = if view.filled > 0 {
            OrderState::PartiallyFilled
        } else {
            OrderState::Open
        };
    } else if view.filled >= sent.qty {
        view.remaining = Some(0);
        view.state = OrderState::Filled;
    } else if cancelled {
        view.state = OrderState::Cancelled;
    } else {
        view.state = OrderState::ClosedUnattributed;
        view.note = Some("filled or cancelled".into());
    }
    view
}

/// Find the id of an `Unknown` send among `candidates`: the same-side orders of the
/// trader-filtered book for `sent.market_id`. A candidate matches on equal price, a
/// quantity no larger than the send, and an id the ledger does not already hold.
/// Exactly one match returns its id; zero or several return `None`.
pub fn resolve_unknown(
    sent: &SentRecord,
    candidates: &[&BookLevelOrder],
    known_ids: &BTreeSet<String>,
) -> Option<String> {
    let mut matches = candidates
        .iter()
        .filter(|c| c.price == sent.price && c.quantity <= sent.qty)
        .map(|c| order_id_hex(&c.order_id))
        .filter(|id| !known_ids.contains(id));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn fill(qty: u128, price: u128, ts: u64) -> Fill {
        Fill { qty, price, ts }
    }

    fn sent(outcome: SentOutcome) -> SentRecord {
        SentRecord {
            ts: 100,
            market_id: "m1".into(),
            side: Side::Buy,
            qty: 1000,
            price: 250,
            outcome,
        }
    }

    fn accepted(in_book: bool, taker_fills: Vec<Fill>) -> SentRecord {
        sent(SentOutcome::Accepted {
            order_id: ID_A.into(),
            in_book,
            taker_fills,
        })
    }

    fn venue_book(remaining: u128) -> VenueView {
        let mut v = VenueView::default();
        v.in_book.insert(ID_A.into(), remaining);
        v
    }

    fn book_order(id_byte: u8, price: u128, quantity: u128) -> BookLevelOrder {
        BookLevelOrder {
            order_id: [id_byte; 32],
            price,
            quantity,
            maker_base_address: "0xbase".into(),
            maker_quote_address: "0xquote".into(),
            timestamp: 7,
        }
    }

    #[test]
    fn order_id_hex_is_prefixed_lowercase() {
        assert_eq!(order_id_hex(&[0xab; 32]), format!("0x{}", "ab".repeat(32)));
    }

    #[test]
    fn failed_is_rejected() {
        let s = sent(SentOutcome::Failed {
            code: "INVALID".into(),
            message: "bad".into(),
        });
        let v = derive(&s, None, false, &VenueView::default());
        assert_eq!(v.state, OrderState::Rejected);
        assert_eq!(v.order_id, None);
        assert_eq!(v.note.as_deref(), Some("INVALID: bad"));
    }

    #[test]
    fn in_book_untouched_is_open() {
        let v = derive(&accepted(true, vec![]), None, false, &venue_book(1000));
        assert_eq!(v.state, OrderState::Open);
        assert_eq!(v.filled, 0);
        assert_eq!(v.remaining, Some(1000));
        assert_eq!(v.order_id.as_deref(), Some(ID_A));
    }

    #[test]
    fn in_book_with_fills_is_partially_filled() {
        let mut venue = venue_book(600);
        venue.fills.insert(ID_A.into(), vec![fill(400, 250, 5)]);
        let v = derive(&accepted(true, vec![]), None, false, &venue);
        assert_eq!(v.state, OrderState::PartiallyFilled);
        assert_eq!(v.filled, 400);
        assert_eq!(v.remaining, Some(600));
    }

    #[test]
    fn not_in_book_fully_filled_is_filled() {
        let mut venue = VenueView::default();
        venue
            .fills
            .insert(ID_A.into(), vec![fill(300, 250, 5), fill(700, 250, 6)]);
        let v = derive(&accepted(true, vec![]), None, false, &venue);
        assert_eq!(v.state, OrderState::Filled);
        assert_eq!(v.filled, 1000);
        assert_eq!(v.remaining, Some(0));
    }

    #[test]
    fn taker_fully_filled_in_response_is_filled() {
        let s = accepted(false, vec![fill(1000, 249, 100)]);
        let v = derive(&s, None, false, &VenueView::default());
        assert_eq!(v.state, OrderState::Filled);
        assert_eq!(v.filled, 1000);
    }

    #[test]
    fn not_in_book_partial_and_cancelled_is_cancelled() {
        let mut venue = VenueView::default();
        venue.fills.insert(ID_A.into(), vec![fill(400, 250, 5)]);
        let v = derive(&accepted(true, vec![]), None, true, &venue);
        assert_eq!(v.state, OrderState::Cancelled);
        assert_eq!(v.filled, 400);
        assert_eq!(v.remaining, None);
    }

    #[test]
    fn not_in_book_partial_not_cancelled_is_closed_unattributed() {
        let mut venue = VenueView::default();
        venue.fills.insert(ID_A.into(), vec![fill(400, 250, 5)]);
        let v = derive(&accepted(true, vec![]), None, false, &venue);
        assert_eq!(v.state, OrderState::ClosedUnattributed);
        assert_eq!(v.note.as_deref(), Some("filled or cancelled"));
    }

    #[test]
    fn taker_fill_not_double_counted() {
        let mut venue = venue_book(700);
        venue.fills.insert(ID_A.into(), vec![fill(300, 249, 100)]);
        let s = accepted(true, vec![fill(300, 249, 100)]);
        let v = derive(&s, None, false, &venue);
        assert_eq!(v.filled, 300);
        assert_eq!(v.fills.len(), 1);
        assert_eq!(v.state, OrderState::PartiallyFilled);
    }

    #[test]
    fn distinct_fills_with_same_qty_both_count() {
        let mut venue = VenueView::default();
        venue
            .fills
            .insert(ID_A.into(), vec![fill(300, 250, 5), fill(300, 250, 6)]);
        let v = derive(&accepted(true, vec![]), None, false, &venue);
        assert_eq!(v.filled, 600);
    }

    #[test]
    fn cancelled_but_fully_filled_reports_filled() {
        let mut venue = VenueView::default();
        venue.fills.insert(ID_A.into(), vec![fill(1000, 250, 5)]);
        let v = derive(&accepted(true, vec![]), None, true, &venue);
        assert_eq!(v.state, OrderState::Filled);
    }

    #[test]
    fn filled_sum_saturates() {
        let mut venue = VenueView::default();
        venue
            .fills
            .insert(ID_A.into(), vec![fill(u128::MAX, 1, 1), fill(5, 1, 2)]);
        let v = derive(&accepted(true, vec![]), None, false, &venue);
        assert_eq!(v.filled, u128::MAX);
        assert_eq!(v.state, OrderState::Filled);
    }

    #[test]
    fn unknown_without_id_stays_unknown() {
        let v = derive(&sent(SentOutcome::Unknown), None, false, &venue_book(1));
        assert_eq!(v.state, OrderState::Unknown);
        assert_eq!(v.order_id, None);
        assert_eq!(
            v.note.as_deref(),
            Some("no answer from the venue; not re-sent")
        );
    }

    #[test]
    fn unknown_with_resolved_id_uses_venue() {
        let mut venue = VenueView::default();
        venue.in_book.insert(ID_B.into(), 1000);
        let v = derive(&sent(SentOutcome::Unknown), Some(ID_B), false, &venue);
        assert_eq!(v.state, OrderState::Open);
        assert_eq!(v.order_id.as_deref(), Some(ID_B));
    }

    #[test]
    fn accepted_ignores_resolved_id() {
        let v = derive(
            &accepted(true, vec![]),
            Some(ID_B),
            false,
            &venue_book(1000),
        );
        assert_eq!(v.order_id.as_deref(), Some(ID_A));
    }

    #[test]
    fn unknown_resolves_on_single_match() {
        let s = sent(SentOutcome::Unknown);
        let hit = book_order(0xcc, 250, 1000);
        let other_price = book_order(0xdd, 251, 1000);
        let got = resolve_unknown(&s, &[&hit, &other_price], &BTreeSet::new());
        assert_eq!(got, Some(order_id_hex(&[0xcc; 32])));
    }

    #[test]
    fn unknown_stays_unknown_on_two_matches() {
        let s = sent(SentOutcome::Unknown);
        let a = book_order(0xcc, 250, 1000);
        let b = book_order(0xdd, 250, 400);
        assert_eq!(resolve_unknown(&s, &[&a, &b], &BTreeSet::new()), None);
    }

    #[test]
    fn unknown_with_no_match_is_none() {
        let s = sent(SentOutcome::Unknown);
        assert_eq!(resolve_unknown(&s, &[], &BTreeSet::new()), None);
    }

    #[test]
    fn unknown_ignores_ids_already_in_ledger() {
        let s = sent(SentOutcome::Unknown);
        let known = book_order(0xcc, 250, 1000);
        let fresh = book_order(0xdd, 250, 1000);
        let ids = BTreeSet::from([order_id_hex(&[0xcc; 32])]);
        assert_eq!(
            resolve_unknown(&s, &[&known, &fresh], &ids),
            Some(order_id_hex(&[0xdd; 32]))
        );
    }

    #[test]
    fn unknown_ignores_larger_quantity() {
        let s = sent(SentOutcome::Unknown);
        let too_big = book_order(0xcc, 250, 1001);
        assert_eq!(resolve_unknown(&s, &[&too_big], &BTreeSet::new()), None);
    }

    #[test]
    fn view_serializes_u128_as_strings() {
        let v = derive(&accepted(true, vec![]), None, false, &venue_book(1000));
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["qty"], "1000");
        assert_eq!(j["remaining"], "1000");
        assert_eq!(j["state"], "open");
    }
}
