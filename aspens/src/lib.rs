pub mod client;
pub mod commands;
pub mod executor;

pub mod proto {
    include!("../proto/generated/xyz.aspens.arborter_config.v1.rs");
}

// Re-export commonly used types
pub use client::{AspensClient, AspensClientBuilder};
pub use executor::{AsyncExecutor, BlockingExecutor, DirectExecutor};
