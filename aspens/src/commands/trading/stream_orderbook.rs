/// Generated protobuf bindings for the `arborter.v1` trading service.
#[allow(missing_docs)]
pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{OrderState, OrderbookEntry, OrderbookRequest, Side};
use eyre::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::grpc::create_channel;

impl fmt::Display for OrderbookEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let side_str = match Side::try_from(self.side) {
            Ok(Side::Bid) => "BID",
            Ok(Side::Ask) => "ASK",
            _ => "UNKNOWN",
        };
        let state_str = match OrderState::try_from(self.state) {
            Ok(OrderState::Pending) => "PENDING",
            Ok(OrderState::Confirmed) => "CONFIRMED",
            Ok(OrderState::Matched) => "MATCHED",
            Ok(OrderState::Canceled) => "CANCELED",
            Ok(OrderState::Settled) => "SETTLED",
            _ => "UNKNOWN",
        };
        write!(
            f,
            "[{}] #{} {} {} @ {} (maker: {}) [{}]",
            self.timestamp,
            self.order_id,
            side_str,
            self.quantity,
            self.price,
            self.maker_base_address,
            state_str
        )
    }
}

/// Options for streaming the orderbook
#[derive(Debug, Clone, Default)]
pub struct StreamOrderbookOptions {
    /// The market ID to stream orders for
    pub market_id: String,
    /// If true, returns existing open orders when stream starts
    pub historical_open_orders: bool,
    /// If set, filter by a specific trader address
    pub filter_by_trader: Option<String>,
}

/// Stream orderbook entries from the server.
///
/// This function connects to the gRPC server and streams orderbook entries
/// as they arrive. Each entry is sent through the provided callback function.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `options` - Options for the stream (market_id, historical orders, trader filter)
/// * `callback` - A function to call for each orderbook entry
///
/// # Returns
/// This function runs until the stream is closed or an error occurs.
pub async fn stream_orderbook<F>(
    url: String,
    options: StreamOrderbookOptions,
    mut callback: F,
) -> Result<()>
where
    F: FnMut(OrderbookEntry),
{
    // Create a channel to connect to the gRPC server
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Create the request
    let request = OrderbookRequest {
        continue_stream: true,
        market_id: options.market_id,
        historical_open_orders: Some(options.historical_open_orders),
        filter_by_trader: options.filter_by_trader,
    };

    // Create a tonic request
    let request = tonic::Request::new(request);

    // Call the orderbook streaming endpoint
    let response = client.orderbook(request).await?;

    // Get the streaming response
    let mut stream = response.into_inner();

    // Process each entry from the stream
    while let Some(entry_result) = stream.next().await {
        match entry_result {
            Ok(entry) => {
                callback(entry);
            }
            Err(e) => {
                tracing::error!("Stream error: {}", e);
                return Err(e.into());
            }
        }
    }

    Ok(())
}

/// Snapshot of the top-of-book at a point in time, as raw u128 prices
/// in the market's pair_decimals scale (= the on-the-wire format the
/// matching engine reports in `OrderbookEntry.price`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TopOfBook {
    /// Highest bid price among Confirmed, non-zero-quantity orders.
    pub best_bid: Option<u128>,
    /// Lowest ask price among Confirmed, non-zero-quantity orders.
    pub best_ask: Option<u128>,
}

