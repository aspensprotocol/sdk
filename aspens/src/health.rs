use eyre::{Context, Result};
use tonic::transport::Channel;
use tonic_reflection::pb::v1::{
    server_reflection_client::ServerReflectionClient, server_reflection_request::MessageRequest,
    server_reflection_response::MessageResponse, ServerReflectionRequest,
};
use tracing::info;

/// Check if the gRPC server is accessible by attempting to list services via reflection
pub async fn check_grpc_server(url: String) -> Result<Vec<String>> {
    info!("Connecting to gRPC server at {}", url);

    // Create a channel to connect to the gRPC server
    let channel = Channel::from_shared(url.clone())
        .wrap_err("Invalid gRPC server URL")?
        .connect()
        .await
        .wrap_err("Failed to connect to gRPC server")?;

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
