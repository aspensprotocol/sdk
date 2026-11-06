//! Strict orderbook snapshot: unlike `fetch_top_of_book`, this maintains book
//! state, so an order cancelled or filled inside the collection window is
//! removed rather than left behind as a phantom best price.

use std::collections::BTreeMap;
use std::time::Duration;

use eyre::Result;

use super::arborter_pb::{OrderState, OrderbookEntry, Side};
use super::stream_orderbook::{StreamOrderbookOptions, stream_orderbook_channel};

/// A resting order on one side of the book. Price and quantity are raw
/// pair-decimal integers, as reported on the wire.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BookLevelOrder {
    pub order_id: [u8; 32],
    pub price: u128,
    pub quantity: u128,
    pub maker_base_address: String,
    pub maker_quote_address: String,
    pub timestamp: u64,
}

/// Open orders keyed by their 32-byte order id.
#[derive(Debug, Clone, Default)]
pub struct BookState {
    bids: BTreeMap<[u8; 32], BookLevelOrder>,
    asks: BTreeMap<[u8; 32], BookLevelOrder>,
}

impl BookState {
    /// Apply one stream entry. Entries with an id that is not exactly 32
    /// bytes are skipped. A Confirmed entry with positive price and quantity
    /// inserts or replaces the order on its side (and clears the id from the
    /// other side); any other entry for a valid id removes it.
    pub fn apply(&mut self, entry: &OrderbookEntry) {
        let Ok(id) = <[u8; 32]>::try_from(entry.order_id.as_slice()) else {
            return;
        };
        let side = match Side::try_from(entry.side) {
            Ok(s @ (Side::Bid | Side::Ask)) => s,
            _ => return,
        };
        let live = if entry.state == OrderState::Confirmed as i32 {
            // Unparseable numbers on a Confirmed entry: ignore the entry
            // entirely rather than guess.
            let (Ok(price), Ok(quantity)) =
                (entry.price.parse::<u128>(), entry.quantity.parse::<u128>())
            else {
                return;
            };
            (price > 0 && quantity > 0).then_some((price, quantity))
        } else {
            None
        };
        self.bids.remove(&id);
        self.asks.remove(&id);
        if let Some((price, quantity)) = live {
            let order = BookLevelOrder {
                order_id: id,
                price,
                quantity,
                maker_base_address: entry.maker_base_address.clone(),
                maker_quote_address: entry.maker_quote_address.clone(),
                timestamp: entry.timestamp,
            };
            match side {
                Side::Bid => self.bids.insert(id, order),
                _ => self.asks.insert(id, order),
            };
        }
    }

    pub fn best_bid(&self) -> Option<u128> {
        self.bids.values().map(|o| o.price).max()
    }

    pub fn best_ask(&self) -> Option<u128> {
        self.asks.values().map(|o| o.price).min()
    }

    /// Bids, highest price first (ties by timestamp, then id).
    pub fn bids(&self) -> Vec<&BookLevelOrder> {
        let mut v: Vec<_> = self.bids.values().collect();
        v.sort_by(|a, b| {
            b.price
                .cmp(&a.price)
                .then(a.timestamp.cmp(&b.timestamp))
                .then(a.order_id.cmp(&b.order_id))
        });
        v
    }

    /// Asks, lowest price first (ties by timestamp, then id).
    pub fn asks(&self) -> Vec<&BookLevelOrder> {
        let mut v: Vec<_> = self.asks.values().collect();
        v.sort_by(|a, b| {
            a.price
                .cmp(&b.price)
                .then(a.timestamp.cmp(&b.timestamp))
                .then(a.order_id.cmp(&b.order_id))
        });
        v
    }
}