/// Snapshot the top-of-book for `market_id` by listening on the
/// orderbook stream for up to `collection_window`.
///
/// Filters to `OrderState::Confirmed` and non-zero `quantity` — only
/// those orders match against a new aggressor (per
/// `match_engine::order_book::process_order_list`). The matching
/// engine streams **all historical open orders first** when
/// `historical_open_orders` is set, so a short collection window is
/// usually enough to capture the resting book; live updates that
/// arrive after the deadline are ignored.
///
/// Designed for short interactive lookups (e.g. CLI `buy-marketable`
/// / `sell-marketable` helpers that need a slippage-cap reference
/// price). For long-running orderbook tracking, use the lower-level
/// [`stream_orderbook`] / [`stream_orderbook_channel`] directly.
pub async fn fetch_top_of_book(
    url: String,
    market_id: String,
    collection_window: std::time::Duration,
) -> Result<TopOfBook> {
    let (mut rx, _handle) = stream_orderbook_channel(
        url,
        StreamOrderbookOptions {
            market_id,
            historical_open_orders: true,
            filter_by_trader: None,
        },
    )
    .await?;

    let mut top = TopOfBook::default();
    let deadline = tokio::time::sleep(collection_window);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            maybe_entry = rx.recv() => {
                let Some(entry) = maybe_entry else { break };
                // Only Confirmed non-zero-qty orders match.
                if entry.state != OrderState::Confirmed as i32 {
                    continue;
                }
                let qty: u128 = match entry.quantity.parse() {
                    Ok(q) if q > 0 => q,
                    _ => continue,
                };
                let price: u128 = match entry.price.parse() {
                    Ok(p) if p > 0 => p,
                    _ => continue,
                };
                // qty filter only; price is what we track.
                let _ = qty;
                match Side::try_from(entry.side) {
                    Ok(Side::Bid) if top.best_bid.is_none_or(|b| price > b) => {
                        top.best_bid = Some(price);
                    }
                    Ok(Side::Ask) if top.best_ask.is_none_or(|a| price < a) => {
                        top.best_ask = Some(price);
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(top)
}

/// Apply a slippage cap to a raw pair-decimal price.
///
/// - `is_buy = true`: `reference * (10_000 + slippage_bps) / 10_000` — the
///   maximum the user is willing to pay above best ask.
/// - `is_buy = false`: `reference * (10_000 - slippage_bps) / 10_000` — the
///   minimum the user is willing to accept below best bid.
///
/// Takes a plain `bool` rather than the proto `Side` enum so callers
/// don't have to round-trip through a specific `arborter_pb::Side`
/// variant (every consumer module includes its own copy of the
/// generated proto types — see `pub mod arborter_pb` at the top of
/// each `trading/` module — and those variants don't unify even though
/// they share a wire format).
///
/// `slippage_bps` is clamped to `[0, 10_000]` so the sell-side
/// arithmetic can't underflow and the buy-side cap can't grow
/// unboundedly. A `0` slippage produces an order priced exactly at the
/// reference; the order may still rest if a faster client crosses it
/// first.
pub fn apply_slippage(reference_price: u128, slippage_bps: u32, is_buy: bool) -> Result<u128> {
    let bps = slippage_bps.min(10_000) as u128;
    let scale = if is_buy {
        10_000u128
            .checked_add(bps)
            .ok_or_else(|| eyre::eyre!("slippage scale overflow"))?
    } else {
        10_000u128
            .checked_sub(bps)
            .ok_or_else(|| eyre::eyre!("slippage scale underflow"))?
    };
    reference_price
        .checked_mul(scale)
        .ok_or_else(|| eyre::eyre!("slippage * price overflow"))
        .map(|v| v / 10_000)
}

/// Stream orderbook entries to a channel.
///
/// This is an alternative API that returns a receiver channel instead of using a callback.
/// Useful when you need to integrate with async code that prefers channels.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `options` - Options for the stream (market_id, historical orders, trader filter)
///
/// # Returns
/// A receiver channel that will receive orderbook entries, and a handle to the background task.
pub async fn stream_orderbook_channel(
    url: String,
    options: StreamOrderbookOptions,
) -> Result<(
    mpsc::Receiver<OrderbookEntry>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let (tx, rx) = mpsc::channel(100);

    let handle = tokio::spawn(async move {
        stream_orderbook(url, options, |entry| {
            // Try to send, ignore if receiver is dropped
            let _ = tx.blocking_send(entry);
        })
        .await
    });

    Ok((rx, handle))
}

/// Format an orderbook entry for CLI display
pub fn format_orderbook_entry(entry: &OrderbookEntry) -> String {
    let side_str = match Side::try_from(entry.side) {
        Ok(Side::Bid) => "BID ",
        Ok(Side::Ask) => "ASK ",
        _ => "??? ",
    };
    let state_str = match OrderState::try_from(entry.state) {
        Ok(OrderState::Pending) => "PENDING  ",
        Ok(OrderState::Confirmed) => "CONFIRMED",
        Ok(OrderState::Matched) => "MATCHED  ",
        Ok(OrderState::Canceled) => "CANCELED ",
        Ok(OrderState::Settled) => "SETTLED  ",
        _ => "UNKNOWN  ",
    };

    format!(
        "{} | ID: {:>8} | {} | Price: {:>12} | Qty: {:>12} | {} | Maker: {}",
        state_str,
        entry.order_id,
        side_str,
        entry.price,
        entry.quantity,
        format_timestamp(entry.timestamp),
        truncate_address(&entry.maker_base_address)
    )
}

/// Format a timestamp for display
fn format_timestamp(timestamp: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};

    let duration = Duration::from_millis(timestamp);
    let datetime = UNIX_EPOCH + duration;

    // Try to format as human-readable, fallback to raw timestamp
    match datetime.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;
            format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
        }
        Err(_) => format!("{}", timestamp),
    }
}

