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
