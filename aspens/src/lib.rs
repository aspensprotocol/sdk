//! Aspens crosschain trading SDK.
//!
//! This crate provides the core building blocks for interacting with the
//! Aspens Market Stack: a chain-agnostic [`Wallet`] abstraction, a
//  NOTE: `ChainClient` / `AspensClient` appear below as code spans, not
//  intra-doc links. Both are behind `feature = "client"`, and these are the
//  CRATE-level docs, which are compiled under every feature combination —
//  including the lean `--no-default-features --features evm,solana` build this
//  very section describes. A link here is a hard error under `-D warnings`
//  exactly when someone documents the lean build.
//! `ChainClient` RPC dispatcher (EVM via Alloy, Solana via
//! `solana-client`), the `AspensClient` gRPC entry point, and the
//! signing helpers in [`evm`], [`solana`], and [`orders`] that produce
//! the exact bytes the arborter validates.
//!
//! # Feature flags
//!
//! - **`evm`** (default) — stateless EVM signing helpers in [`evm`] and
//!   the EIP-712 bindings under [`evm`]. Pulls Alloy primitives only.
//! - **`solana`** (default) — stateless Solana helpers in [`solana`]
//!   (PDA derivations, instruction builders, borsh payload encoder).
//!   Pulls `solana-sdk`, `bs58`, and `borsh`.
//! - **`client`** (default) — full gRPC + RPC runtime: `AspensClient`,
//!   the `commands` modules, `chain_client`, the `executor`
//!   abstraction, and Solana RPC submission. Pulls `tonic`, `prost`,
//!   `tokio`, `solana-client`, and the proto-generated bindings.
//!
//! Lean signing consumers (browser, embedded, etc.) can build with
//! `--no-default-features --features evm,solana` to skip all of tonic /
//! prost / tokio / solana-client.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "client")]
pub mod chain_client;
/// gRPC client and builder for the Aspens Market Stack.
#[cfg(feature = "client")]
pub mod client;
/// gRPC command implementations (config, trading, admin, auth).
#[cfg(feature = "client")]
pub mod commands;
/// Decimal-string ↔ base-units conversion shared by all amount-parsing
/// call sites (CLI, REPL, library).
pub mod decimals;
#[cfg(feature = "evm")]
pub mod evm;

/// Async/sync execution strategies used by binaries to drive the client.
#[cfg(feature = "client")]
pub mod executor;
/// FCE direct-action transport (Flare Confidential Extension proxy). Behind the
/// `fce` feature; see `sdk/docs/fce-transport-design.md`.
#[cfg(feature = "fce")]
pub mod fce;
// Internal — gRPC channel construction helpers shared by the commands
// modules. Not part of the stable public API; may change without notice.
#[cfg(feature = "client")]
#[doc(hidden)]
pub mod grpc;
/// gRPC health-check helpers used to probe stack readiness.
#[cfg(feature = "client")]
pub mod health;
pub mod orders;
#[cfg(feature = "solana")]
pub mod solana;
/// Relying-party TDX attestation verification (REPORTDATA/manifest reconstruction
/// + the verify pipeline). Pure `sha2`; the DCAP backend is a separate phase.
pub mod tdx_verify;
pub mod wallet;

/// Generated protobuf bindings for the attestation service.
#[cfg(feature = "client")]
pub mod attestation {
    /// Attestation service protobuf bindings, version 1.
    #[allow(missing_docs)]
    pub mod v1 {
        include!("../proto/generated/xyz.aspens.attestation.v1.rs");
    }
}

// The `arborter_config.v1` and `arborter_auth.v1` bindings are NOT included
// here. Each generated file is compiled exactly once, next to the command
// module that wraps its service — `commands::config::config_pb` and
// `commands::auth::auth_pb`. A second `include!` would not alias those types,
// it would mint a parallel set that shares the wire format and nothing else,
// so a `GetConfigResponse` from `AspensClient` would refuse to typecheck
// against the one spelled here. `attestation::v1` above is the sole crate-root
// binding, because `build.rs` rewrites the generated cross-package references
// in `arborter_config.v1` to that absolute path.

// Re-export commonly used types
#[cfg(feature = "client")]
pub use chain_client::ChainClient;
#[cfg(feature = "client")]
pub use client::{AspensClient, AspensClientBuilder, JwtToken};
#[cfg(feature = "client")]
pub use executor::{AsyncExecutor, BlockingExecutor, DirectExecutor};
pub use wallet::{CurveType, Wallet, load_admin_wallet, load_trader_wallet};

// Chain-aware wallet helpers depend on the proto-generated `Chain` /
// `GetConfigResponse` types under `commands::config`, which only exist
// when the `client` feature is enabled.
#[cfg(feature = "client")]
pub use wallet::{chain_curve, load_trader_wallet_for_chain, load_trader_wallet_for_network};

// Operator-authority direct-signing flows (Solana). Deliberately not behind
// `admin`: `commands::admin` is the gRPC surface where the ARBORTER signs, and
// these exist precisely because that shape can't secure a control aimed at the
// arborter's own key.
#[cfg(all(feature = "client", feature = "solana"))]
pub use commands::operator;
#[cfg(feature = "solana")]
pub use wallet::{OPERATOR_ADMIN_PRIVKEY_SOLANA_ENV, load_operator_admin_wallet_solana};

// Re-export admin types when admin feature is enabled
#[cfg(all(feature = "admin", feature = "client"))]
pub use commands::admin;
#[cfg(all(feature = "admin", feature = "client"))]
pub use commands::auth;
