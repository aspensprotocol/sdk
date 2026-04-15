pub mod chain_client;
pub mod client;
pub mod commands;
pub mod executor;
pub mod grpc;
pub mod health;
#[cfg(feature = "solana")]
pub mod solana;
pub mod wallet;

pub mod attestation {
    pub mod v1 {
        include!("../proto/generated/xyz.aspens.attestation.v1.rs");
    }
}

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
pub use chain_client::ChainClient;
pub use client::{AspensClient, AspensClientBuilder, JwtToken};
pub use executor::{AsyncExecutor, BlockingExecutor, DirectExecutor};
pub use wallet::{load_admin_wallet, load_trader_wallet, CurveType, Wallet};

// Re-export admin types when admin feature is enabled
#[cfg(feature = "admin")]
pub use commands::admin;
#[cfg(feature = "admin")]
pub use commands::auth;
