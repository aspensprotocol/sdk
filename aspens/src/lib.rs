#[cfg(feature = "client")]
pub mod chain_client;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod commands;
#[cfg(feature = "evm")]
pub mod evm;
#[cfg(feature = "client")]
pub mod executor;
#[cfg(feature = "client")]
pub mod grpc;
#[cfg(feature = "client")]
pub mod health;
pub mod orders;
#[cfg(feature = "solana")]
pub mod solana;
pub mod wallet;

#[cfg(feature = "client")]
pub mod attestation {
    pub mod v1 {
        include!("../proto/generated/xyz.aspens.attestation.v1.rs");
    }
}

#[cfg(feature = "client")]
pub mod proto {
    pub mod config {
        include!("../proto/generated/xyz.aspens.arborter_config.v1.rs");
    }
    #[cfg(feature = "admin")]
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
