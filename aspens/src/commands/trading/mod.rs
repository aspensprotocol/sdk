// The RPC-enabled MidribV3 + IERC20 sol! bindings now live in
// `aspens::evm::rpc` (gated on the `client` feature). Trading commands
// import them via `use crate::evm::rpc::{MidribV3, IERC20};`.

/// Query balances across chains (native gas, ERC-20 / SPL, locked / withdrawable).
pub mod balance;
/// Submit a `cancel_order` request and decode the gRPC response.
pub mod cancel_order;
/// Deposit tokens into the trading contract so they're available to trade.
pub mod deposit;
/// Build the gasless cross-chain order envelope used by `send_order`.
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
