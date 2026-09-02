//! `Display` impls and CLI-formatting helpers for the proto-generated
//! `Order` and `SendOrderResponse` types.
//!
//! These can't live in the generated proto module (it's overwritten on
//! every `cargo build`), and they don't depend on any of the call /
//! signing logic in `mod.rs`, so they're parked here in a focused
//! submodule.

use std::fmt;

use crate::commands::trading::arborter_pb::{Order, SendOrderResponse};

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

impl SendOrderResponse {
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
            "SendOrderResponse {{\n  order_id: 0x{},\n  order_in_book: {},\n  order: {},\n  trades: [{}]\n}}",
            hex::encode(&self.order_id),
            self.order_in_book,
            self.order
                .as_ref()
                .map_or("None".to_string(), |o| format!("{}", o)),
            self.trades
                .iter()
                .map(|t| format!("{:?}", t))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
