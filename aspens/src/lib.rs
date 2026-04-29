//! Aspens crosschain trading SDK.
//!
//! This crate provides the core building blocks for interacting with the
//! Aspens Market Stack: a chain-agnostic [`Wallet`] abstraction, a
//! [`ChainClient`] RPC dispatcher (EVM via Alloy, Solana via
//! `solana-client`), the [`AspensClient`] gRPC entry point, and the
//! signing helpers in [`evm`], [`solana`], and [`orders`] that produce
//! the exact bytes the arborter validates.
//!
//! # Feature flags
//!
//! - **`evm`** (default) — stateless EVM signing helpers in [`evm`] and
//!   the EIP-712 bindings under [`evm`]. Pulls Alloy primitives only.
//! - **`solana`** (default) — stateless Solana helpers in [`solana`]
//!   (PDA derivations, instruction builders, borsh payload encoder).
//!   Pulls `solana-sdk`, `bs58`, `ed25519-dalek`, and `borsh`.
//! - **`client`** (default) — full gRPC + RPC runtime: [`AspensClient`],
//!   the [`commands`] modules, [`chain_client`], the [`executor`]
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
#[cfg(feature = "evm")]
pub mod evm;
/// Async/sync execution strategies used by binaries to drive the client.
#[cfg(feature = "client")]
pub mod executor;
#[cfg(feature = "client")]
pub mod grpc;
/// gRPC health-check helpers used to probe stack readiness.
#[cfg(feature = "client")]
pub mod health;
pub mod orders;
#[cfg(feature = "solana")]
pub mod solana;
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

/// Generated protobuf bindings for the arborter config and auth services.
#[cfg(feature = "client")]
pub mod proto {
    /// Arborter config service protobuf bindings.
    #[allow(missing_docs)]
    pub mod config {
        include!("../proto/generated/xyz.aspens.arborter_config.v1.rs");
    }
    /// Arborter auth service protobuf bindings (admin feature only).
    #[cfg(feature = "admin")]
    #[allow(missing_docs)]
    pub mod auth {
        include!("../proto/generated/xyz.aspens.arborter_auth.v1.rs");
    }
}

// Re-export commonly used types
#[cfg(feature = "client")]
pub use chain_client::ChainClient;
#[cfg(feature = "client")]
pub use client::{AspensClient, AspensClientBuilder, JwtToken};
#[cfg(feature = "client")]
pub use executor::{AsyncExecutor, BlockingExecutor, DirectExecutor};
pub use wallet::{
    chain_curve, load_admin_wallet, load_trader_wallet, load_trader_wallet_for_chain,
    load_trader_wallet_for_network, CurveType, Wallet,
};

// Re-export admin types when admin feature is enabled
#[cfg(all(feature = "admin", feature = "client"))]
pub use commands::admin;
#[cfg(all(feature = "admin", feature = "client"))]
pub use commands::auth;
