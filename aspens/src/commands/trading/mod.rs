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

/// Encode a prost message and sign the bytes with `wallet` the way a leg on
/// `architecture` is verified — the outer envelope signature the arborter
/// authenticates. Shared by the gRPC and FCE paths so the signed bytes are
/// byte-identical (the cross-repo parity invariant; see AGENTS.md). Order
/// entry / cancel both authenticate this way.
///
/// - EVM: EIP-191 over the encoded bytes (65 bytes, `r ‖ s ‖ v`).
/// - Solana: Ed25519 over the encoded bytes (64 bytes).
/// - Tron: the TRON message prefix over the lowercase hex text of the encoded
///   bytes (65 bytes). An EIP-191 signature is refused on a Tron leg, and the
///   other way round.
pub(crate) async fn sign_encoded<M: prost::Message>(
    msg: &M,
    wallet: &crate::Wallet,
    architecture: crate::wallet::Architecture,
) -> eyre::Result<Vec<u8>> {
    use crate::wallet::Architecture;
    let mut buf = Vec::new();
    msg.encode(&mut buf)?;
    match architecture {
        Architecture::Evm | Architecture::Solana => wallet.sign_message(&buf).await,
        #[cfg(feature = "tron")]
        Architecture::Tron => Ok(crate::tron::tron_sign_proto_message(wallet, &buf)
            .await?
            .to_vec()),
        #[cfg(not(feature = "tron"))]
        Architecture::Tron => Err(eyre::eyre!(
            "signing for a Tron leg needs the `tron` feature"
        )),
    }
}

/// Sign the withdraw request text (`network|token|account|amount`) the way a
/// withdraw on `architecture` is verified: EIP-191 (EVM) or Ed25519 (Solana)
/// over its bytes, or the TRON message prefix over the text as is (Tron).
pub(crate) async fn sign_withdraw_request(
    wallet: &crate::Wallet,
    architecture: crate::wallet::Architecture,
    text: &str,
) -> eyre::Result<Vec<u8>> {
    use crate::wallet::Architecture;
    match architecture {
        Architecture::Evm | Architecture::Solana => wallet.sign_message(text.as_bytes()).await,
        #[cfg(feature = "tron")]
        Architecture::Tron => Ok(crate::tron::tron_sign_text(wallet, text).await?.to_vec()),
        #[cfg(not(feature = "tron"))]
        Architecture::Tron => Err(eyre::eyre!(
            "signing for a Tron leg needs the `tron` feature"
        )),
    }
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

#[cfg(all(test, feature = "tron"))]
mod tron_signing_tests {
    use super::arborter_pb::OrderToCancel;
    use super::*;
    use crate::wallet::{Architecture, Wallet};
    use alloy::primitives::{Address, B256, Signature, keccak256};
    use prost::Message;

    fn tron_digest(text: &str) -> B256 {
        let mut pre = b"\x19TRON Signed Message:\n".to_vec();
        pre.extend_from_slice(text.len().to_string().as_bytes());
        pre.extend_from_slice(text.as_bytes());
        keccak256(pre)
    }

    fn eip191_digest(msg: &[u8]) -> B256 {
        let mut pre = b"\x19Ethereum Signed Message:\n".to_vec();
        pre.extend_from_slice(msg.len().to_string().as_bytes());
        pre.extend_from_slice(msg);
        keccak256(pre)
    }

    fn recovers(sig: &[u8], digest: B256) -> Option<Address> {
        Signature::from_raw(sig)
            .ok()?
            .recover_address_from_prehash(&digest)
            .ok()
    }

    /// One key, one message, both schemes. A Tron leg's signature recovers
    /// the wallet under the TRON prefix over the hex text and NOT under
    /// EIP-191; an EVM leg's is the other way round. The same holds for the
    /// withdraw request text, which is signed as is.
    #[tokio::test]
    async fn tron_leg_signs_with_tron_prefix() {
        let wallet = Wallet::from_evm_hex(
            "0xdfdf4b45dfa67d802fa6d8472e275b02ed0ca396194d65ae3d334fa1b6801d5c",
        )
        .unwrap();
        let me = wallet.as_evm().unwrap().address();
        let cancel = OrderToCancel {
            market_id: "tron-nile::0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee::base::0x01".into(),
            side: 2,
            order_id: vec![0xab; 32],
        };
        let encoded = cancel.encode_to_vec();
        let tron_text = hex::encode(&encoded);

        let tron = sign_encoded(&cancel, &wallet, Architecture::Tron)
            .await
            .unwrap();
        let evm = sign_encoded(&cancel, &wallet, Architecture::Evm)
            .await
            .unwrap();
        assert_eq!(tron.len(), 65);
        assert_eq!(recovers(&tron, tron_digest(&tron_text)), Some(me));
        assert_ne!(recovers(&tron, eip191_digest(&encoded)), Some(me));
        assert_eq!(recovers(&evm, eip191_digest(&encoded)), Some(me));
        assert_ne!(recovers(&evm, tron_digest(&tron_text)), Some(me));

        let text = "tron-nile|0xeca9bc828a3005b9a3b909f2cc5c2a54794de05f|\
                    0xa5411a1a5784b0828bbafcfa588c72ef3f3ce646|1000000";
        let tron = sign_withdraw_request(&wallet, Architecture::Tron, text)
            .await
            .unwrap();
        let evm = sign_withdraw_request(&wallet, Architecture::Evm, text)
            .await
            .unwrap();
        assert_eq!(recovers(&tron, tron_digest(text)), Some(me));
        assert_ne!(recovers(&tron, eip191_digest(text.as_bytes())), Some(me));
        assert_eq!(recovers(&evm, eip191_digest(text.as_bytes())), Some(me));
        assert_ne!(recovers(&evm, tron_digest(text)), Some(me));
    }
}