/// Collect the orderbook stream for `window` (historical open orders first,
/// then live updates) and return the resulting book state.
pub async fn fetch_book_snapshot(
    url: String,
    market_id: String,
    filter_by_trader: Option<String>,
    window: Duration,
) -> Result<BookState> {
    let (mut rx, handle) = stream_orderbook_channel(
        url,
        StreamOrderbookOptions {
            market_id,
            historical_open_orders: true,
            filter_by_trader,
        },
    )
    .await?;

    let mut book = BookState::default();
    let deadline = tokio::time::sleep(window);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            maybe_entry = rx.recv() => {
                let Some(entry) = maybe_entry else { break };
                book.apply(&entry);
            }
        }
    }

    // The stream never ends on its own; stop it rather than leak it.
    handle.abort();

    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> Vec<u8> {
        vec![n; 32]
    }

    fn entry(n: u8, side: Side, state: OrderState, price: &str, qty: &str) -> OrderbookEntry {
        OrderbookEntry {
            timestamp: 1_000 + n as u64,
            order_id: id(n),
            price: price.to_string(),
            quantity: qty.to_string(),
            side: side as i32,
            maker_base_address: format!("base{n}"),
            maker_quote_address: format!("quote{n}"),
            state: state as i32,
            ..Default::default()
        }
    }

    #[test]
    fn added_then_canceled_inside_window_is_not_best() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "105", "7"));
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "103", "7"));
        assert_eq!(b.best_ask(), Some(103));
        b.apply(&entry(2, Side::Ask, OrderState::Canceled, "103", "7"));
        assert_eq!(b.best_ask(), Some(105));
    }

    #[test]
    fn matched_or_zero_quantity_removes() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Bid, OrderState::Confirmed, "90", "5"));
        b.apply(&entry(2, Side::Bid, OrderState::Confirmed, "91", "5"));
        b.apply(&entry(1, Side::Bid, OrderState::Matched, "90", "5"));
        assert_eq!(b.bids().len(), 1);
        b.apply(&entry(2, Side::Bid, OrderState::Confirmed, "91", "0"));
        assert!(b.bids().is_empty());
        assert_eq!(b.best_bid(), None);
    }

    #[test]
    fn requantity_replaces() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "105", "7"));
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "105", "3"));
        let asks = b.asks();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].quantity, 3);
    }

    #[test]
    fn unparseable_price_is_ignored_not_panicking() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "105", "7"));
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "abc", "7"));
        assert_eq!(b.asks().len(), 1);
        assert_eq!(b.best_ask(), Some(105));
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "abc", "7"));
        assert_eq!(b.asks().len(), 1);
    }

    #[test]
    fn side_unspecified_is_ignored() {
        let mut b = BookState::default();
        let mut e = entry(1, Side::Ask, OrderState::Confirmed, "105", "7");
        e.side = 0;
        b.apply(&e);
        assert!(b.asks().is_empty() && b.bids().is_empty());
    }

    #[test]
    fn wrong_length_id_is_skipped() {
        let mut b = BookState::default();
        let mut e = entry(1, Side::Ask, OrderState::Confirmed, "105", "7");
        e.order_id = vec![1; 8];
        b.apply(&e);
        assert!(b.asks().is_empty());
    }

    #[test]
    fn ordering() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Bid, OrderState::Confirmed, "90", "5"));
        b.apply(&entry(2, Side::Bid, OrderState::Confirmed, "92", "6"));
        b.apply(&entry(3, Side::Bid, OrderState::Confirmed, "91", "4"));
        b.apply(&entry(4, Side::Ask, OrderState::Confirmed, "110", "5"));
        b.apply(&entry(5, Side::Ask, OrderState::Confirmed, "108", "6"));
        b.apply(&entry(6, Side::Ask, OrderState::Confirmed, "109", "4"));
        let bids: Vec<u128> = b.bids().iter().map(|o| o.price).collect();
        let asks: Vec<u128> = b.asks().iter().map(|o| o.price).collect();
        assert_eq!(bids, vec![92, 91, 90]);
        assert_eq!(asks, vec![108, 109, 110]);
        assert_eq!(b.best_bid(), Some(92));
        assert_eq!(b.best_ask(), Some(108));
        assert_eq!(b.asks()[0].maker_quote_address, "quote5");
    }
}
