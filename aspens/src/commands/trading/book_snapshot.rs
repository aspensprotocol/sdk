//! Strict orderbook snapshot: unlike `fetch_top_of_book`, this maintains book
//! state, so an order cancelled or filled inside the collection window is
//! removed rather than left behind as a phantom best price.

use std::collections::BTreeMap;
use std::time::Duration;

use eyre::Result;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::arborter_pb::{OrderState, OrderbookEntry, Side};
use super::stream_orderbook::{StreamOrderbookOptions, apply_slippage, stream_orderbook_channel};

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
        // A Confirmed entry needs a valid side to be placed; removal does not.
        let side = match Side::try_from(entry.side) {
            Ok(s @ (Side::Bid | Side::Ask)) => Some(s),
            _ => None,
        };
        if live.is_some() && side.is_none() {
            return;
        }
        self.bids.remove(&id);
        self.asks.remove(&id);
        if let (Some((price, quantity)), Some(side)) = (live, side) {
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

/// Slippage-capped limit price (raw pair-decimal integer) from a top of book.
/// A buy references the best ask, a sell the best bid. The error names no
/// market; the caller adds that context.
pub fn marketable_price_from_top(
    best_bid: Option<u128>,
    best_ask: Option<u128>,
    is_buy: bool,
    slippage_bps: u32,
) -> Result<u128> {
    let reference = if is_buy {
        best_ask.ok_or_else(|| {
            eyre::eyre!(
                "no resting asks — cannot compute a marketable buy price. \
                 Place a limit buy at a price you're willing to pay."
            )
        })?
    } else {
        best_bid.ok_or_else(|| {
            eyre::eyre!(
                "no resting bids — cannot compute a marketable sell price. \
                 Place a limit sell at a price you're willing to accept."
            )
        })?
    };
    apply_slippage(reference, slippage_bps, is_buy)
}

/// Slippage-capped limit price from a snapshot's best opposing price.
pub fn marketable_price(book: &BookState, is_buy: bool, slippage_bps: u32) -> Result<u128> {
    marketable_price_from_top(book.best_bid(), book.best_ask(), is_buy, slippage_bps)
}

/// Collect the orderbook stream for `window` (historical open orders first,
/// then live updates) and return the resulting book state.
///
/// A stream that fails, or ends without succeeding, before or at the deadline
/// is an error, never an empty book: with `filter_by_trader` an empty book
/// reads as "no open orders".
pub async fn fetch_book_snapshot(
    url: String,
    market_id: String,
    filter_by_trader: Option<String>,
    window: Duration,
) -> Result<BookState> {
    let (rx, handle) = stream_orderbook_channel(
        url,
        StreamOrderbookOptions {
            market_id,
            historical_open_orders: true,
            filter_by_trader,
        },
    )
    .await?;
    collect_book(rx, handle, window).await
}

/// Aborts the task when dropped; dropping a bare `JoinHandle` detaches it.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Apply entries from `rx` until `window` elapses, then stop the stream task.
/// The task is also stopped if this future is dropped.
async fn collect_book(
    mut rx: mpsc::Receiver<OrderbookEntry>,
    handle: JoinHandle<Result<()>>,
    window: Duration,
) -> Result<BookState> {
    let mut handle = AbortOnDrop(handle);
    let mut book = BookState::default();
    let deadline = tokio::time::sleep(window);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            // The task's own outcome first, so a failure at the deadline wins.
            biased;
            res = &mut handle.0 => {
                finished(res)?;
                while let Ok(entry) = rx.try_recv() {
                    book.apply(&entry);
                }
                return Ok(book);
            }
            maybe_entry = rx.recv() => match maybe_entry {
                Some(entry) => book.apply(&entry),
                // The sender is gone, so the task has ended: report how.
                None => {
                    finished((&mut handle.0).await)?;
                    return Ok(book);
                }
            },
            _ = &mut deadline => {
                // The stream never ends on its own; stop it rather than leak it.
                handle.0.abort();
                return Ok(book);
            }
        }
    }
}

