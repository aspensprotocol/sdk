//! `Display` impls and CLI-formatting helpers for the proto-generated
//! `Order`, `TransactionHash`, and `SendOrderResponse` types.
//!
//! These can't live in the generated proto module (it's overwritten on
//! every `cargo build`), and they don't depend on any of the call /
//! signing logic in `mod.rs`, so they're parked here in a focused
//! submodule.

use std::fmt;

use super::arborter_pb::{Order, SendOrderResponse, TransactionHash};

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Order {{\n  side: {},\n  quantity: {},\n  price: {},\n  market_id: {},\n  base_account_address: {},\n  quote_account_address: {},\n  execution_type: {},\n  matching_order_ids: {:?}\n}}",
            self.side,
            self.quantity,
            self.price
                .clone()
                .map_or("None".to_string(), |p| p.to_string()),
            self.market_id,
            self.base_account_address,
            self.quote_account_address,
            self.execution_type,
            self.matching_order_ids
        )
    }
}

/// Transaction hash information for blockchain transactions
///
/// This struct contains information about transaction hashes that are generated
/// when orders are processed on the blockchain. Each transaction hash includes
/// a type (e.g., "deposit", "settlement", "withdrawal") and the actual hash value.
impl fmt::Display for TransactionHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TransactionHash {{ hash_type: {}, hash_value: {} }}",
            self.hash_type, self.hash_value
        )
    }
}

impl TransactionHash {
    /// Format transaction hash for CLI display
    ///
    /// Returns a user-friendly string representation of the transaction hash
    /// in the format "type: hash_value"
    pub fn format_for_cli(&self) -> String {
        format!("[{}] {}", self.hash_type.to_uppercase(), self.hash_value)
    }

    /// Get block explorer URL hints based on common chains
    ///
    /// Returns a suggested block explorer base URL for common chains
    pub fn get_explorer_hint(&self) -> Option<String> {
        // This is a simple implementation - could be enhanced with actual chain detection
        Some(
            "Paste this hash into your chain's block explorer (e.g., Etherscan, Basescan)"
                .to_string(),
        )
    }
}

impl SendOrderResponse {
    /// Get formatted transaction hashes for CLI display
    ///
    /// Returns a vector of formatted transaction hash strings that can be
    /// easily displayed in the CLI or REPL interface
    pub fn get_formatted_transaction_hashes(&self) -> Vec<String> {
        self.transaction_hashes
            .iter()
            .map(|th| th.format_for_cli())
            .collect()
    }
}

impl fmt::Display for SendOrderResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SendOrderResponse {{\n  order_id: {},\n  order_in_book: {},\n  order: {},\n  trades: [{}],\n  transaction_hashes: [{}]\n}}",
            self.order_id,
            self.order_in_book,
            self.order
                .as_ref()
                .map_or("None".to_string(), |o| format!("{}", o)),
            self.trades
                .iter()
                .map(|t| format!("{:?}", t))
                .collect::<Vec<_>>()
                .join(", "),
            self.transaction_hashes
                .iter()
                .map(|th| format!("{}: {}", th.hash_type, th.hash_value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