/// Truncate an address for display
fn truncate_address(address: &str) -> String {
    if address.len() > 12 {
        format!("{}...{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_address() {
        assert_eq!(
            truncate_address("0x1234567890abcdef1234567890abcdef12345678"),
            "0x1234...5678"
        );
        assert_eq!(truncate_address("short"), "short");
    }

    #[test]
    fn test_format_timestamp() {
        // Test that it doesn't panic
        let _ = format_timestamp(0);
        let _ = format_timestamp(1000000000000);
    }

    #[test]
    fn test_stream_orderbook_options_default() {
        let options = StreamOrderbookOptions::default();
        assert_eq!(options.market_id, "");
        assert!(!options.historical_open_orders);
        assert!(options.filter_by_trader.is_none());
    }

    #[test]
    fn apply_slippage_buy_adds_premium() {
        // 50 bps = 0.5%. 1_000_000 * 1.005 = 1_005_000.
        assert_eq!(apply_slippage(1_000_000, 50, true).unwrap(), 1_005_000);
    }

    #[test]
    fn apply_slippage_sell_subtracts_discount() {
        // 50 bps = 0.5%. 1_000_000 * 0.995 = 995_000.
        assert_eq!(apply_slippage(1_000_000, 50, false).unwrap(), 995_000);
    }

    #[test]
    fn apply_slippage_zero_is_no_op() {
        assert_eq!(apply_slippage(42_000, 0, true).unwrap(), 42_000);
        assert_eq!(apply_slippage(42_000, 0, false).unwrap(), 42_000);
    }

    #[test]
    fn apply_slippage_truncates_toward_zero() {
        // 1_001 * 1.0001 = 1001.1001 -> truncated to 1001.
        assert_eq!(apply_slippage(1_001, 1, true).unwrap(), 1_001);
        // 1_001 * 0.9999 = 1000.8999 -> truncated to 1000.
        assert_eq!(apply_slippage(1_001, 1, false).unwrap(), 1_000);
    }

    #[test]
    fn apply_slippage_clamps_above_10000_bps() {
        // 20_000 bps clamps to 10_000 bps (= 100%).
        // Buy: 100 * 2.0 = 200. Sell: 100 * 0.0 = 0.
        assert_eq!(apply_slippage(100, 20_000, true).unwrap(), 200);
        assert_eq!(apply_slippage(100, 20_000, false).unwrap(), 0);
    }

    #[test]
    fn apply_slippage_sell_at_10000_bps_is_zero() {
        // 100% sell slippage means accept any price down to zero.
        assert_eq!(apply_slippage(1_000_000, 10_000, false).unwrap(), 0);
    }

    #[test]
    fn apply_slippage_buy_overflow_rejected() {
        // u128::MAX * 1.0001 overflows the checked_mul step.
        assert!(apply_slippage(u128::MAX, 1, true).is_err());
    }
}