/// The stream task's result: its own error, or a join error (panic, abort).
fn finished(res: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    res.map_err(|e| eyre::eyre!("orderbook stream task: {e}"))?
        .map_err(|e| e.wrap_err("orderbook stream"))
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
    fn removal_ignores_side() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Ask, OrderState::Confirmed, "105", "7"));
        let mut e = entry(1, Side::Ask, OrderState::Canceled, "105", "7");
        e.side = 0;
        b.apply(&e);
        assert!(b.asks().is_empty());
        // A Confirmed entry with an invalid side stays ignored.
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "105", "7"));
        let mut c = entry(2, Side::Ask, OrderState::Confirmed, "105", "7");
        c.side = 0;
        b.apply(&c);
        assert_eq!(b.asks().len(), 1);
    }

    #[test]
    fn marketable_buy_caps_off_best_ask() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Bid, OrderState::Confirmed, "1990", "5"));
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "2000", "5"));
        assert_eq!(marketable_price(&b, true, 50).unwrap(), 2010);
    }

    #[test]
    fn marketable_sell_caps_off_best_bid() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Bid, OrderState::Confirmed, "1990", "5"));
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "2000", "5"));
        assert_eq!(marketable_price(&b, false, 50).unwrap(), 1980);
    }

    #[test]
    fn marketable_errors_on_empty_reference_side() {
        let mut b = BookState::default();
        b.apply(&entry(1, Side::Bid, OrderState::Confirmed, "1990", "5"));
        let e = marketable_price(&b, true, 50).unwrap_err().to_string();
        assert!(e.contains("no resting asks"), "{e}");
        let mut b = BookState::default();
        b.apply(&entry(2, Side::Ask, OrderState::Confirmed, "2000", "5"));
        let e = marketable_price(&b, false, 50).unwrap_err().to_string();
        assert!(e.contains("no resting bids"), "{e}");
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

    mod collect {
        use super::super::*;
        use std::sync::Arc;

        fn live(n: u8) -> OrderbookEntry {
            OrderbookEntry {
                timestamp: 2_000 + n as u64,
                order_id: vec![n; 32],
                price: "300".to_string(),
                quantity: "7".to_string(),
                side: Side::Bid as i32,
                state: OrderState::Confirmed as i32,
                ..Default::default()
            }
        }

        const WINDOW: Duration = Duration::from_millis(200);

        #[tokio::test]
        async fn a_failed_stream_is_an_error_not_an_empty_book() {
            let (tx, rx) = mpsc::channel(8);
            let handle = tokio::spawn(async move {
                tx.send(live(1)).await.unwrap();
                Err(eyre::eyre!("connect refused"))
            });
            let err = collect_book(rx, handle, WINDOW).await.unwrap_err();
            assert!(format!("{err:#}").contains("connect refused"), "{err:#}");
        }

        #[tokio::test]
        async fn a_task_that_ends_without_succeeding_is_an_error() {
            let (tx, rx) = mpsc::channel::<OrderbookEntry>(8);
            let handle = tokio::spawn(async move {
                let _tx = tx;
                std::future::pending::<Result<()>>().await
            });
            handle.abort();
            assert!(collect_book(rx, handle, WINDOW).await.is_err());
        }

        #[tokio::test]
        async fn a_stream_that_ends_cleanly_keeps_its_entries() {
            let (tx, rx) = mpsc::channel(8);
            let handle = tokio::spawn(async move {
                tx.send(live(1)).await.unwrap();
                tx.send(live(2)).await.unwrap();
                Ok(())
            });
            let book = collect_book(rx, handle, WINDOW).await.unwrap();
            assert_eq!(book.bids().len(), 2);
        }

        /// Wait (bounded) for the task holding `probe` to be dropped.
        async fn released(probe: &Arc<()>) -> bool {
            for _ in 0..100 {
                if Arc::strong_count(probe) == 1 {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            false
        }

        #[tokio::test]
        async fn a_live_stream_is_collected_then_aborted_at_the_deadline() {
            let (tx, rx) = mpsc::channel(8);
            let probe = Arc::new(());
            let held = probe.clone();
            let handle = tokio::spawn(async move {
                let _held = held;
                tx.send(live(3)).await.unwrap();
                std::future::pending::<Result<()>>().await
            });
            let book = collect_book(rx, handle, WINDOW).await.unwrap();
            assert_eq!(book.best_bid(), Some(300));
            assert!(released(&probe).await, "stream task still running");
        }

        #[tokio::test]
        async fn dropping_the_collection_aborts_the_stream() {
            let (tx, rx) = mpsc::channel::<OrderbookEntry>(8);
            let probe = Arc::new(());
            let held = probe.clone();
            let handle = tokio::spawn(async move {
                let _held = held;
                let _tx = tx;
                std::future::pending::<Result<()>>().await
            });
            let collecting = tokio::spawn(collect_book(rx, handle, Duration::from_secs(60)));
            tokio::time::sleep(Duration::from_millis(20)).await;
            collecting.abort();
            assert!(released(&probe).await, "stream task leaked");
        }
    }
}
