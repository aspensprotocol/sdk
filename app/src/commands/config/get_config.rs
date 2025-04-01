pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.rs");
}

use anyhow::Result;
use config_pb::config_service_client::ConfigServiceClient;

pub async fn call_get_config(url: String) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ConfigServiceClient::new(channel);

    // Create a request object
    let request = tonic::Request::new(config_pb::ConfigRequest {});

    // Call the get_config endpoint
    let response = client.get_config(request).await?;

    // Print the response from the server
    println!("{:#?}", response.into_inner());

    Ok(())
}
