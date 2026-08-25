//! `Display` impls and CLI-formatting helpers for the proto-generated
//! `Order`, `TransactionHash`, and `SendOrderResponse` types.
//!
//! These can't live in the generated proto module (it's overwritten on
//! every `cargo build`), and they don't depend on any of the call /
//! signing logic in `mod.rs`, so they're parked here in a focused
//! submodule.

use std::fmt;

use crate::commands::trading::arborter_pb::{Order, SendOrderResponse, TransactionHash};

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

    /// One line saying where each leg of this order settles — the
    /// acknowledgement surface for the two per-chain account addresses.
    ///
    /// Reads the venue's byte-verbatim echo of the SIGNED order, so it
    /// reports what was actually committed to, not what the caller
    /// intended. When both legs are hex and equal ignoring case (one EVM
    /// wallet serving an EVM/EVM market — the silent default), it says so
    /// out loud. `None` when the response carries no order echo.
    pub fn settlement_summary(&self) -> Option<String> {
        let order = self.order.as_ref()?;
        let parts: Vec<&str> = order.market_id.split("::").collect();
        let (base_net, quote_net) = match parts.as_slice() {
            [b, _, q, _] => (*b, *q),
            _ => ("base", "quote"),
        };
        let b = &order.base_account_address;
        let q = &order.quote_account_address;
        let same_note = if b.starts_with("0x") && q.starts_with("0x") && b.eq_ignore_ascii_case(q) {
            " — the SAME address on both chains"
        } else {
            ""
        };
        Some(format!(
            "Settlement: base leg → {b} on {base_net}; quote leg → {q} on {quote_net}{same_note}"
        ))
    }
}

impl fmt::Display for SendOrderResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SendOrderResponse {{\n  order_id: 0x{},\n  order_in_book: {},\n  order: {},\n  trades: [{}],\n  transaction_hashes: [{}]\n}}",
            hex::encode(&self.order_id),
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
