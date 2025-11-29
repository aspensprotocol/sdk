pub mod client;
pub mod commands;
pub mod executor;
pub mod health;

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
pub use client::{AspensClient, AspensClientBuilder, JwtToken};
pub use executor::{AsyncExecutor, BlockingExecutor, DirectExecutor};

// Re-export admin types when admin feature is enabled
#[cfg(feature = "admin")]
pub use commands::admin;
#[cfg(feature = "admin")]
pub use commands::auth;
