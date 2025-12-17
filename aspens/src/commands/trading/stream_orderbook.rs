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
            self.timestamp, self.order_id, side_str, self.quantity, self.price, self.maker_base_address, state_str
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
pub async fn stream_orderbook<F>(url: String, options: StreamOrderbookOptions, mut callback: F) -> Result<()>
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
) -> Result<(mpsc::Receiver<OrderbookEntry>, tokio::task::JoinHandle<Result<()>>)> {
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
        format!("{}...{}", &address[..6], &address[address.len()-4..])
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_address() {
        assert_eq!(truncate_address("0x1234567890abcdef1234567890abcdef12345678"), "0x1234...5678");
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
}
