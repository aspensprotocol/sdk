pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Trade, TradeRequest, TradeRole};
use eyre::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::grpc::create_channel;

impl fmt::Display for Trade {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let buyer_str = match TradeRole::try_from(self.buyer_is) {
            Ok(TradeRole::Maker) => "MAKER",
            Ok(TradeRole::Taker) => "TAKER",
            _ => "UNKNOWN",
        };
        let seller_str = match TradeRole::try_from(self.seller_is) {
            Ok(TradeRole::Maker) => "MAKER",
            Ok(TradeRole::Taker) => "TAKER",
            _ => "UNKNOWN",
        };
        write!(
            f,
            "[{}] {} @ {} (buyer: {}, seller: {}) order_hit: #{}",
            self.timestamp, self.qty, self.price, buyer_str, seller_str, self.order_hit
        )
    }
}

/// Options for streaming trades
#[derive(Debug, Clone, Default)]
pub struct StreamTradesOptions {
    /// The market ID to stream trades for
    pub market_id: String,
    /// If true, returns historical closed trades when stream starts
    pub historical_closed_trades: bool,
    /// If set, filter by a specific trader address
    pub filter_by_trader: Option<String>,
}

/// Stream trades from the server.
///
/// This function connects to the gRPC server and streams trades
/// as they arrive. Each trade is sent through the provided callback function.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `options` - Options for the stream (market_id, historical trades, trader filter)
/// * `callback` - A function to call for each trade
///
/// # Returns
/// This function runs until the stream is closed or an error occurs.
pub async fn stream_trades<F>(
    url: String,
    options: StreamTradesOptions,
    mut callback: F,
) -> Result<()>
where
    F: FnMut(Trade),
{
    // Create a channel to connect to the gRPC server
    let channel = create_channel(&url).await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Create the request
    let request = TradeRequest {
        continue_stream: true,
        market_id: options.market_id,
        historical_closed_trades: Some(options.historical_closed_trades),
        filter_by_trader: options.filter_by_trader,
    };

    // Create a tonic request
    let request = tonic::Request::new(request);

    // Call the trades streaming endpoint
    let response = client.trades(request).await?;

    // Get the streaming response
    let mut stream = response.into_inner();

    // Process each trade from the stream
    while let Some(trade_result) = stream.next().await {
        match trade_result {
            Ok(trade) => {
                callback(trade);
            }
            Err(e) => {
                tracing::error!("Stream error: {}", e);
                return Err(e.into());
            }
        }
    }

    Ok(())
}

/// Stream trades to a channel.
///
/// This is an alternative API that returns a receiver channel instead of using a callback.
/// Useful when you need to integrate with async code that prefers channels.
///
/// # Arguments
/// * `url` - The Aspens Market Stack URL
/// * `options` - Options for the stream (market_id, historical trades, trader filter)
///
/// # Returns
/// A receiver channel that will receive trades, and a handle to the background task.
pub async fn stream_trades_channel(
    url: String,
    options: StreamTradesOptions,
) -> Result<(mpsc::Receiver<Trade>, tokio::task::JoinHandle<Result<()>>)> {
    let (tx, rx) = mpsc::channel(100);

    let handle = tokio::spawn(async move {
        stream_trades(url, options, |trade| {
            // Try to send, ignore if receiver is dropped
            let _ = tx.blocking_send(trade);
        })
        .await
    });

    Ok((rx, handle))
}

/// Format a trade for CLI display
pub fn format_trade(trade: &Trade) -> String {
    let buyer_str = match TradeRole::try_from(trade.buyer_is) {
        Ok(TradeRole::Maker) => "MAKER",
        Ok(TradeRole::Taker) => "TAKER",
        _ => "???  ",
    };
    let seller_str = match TradeRole::try_from(trade.seller_is) {
        Ok(TradeRole::Maker) => "MAKER",
        Ok(TradeRole::Taker) => "TAKER",
        _ => "???  ",
    };

    format!(
        "{} | Price: {:>12} | Qty: {:>12} | Buyer: {} | Seller: {} | Order: {:>8} | Maker: {} ↔ Taker: {}",
        format_timestamp(trade.timestamp),
        trade.price,
        trade.qty,
        buyer_str,
        seller_str,
        trade.order_hit,
        truncate_address(&trade.maker_base_address),
        truncate_address(&trade.taker_base_address)
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
    fn test_stream_trades_options_default() {
        let options = StreamTradesOptions::default();
        assert_eq!(options.market_id, "");
        assert!(!options.historical_closed_trades);
        assert!(options.filter_by_trader.is_none());
    }
}
