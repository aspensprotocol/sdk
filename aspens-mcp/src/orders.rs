//! Order-state derivation: what the agent sent (local ledger) plus what the
//! venue's owner-signed order-status reads report becomes one state per order.
//! Pure. Rule: report what is known and never guess.
//!
//! The venue is the source of truth for every order it reports: its state,
//! quantities and fills are passed through unchanged. The ledger decides only
//! what the venue cannot: a send the venue definitively refused (`rejected`),
//! and a send with no venue record (`unknown`, never re-sent).

use aspens::commands::trading::arborter_pb::MyOrderState;
use serde::Serialize;

use crate::ledger::{SentOutcome, SentRecord, Side};

/// How long the venue keeps reporting an order after it closes.
pub const VENUE_RETENTION_SECS: u64 = 604_800;

/// An order's state. The first eight are the venue's own `MyOrderState`
/// values; `rejected` and `unknown` come from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    /// The venue reported a state this server does not know.
    Unspecified,
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Expired,
    /// The venue restarted while the order rested, and dropped it.
    DroppedByRestart,
    /// Closed by matching; the venue knows its fills but not its initial size.
    Traded,
    /// The venue definitively refused the send; nothing was placed.
    Rejected,
    /// The venue has no record of the order; the send is never repeated.
    Unknown,
}

impl OrderState {
    /// The state a venue `MyOrderState` value names.
    pub fn from_venue(state: i32) -> OrderState {
        match MyOrderState::try_from(state) {
            Ok(MyOrderState::Open) => OrderState::Open,
            Ok(MyOrderState::PartiallyFilled) => OrderState::PartiallyFilled,
            Ok(MyOrderState::Filled) => OrderState::Filled,
            Ok(MyOrderState::Cancelled) => OrderState::Cancelled,
            Ok(MyOrderState::Expired) => OrderState::Expired,
            Ok(MyOrderState::DroppedByRestart) => OrderState::DroppedByRestart,
            Ok(MyOrderState::Traded) => OrderState::Traded,
            Ok(MyOrderState::Unspecified) | Err(_) => OrderState::Unspecified,
        }
    }

    /// The venue's states that can still fill. An unrecognised state counts
    /// as live, so the agent keeps looking at it.
    fn live(self) -> bool {
        matches!(
            self,
            OrderState::Open | OrderState::PartiallyFilled | OrderState::Unspecified
        )
    }
}

/// The agent's role in a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Maker,
    Taker,
}

/// One fill of an order. Amounts are pair-decimal units; `ts` is the venue's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderFill {
    /// The venue's fill id (`0x` + 64 hex); `None` for a fill known only from
    /// the send response.
    pub fill_id: Option<String>,
    pub role: Role,
    pub qty: u128,
    pub price: u128,
    pub ts: u64,
}

/// One of the agent's orders as the venue reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueOrder {
    /// `0x` + 64 hex.
    pub order_id: String,
    /// `None` only for a fills-only (`Traded`) order, which the venue reports
    /// without a market.
    pub market_id: Option<String>,
    /// `None` only for a fills-only (`Traded`) order, which the venue reports
    /// without a side.
    pub side: Option<Side>,
    pub hidden: bool,
    pub state: OrderState,
    pub filled: u128,
    /// `None` when the venue cannot recover the order's initial size.
    pub remaining: Option<u128>,
    /// Ascending by match time, then fill id.
    pub fills: Vec<OrderFill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderView {
    pub order_id: Option<String>,
    pub market_id: String,
    pub side: Side,
    /// As sent.
    pub qty: u128,
    /// As sent.
    pub price: u128,
    /// `None` when not known.
    pub filled: Option<u128>,
    /// `None` when not known.
    pub remaining: Option<u128>,
    pub fills: Vec<OrderFill>,
    pub state: OrderState,
    /// From the venue; `None` when it does not report the order.
    pub hidden: Option<bool>,
    /// The order can no longer fill (or is past the venue's retention).
    pub closed: bool,
    pub note: Option<String>,
}

