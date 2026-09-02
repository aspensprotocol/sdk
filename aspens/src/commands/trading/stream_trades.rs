use std::fmt;

use super::arborter_pb::arborter_service_client::ArborterServiceClient;
use super::arborter_pb::{Trade, TradeRequest, TradeRole};
use eyre::Result;
use futures::StreamExt;

use super::{format_timestamp, truncate_address};
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
            "[{}] {} @ {} (buyer: {}, seller: {}) order_hit: #0x{}",
            self.timestamp,
            self.qty,
            self.price,
            buyer_str,
            seller_str,
            hex::encode(&self.order_hit)
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

    // As in `format_orderbook_entry`: the id is now a full 32-byte handle,
    // so it's truncated for the column rather than right-aligned as a number.
    let order_hit_hex = truncate_address(&format!("0x{}", hex::encode(&trade.order_hit)));
    format!(
        "{} | Price: {:>12} | Qty: {:>12} | Buyer: {} | Seller: {} | Order: {} | Maker: {} ↔ Taker: {}",
        format_timestamp(trade.timestamp),
        trade.price,
        trade.qty,
        buyer_str,
        seller_str,
        order_hit_hex,
        truncate_address(&trade.maker_base_address),
        truncate_address(&trade.taker_base_address)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_trades_options_default() {
        let options = StreamTradesOptions::default();
        assert_eq!(options.market_id, "");
        assert!(!options.historical_closed_trades);
        assert!(options.filter_by_trader.is_none());
    }
}
