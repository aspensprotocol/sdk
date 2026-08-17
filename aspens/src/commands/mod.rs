//! gRPC command implementations.
//!
//! Each submodule wraps one of the arborter service surfaces and exposes
//! plain Rust functions that callers (CLI, REPL, admin tools) can invoke
//! without dealing with tonic clients or protobuf types directly.

/// Stack configuration: chain / token / market metadata fetches.
pub mod config;

/// Trading flows: balance, deposit, withdraw, send/cancel order, streams.
#[cfg(any(feature = "trader", feature = "admin"))]
pub mod trading;

/// Admin flows: chain / token / market / contract management.
#[cfg(feature = "admin")]
pub mod admin;

/// EIP-712 / Ed25519 admin authentication and JWT issuance.
#[cfg(feature = "admin")]
pub mod auth;

/// Operator-authority on-chain actions signed DIRECTLY by an offline operator
/// key — never through the arborter. Not gated on `trader`/`admin`: those
/// select gRPC surfaces, and nothing here talks to the arborter beyond reading
/// its published config.
#[cfg(all(feature = "client", feature = "solana"))]
pub mod operator;
