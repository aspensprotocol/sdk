// The RPC-enabled MidribV3 + IERC20 sol! bindings now live in
// `aspens::evm::rpc` (gated on the `client` feature). Trading commands
// import them via `use crate::evm::rpc::{MidribV3, IERC20};`.

/// Generated protobuf bindings for the `arborter.v1` trading service.
///
/// This is the **only** place `xyz.aspens.arborter.v1.rs` is compiled. Every
/// `trading/` submodule imports from here rather than `include!`ing its own
/// copy: an `include!` is a textual expansion, so five copies were five sets
/// of distinct, mutually incompatible types with one wire format — the
/// `Order` you signed in `send_order` would not typecheck against the `Order`
/// spelled anywhere else. The bindings live next to the commands that wrap
/// the service, matching [`crate::commands::config::config_pb`] and
/// `commands::auth::auth_pb`. (`attestation.v1` is the one exception,
/// anchored at [`crate::attestation::v1`] because `build.rs` rewrites the
/// generated cross-package references to that absolute path.)
#[allow(missing_docs)]
pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

/// Query balances across chains (native gas, ERC-20 / SPL, locked / withdrawable).
pub mod balance;
/// Submit a `cancel_order` request and decode the gRPC response.
pub mod cancel_order;
/// Deposit tokens into the trading contract so they're available to trade.
pub mod deposit;
/// Resolve an order's budget and derive its canonical id — the client's own
/// copy of the id the arborter derives from the signed order. Nothing here
/// goes on the wire. (Despite the name, nothing here signs or locks.)
pub mod gasless;
/// Build, sign, and submit a buy/sell order envelope.
pub mod send_order;
/// Subscribe to the orderbook stream for a given market.
pub mod stream_orderbook;
/// Subscribe to the trades stream for a given market.
pub mod stream_trades;
/// Withdraw tokens from the trading contract back to the user's wallet.
pub mod withdraw;

/// FCE direct-action routing: builds the same signed envelopes as the gRPC
/// commands and submits them through the ext-proxy transport. Only compiled
/// when both `client` and `fce` are on.
#[cfg(feature = "fce")]
pub mod fce_actions;

/// Encode a prost message and sign the bytes with `wallet` — the outer
/// envelope signature the arborter authenticates. Shared by the gRPC and FCE
/// paths so the signed bytes are byte-identical (the cross-repo parity
/// invariant; see CLAUDE.md). Order entry / cancel both authenticate this way.
pub(crate) async fn sign_encoded<M: prost::Message>(
    msg: &M,
    wallet: &crate::Wallet,
) -> eyre::Result<Vec<u8>> {
    let mut buf = Vec::new();
    msg.encode(&mut buf)?;
    wallet.sign_message(&buf).await
}

/// Render a millisecond Unix timestamp as `HH:MM:SS` (UTC), falling back to
/// the raw number if it cannot be interpreted.
///
/// Display only — no signed value is derived from it. Shared by
/// [`stream_trades`] and [`stream_orderbook`], which each carried a
/// byte-identical private copy; they format the same `timestamp` field off the
/// same stream, so two copies could only ever drift apart, never usefully
/// differ.
pub(crate) fn format_timestamp(timestamp: u64) -> String {
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

/// Abbreviate an address for column display: first 6 and last 4 characters.
/// Anything 12 characters or shorter is returned unchanged.
///
/// Display only — never fed back into a market id, an order, or a hash. Same
/// deduplication note as [`format_timestamp`].
pub(crate) fn truncate_address(address: &str) -> String {
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
    fn truncate_address_keeps_head_and_tail() {
        assert_eq!(
            truncate_address("0x1234567890abcdef1234567890abcdef12345678"),
            "0x1234...5678"
        );
        assert_eq!(truncate_address("short"), "short");
    }

    #[test]
    fn format_timestamp_renders_hms_and_never_panics() {
        assert_eq!(format_timestamp(0), "00:00:00");
        // 1 h 2 min 3 s past midnight, in millis.
        assert_eq!(format_timestamp((3600 + 120 + 3) * 1000), "01:02:03");
        // A realistic epoch-millis value: the clock wraps at 24 h, so only the
        // time-of-day survives — pin that rather than asserting no panic.
        assert_eq!(format_timestamp(1_000_000_000_000), "01:46:40");
    }
}
