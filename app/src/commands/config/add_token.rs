pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.rs");
}

use anyhow::Result;
use config_pb::config_service_client::ConfigServiceClient;

pub async fn call_add_token(url: String, network: &str) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ConfigServiceClient::new(channel);

    let token = config_pb::Token {
        decimals: 6,
        token_id: None,
        name: "USDC".to_string(),
        symbol: "USDC".to_string(),
        address: "0x".to_string(),
    };

    // Create a request object
    let request = tonic::Request::new(config_pb::AddTokenRequest {
        chain_network: network.into(),
        token: Some(token),
    });

    // Call the add_market rpc endpoint
    let response = client.add_token(request).await?;

    // Print the response from the server
    println!("{:#?}", response.into_inner());

    Ok(())
}
