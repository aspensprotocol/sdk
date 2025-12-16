//! gRPC channel utilities for connecting to Aspens servers.
//!
//! This module provides helpers for creating gRPC channels that work with both
//! HTTP (local/development) and HTTPS (remote/production) endpoints.

use eyre::{Context, Result};
use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig};

/// Default timeout for gRPC operations (1 minute)
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Create a gRPC channel that automatically configures TLS for HTTPS URLs.
///
/// This function detects whether the URL uses `https://` and configures
/// TLS accordingly. For `http://` URLs, it connects without TLS.
///
/// The channel is configured with:
/// - 2 minute operation timeout
/// - 10 second connection timeout
/// - HTTP/2 keep-alive to prevent connection drops
///
/// # Arguments
/// * `url` - The gRPC server URL (e.g., "http://localhost:50051" or "<https://grpc.example.com:50051>")
///
/// # Returns
/// A configured `Channel` ready for use with gRPC clients.
///
/// # Example
/// ```ignore
/// use aspens::grpc::create_channel;
///
/// // Local development (no TLS)
/// let channel = create_channel("http://localhost:50051").await?;
///
/// // Remote production (with TLS)
/// let channel = create_channel("https://grpc.example.com:50051").await?;
/// ```
pub async fn create_channel(url: &str) -> Result<Channel> {
    let is_https = url.starts_with("https://");

    let endpoint = Channel::from_shared(url.to_string())
        .wrap_err_with(|| format!("Invalid gRPC URL: {}", url))?
        .timeout(DEFAULT_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        // HTTP/2 keep-alive settings to prevent "h2 protocol error" issues
        .http2_keep_alive_interval(Duration::from_secs(10))
        .keep_alive_timeout(Duration::from_secs(20))
        .keep_alive_while_idle(true);

    let endpoint = if is_https {
        // Configure TLS for HTTPS connections
        let tls_config = ClientTlsConfig::new().with_native_roots();
        endpoint
            .tls_config(tls_config)
            .wrap_err("Failed to configure TLS")?
    } else {
        endpoint
    };

    endpoint
        .connect()
        .await
        .wrap_err_with(|| format!("Failed to connect to gRPC server at {}", url))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_https_detection() {
        assert!("https://example.com:50051".starts_with("https://"));
        assert!("https://grpc.cvm-demo.aspens.xyz:50051".starts_with("https://"));
        assert!(!"http://localhost:50051".starts_with("https://"));
        assert!(!"http://127.0.0.1:50051".starts_with("https://"));
    }
}