/// `0x` + 64 lowercase hex: the one spelling used for every id comparison.
pub fn order_id_hex(id: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(id))
}

/// The id a send is tracked by: the venue's for an accepted order, the one
/// derived before sending for an unanswered one, none for a refusal or an
/// unanswered send recorded before ids were.
pub fn tracked_id(sent: &SentRecord) -> Option<&str> {
    match &sent.outcome {
        SentOutcome::Accepted { order_id, .. } => Some(order_id),
        SentOutcome::Unknown { order_id } => order_id.as_deref(),
        SentOutcome::Failed { .. } => None,
    }
}

fn base(sent: &SentRecord) -> OrderView {
    OrderView {
        order_id: tracked_id(sent).map(str::to_string),
        market_id: sent.market_id.clone(),
        side: sent.side,
        qty: sent.qty,
        price: sent.price,
        filled: None,
        remaining: None,
        fills: Vec::new(),
        state: OrderState::Unknown,
        hidden: None,
        closed: false,
        note: None,
    }
}

fn rejected(mut view: OrderView, code: &str, message: &str) -> OrderView {
    view.state = OrderState::Rejected;
    view.filled = Some(0);
    view.remaining = Some(0);
    view.closed = true;
    view.note = Some(format!("{code}: {message}"));
    view
}

const NOT_RESENT: &str = "no answer from the venue; not re-sent";

/// Derive one order's state from its send and the venue's record of it
/// (`venue`: `None` when the venue has none). `now` is Unix seconds.
pub fn derive(sent: &SentRecord, venue: Option<&VenueOrder>, now: u64) -> OrderView {
    let mut view = base(sent);
    if let SentOutcome::Failed { code, message } = &sent.outcome {
        return rejected(view, code, message);
    }
    if view.order_id.is_none() {
        view.note = Some(NOT_RESENT.into());
        return view;
    }
    match venue {
        Some(v) => {
            view.state = v.state;
            view.filled = Some(v.filled);
            view.remaining = v.remaining;
            view.fills = v.fills.clone();
            view.hidden = Some(v.hidden);
            view.closed = !v.state.live();
            if v.state == OrderState::Unspecified {
                view.note = Some("the venue reported a state this server does not know".into());
            }
        }
        None => match &sent.outcome {
            SentOutcome::Unknown { .. } => {
                view.note = Some(format!(
                    "{NOT_RESENT}, and the venue has no record of this order for you"
                ));
            }
            _ if now.saturating_sub(sent.ts) < VENUE_RETENTION_SECS => {
                view.note = Some(
                    "accepted, but the venue has no record of this order for you yet; check \
                     again later"
                        .into(),
                );
            }
            _ => {
                view.closed = true;
                view.note = Some(
                    "accepted more than 7 days ago; the venue reports a closed order for 7 days \
                     and no longer reports this one"
                        .into(),
                );
            }
        },
    }
    view
}

