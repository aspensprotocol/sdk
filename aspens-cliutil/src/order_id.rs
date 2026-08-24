//! Parsing for the CLI/REPL order-id arguments: `--match-order-id` and the
//! `cancel-order` positional both take a `0x`-prefixed 64-hex-character
//! string — the CLI/REPL's rendering of the arborter's server-derived
//! 32-byte order handle (`SendOrderResponse.order_id`,
//! `OrderToCancel.order_id`, `Order.matching_order_ids`). The id is never
//! chosen by the caller — only echoed back to it by the server and pasted
//! back in here — so parsing is entirely about accepting what a person
//! copies and rejecting the wrong shape before it ever reaches a wallet
//! signature.

use eyre::Result;

/// Parse a `0x` + 64-hex-character order id for `--{label}` (or a bare
/// positional, using `label` to name it in the error).
///
/// Mirrors the arborter's own `INVALID_ARGUMENT` wording for a malformed
/// wire id (`order_id must be exactly 32 bytes, got N`), so a value
/// rejected here reads the same as one the server would have rejected.
pub fn parse_order_id(label: &str, s: &str) -> Result<[u8; 32]> {
    let body = s.strip_prefix("0x").unwrap_or(s);
    let bytes =
        hex::decode(body).map_err(|e| eyre::eyre!("invalid {label} '{s}': not hex ({e})"))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        eyre::eyre!(
            "{label} must be exactly 32 bytes, got {} (from '{s}')",
            v.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire's own 0x + 64-hex shape round-trips.
    #[test]
    fn parses_a_well_formed_id() {
        let hex_id = format!("0x{}", "ab".repeat(32));
        let parsed = parse_order_id("order_id", &hex_id).expect("well-formed id parses");
        assert_eq!(parsed, [0xab; 32]);
    }

    /// The `0x` prefix is optional.
    #[test]
    fn accepts_a_prefix_free_id() {
        let hex_id = "cd".repeat(32);
        let parsed = parse_order_id("order_id", &hex_id).expect("prefix-free id parses");
        assert_eq!(parsed, [0xcd; 32]);
    }

    /// Too short must be rejected with the arborter's own wording, not a
    /// silently truncated/zero-padded value.
    #[test]
    fn rejects_a_short_id() {
        let err = parse_order_id("order_id", "0xabcd").unwrap_err();
        assert!(
            err.to_string().contains("must be exactly 32 bytes, got 2"),
            "got: {err}"
        );
    }

    /// Too long must also be rejected, not silently truncated.
    #[test]
    fn rejects_a_long_id() {
        let hex_id = format!("0x{}", "11".repeat(33));
        let err = parse_order_id("order_id", &hex_id).unwrap_err();
        assert!(
            err.to_string().contains("must be exactly 32 bytes, got 33"),
            "got: {err}"
        );
    }

    /// Non-hex input is a distinct failure from wrong-length input.
    #[test]
    fn rejects_non_hex_input() {
        let err = parse_order_id("order_id", "0xzz").unwrap_err();
        assert!(err.to_string().contains("not hex"), "got: {err}");
    }

    /// The label names the argument in the error, so a `--match-order-id`
    /// failure doesn't read like a `cancel-order` positional failure.
    #[test]
    fn the_label_names_the_argument_in_the_error() {
        let err = parse_order_id("match-order-id", "0xab").unwrap_err();
        assert!(err.to_string().contains("match-order-id"), "got: {err}");
    }
}
