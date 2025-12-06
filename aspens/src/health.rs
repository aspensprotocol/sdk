use eyre::{Context, Result};
use std::time::{Duration, Instant};
use tonic::transport::{Channel, ClientTlsConfig};
use tonic_reflection::pb::v1::{
    server_reflection_client::ServerReflectionClient, server_reflection_request::MessageRequest,
    server_reflection_response::MessageResponse, ServerReflectionRequest,
};
use tracing::info;

use crate::grpc::create_channel;

/// Check if the gRPC server is accessible by attempting to list services via reflection
pub async fn check_grpc_server(url: String) -> Result<Vec<String>> {
    info!("Connecting to gRPC server at {}", url);

    // Create a channel to connect to the gRPC server
    let channel = create_channel(&url).await?;

    // Use the reflection client to list services
    let mut reflection_client = ServerReflectionClient::new(channel);

    // Create a request to list all services
    let request = ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    };

    // Send the request and get the response stream
    let response = reflection_client
        .server_reflection_info(tokio_stream::iter(vec![request]))
        .await
        .wrap_err("Failed to call gRPC reflection service")?;

    let mut stream = response.into_inner();
    let mut services = Vec::new();

    // Read from the response stream
    while let Some(response) = stream.message().await? {
        if let Some(message_response) = response.message_response {
            match message_response {
                MessageResponse::ListServicesResponse(list) => {
                    for service in list.service {
                        services.push(service.name);
                    }
                }
                MessageResponse::ErrorResponse(err) => {
                    return Err(eyre::eyre!(
                        "gRPC reflection error: {} (code: {})",
                        err.error_message,
                        err.error_code
                    ));
                }
                _ => {}
            }
        }
    }

    if services.is_empty() {
        info!("⚠️  gRPC server is accessible but no services are listed (reflection may not be enabled)");
    } else {
        info!("✓ gRPC server is accessible");
        info!("Available services:");
        for service in &services {
            info!("  - {}", service);
        }
    }

    Ok(services)
}

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
