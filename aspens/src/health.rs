use std::time::{Duration, Instant};
use tonic::transport::{Channel, ClientTlsConfig};

/// Result of a ping attempt to the gRPC server
#[derive(Debug)]
pub struct PingResult {
    /// Whether the connection was successful
    pub success: bool,
    /// The URL that was pinged
    pub url: String,
    /// Round-trip time in milliseconds (if successful)
    pub latency_ms: Option<u64>,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Ping the gRPC server by attempting to establish a connection
///
/// This performs a lightweight connection attempt to verify the server is reachable.
/// Uses a configurable timeout (default 5 seconds).
pub async fn ping_grpc_server(url: String) -> PingResult {
    ping_grpc_server_with_timeout(url, Duration::from_secs(5)).await
}

/// Ping the gRPC server with a custom timeout
pub async fn ping_grpc_server_with_timeout(url: String, timeout: Duration) -> PingResult {
    let start = Instant::now();
    let is_https = url.starts_with("https://");

    let endpoint = match Channel::from_shared(url.clone()) {
        Ok(ep) => ep.connect_timeout(timeout).timeout(timeout),
        Err(e) => {
            return PingResult {
                success: false,
                url,
                latency_ms: None,
                error: Some(format!("Invalid URL: {}", e)),
            };
        }
    };

    // Configure TLS for HTTPS connections
    let endpoint = if is_https {
        let tls_config = ClientTlsConfig::new().with_native_roots();
        match endpoint.tls_config(tls_config) {
            Ok(ep) => ep,
            Err(e) => {
                return PingResult {
                    success: false,
                    url,
                    latency_ms: None,
                    error: Some(format!("TLS configuration error: {}", e)),
                };
            }
        }
    } else {
        endpoint
    };

    match endpoint.connect().await {
        Ok(_channel) => {
            let latency = start.elapsed().as_millis() as u64;
            PingResult {
                success: true,
                url,
                latency_ms: Some(latency),
                error: None,
            }
        }
        Err(e) => PingResult {
            success: false,
            url,
            latency_ms: None,
            error: Some(e.to_string()),
        },
    }
}