/// The state of an order just sent, from its send outcome alone: an accepted
/// order the venue put in the book rests with what its taker fills left.
pub fn from_send(sent: &SentRecord) -> OrderView {
    let mut view = base(sent);
    match &sent.outcome {
        SentOutcome::Failed { code, message } => rejected(view, code, message),
        SentOutcome::Unknown { .. } => {
            view.note = Some(NOT_RESENT.into());
            view
        }
        SentOutcome::Accepted {
            in_book,
            taker_fills,
            ..
        } => {
            view.fills = taker_fills
                .iter()
                .map(|f| OrderFill {
                    fill_id: None,
                    role: Role::Taker,
                    qty: f.qty,
                    price: f.price,
                    ts: f.ts,
                })
                .collect();
            let filled = taker_fills
                .iter()
                .fold(0u128, |acc, f| acc.saturating_add(f.qty));
            view.filled = Some(filled);
            if *in_book {
                view.remaining = Some(sent.qty.saturating_sub(filled));
                view.state = if filled > 0 {
                    OrderState::PartiallyFilled
                } else {
                    OrderState::Open
                };
            } else if filled >= sent.qty {
                view.remaining = Some(0);
                view.state = OrderState::Filled;
                view.closed = true;
            } else {
                view.note = Some(
                    "accepted and not resting, but the response does not fill it; check it \
                     with get_order"
                        .into(),
                );
            }
            view
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Fill;

    const ID_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_D: &str = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const SENT_AT: u64 = 1_700_000_000;

    fn sent(outcome: SentOutcome) -> SentRecord {
        SentRecord {
            ts: SENT_AT,
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

    fn venue(id: &str, state: OrderState, filled: u128, remaining: Option<u128>) -> VenueOrder {
        VenueOrder {
            order_id: id.into(),
            market_id: Some("m1".into()),
            side: Some(Side::Buy),
            hidden: true,
            state,
            filled,
            remaining,
            fills: vec![OrderFill {
                fill_id: Some(ID_D.into()),
                role: Role::Maker,
                qty: filled,
                price: 249,
                ts: 77,
            }],
        }
    }

    #[test]
    fn order_id_hex_is_prefixed_lowercase() {
        assert_eq!(order_id_hex(&[0xab; 32]), format!("0x{}", "ab".repeat(32)));
    }

    #[test]
    fn every_venue_state_maps() {
        let cases = [
            (
                MyOrderState::Unspecified,
                OrderState::Unspecified,
                "unspecified",
            ),
            (MyOrderState::Open, OrderState::Open, "open"),
            (
                MyOrderState::PartiallyFilled,
                OrderState::PartiallyFilled,
                "partially_filled",
            ),
            (MyOrderState::Filled, OrderState::Filled, "filled"),
            (MyOrderState::Cancelled, OrderState::Cancelled, "cancelled"),
            (MyOrderState::Expired, OrderState::Expired, "expired"),
            (
                MyOrderState::DroppedByRestart,
                OrderState::DroppedByRestart,
                "dropped_by_restart",
            ),
            (MyOrderState::Traded, OrderState::Traded, "traded"),
        ];
        for (wire, state, name) in cases {
            assert_eq!(OrderState::from_venue(wire as i32), state);
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
        assert_eq!(OrderState::from_venue(42), OrderState::Unspecified);
        assert_eq!(
            serde_json::to_value(OrderState::Unknown).unwrap(),
            "unknown"
        );
        assert_eq!(
            serde_json::to_value(OrderState::Rejected).unwrap(),
            "rejected"
        );
    }

    #[test]
    fn failed_is_rejected() {
        let s = sent(SentOutcome::Failed {
            code: "INVALID".into(),
            message: "bad".into(),
        });
        let v = derive(&s, None, SENT_AT);
        assert_eq!(v.state, OrderState::Rejected);
        assert_eq!(v.order_id, None);
        assert!(v.closed);
        assert_eq!(v.note.as_deref(), Some("INVALID: bad"));
    }

    #[test]
    fn venue_record_is_passed_through() {
        let rec = venue(ID_A, OrderState::Cancelled, 400, Some(600));
        let v = derive(&accepted(true, vec![]), Some(&rec), SENT_AT);
        assert_eq!(v.state, OrderState::Cancelled);
        assert_eq!(v.filled, Some(400));
        assert_eq!(v.remaining, Some(600));
        assert_eq!(v.fills, rec.fills);
        assert_eq!(v.hidden, Some(true));
        assert!(v.closed);
        assert_eq!(v.note, None);
        // The sent qty and price stay as sent.
        assert_eq!((v.qty, v.price), (1000, 250));
    }

    #[test]
    fn live_venue_states_are_open() {
        for state in [
            OrderState::Open,
            OrderState::PartiallyFilled,
            OrderState::Unspecified,
        ] {
            let v = derive(
                &accepted(true, vec![]),
                Some(&venue(ID_A, state, 0, Some(1000))),
                SENT_AT,
            );
            assert!(!v.closed, "{state:?}");
        }
        for state in [
            OrderState::Filled,
            OrderState::Expired,
            OrderState::DroppedByRestart,
            OrderState::Traded,
        ] {
            let v = derive(
                &accepted(true, vec![]),
                Some(&venue(ID_A, state, 3, None)),
                SENT_AT,
            );
            assert!(v.closed, "{state:?}");
        }
    }

    #[test]
    fn traded_reports_no_remaining() {
        let v = derive(
            &accepted(true, vec![]),
            Some(&venue(ID_A, OrderState::Traded, 300, None)),
            SENT_AT,
        );
        assert_eq!(v.state, OrderState::Traded);
        assert_eq!(v.filled, Some(300));
        assert_eq!(v.remaining, None);
    }

    #[test]
    fn unknown_with_derived_id_takes_the_venue_record() {
        let s = sent(SentOutcome::Unknown {
            order_id: Some(ID_D.into()),
        });
        let v = derive(
            &s,
            Some(&venue(ID_D, OrderState::PartiallyFilled, 350, Some(650))),
            SENT_AT,
        );
        assert_eq!(v.state, OrderState::PartiallyFilled);
        assert_eq!(v.order_id.as_deref(), Some(ID_D));
        assert_eq!(v.remaining, Some(650));
    }

    #[test]
    fn unknown_the_venue_does_not_know_stays_unknown() {
        let s = sent(SentOutcome::Unknown {
            order_id: Some(ID_D.into()),
        });
        let v = derive(&s, None, SENT_AT + 10 * VENUE_RETENTION_SECS);
        assert_eq!(v.state, OrderState::Unknown);
        assert_eq!(v.order_id.as_deref(), Some(ID_D));
        assert!(!v.closed);
        assert!(v.note.unwrap().contains("not re-sent"));
    }

    #[test]
    fn unknown_without_id_stays_unknown() {
        let v = derive(
            &sent(SentOutcome::Unknown { order_id: None }),
            None,
            SENT_AT,
        );
        assert_eq!(v.state, OrderState::Unknown);
        assert_eq!(v.order_id, None);
        assert_eq!(v.note.as_deref(), Some(NOT_RESENT));
    }

    #[test]
    fn accepted_without_a_venue_record_ages_into_closed() {
        let s = accepted(true, vec![]);
        let fresh = derive(&s, None, SENT_AT + VENUE_RETENTION_SECS - 1);
        assert_eq!(fresh.state, OrderState::Unknown);
        assert!(!fresh.closed);
        assert!(fresh.note.unwrap().contains("no record"));
        let old = derive(&s, None, SENT_AT + VENUE_RETENTION_SECS);
        assert_eq!(old.state, OrderState::Unknown);
        assert!(old.closed);
        assert!(old.note.unwrap().contains("7 days"));
        assert_eq!(old.filled, None);
    }

    #[test]
    fn placed_in_book_rests_with_what_taker_fills_left() {
        let s = accepted(
            true,
            vec![Fill {
                qty: 300,
                price: 249,
                ts: 9,
            }],
        );
        let v = from_send(&s);
        assert_eq!(v.state, OrderState::PartiallyFilled);
        assert_eq!(v.filled, Some(300));
        assert_eq!(v.remaining, Some(700));
        assert_eq!(v.fills[0].role, Role::Taker);
        assert_eq!(v.fills[0].fill_id, None);
        let v = from_send(&accepted(true, vec![]));
        assert_eq!((v.state, v.remaining), (OrderState::Open, Some(1000)));
    }

    #[test]
    fn placed_and_fully_taken_is_filled() {
        let s = accepted(
            false,
            vec![
                Fill {
                    qty: 400,
                    price: 249,
                    ts: 9,
                },
                Fill {
                    qty: 600,
                    price: 248,
                    ts: 10,
                },
            ],
        );
        let v = from_send(&s);
        assert_eq!(v.state, OrderState::Filled);
        assert_eq!(v.filled, Some(1000));
        assert!(v.closed);
    }

    #[test]
    fn placed_unanswered_carries_its_derived_id() {
        let v = from_send(&sent(SentOutcome::Unknown {
            order_id: Some(ID_D.into()),
        }));
        assert_eq!(v.state, OrderState::Unknown);
        assert_eq!(v.order_id.as_deref(), Some(ID_D));
    }
}
