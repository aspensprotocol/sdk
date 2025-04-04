pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.rs");
}

use anyhow::Result;
use config_pb::config_service_client::ConfigServiceClient;

pub async fn call_add_market(url: String) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ConfigServiceClient::new(channel);

    // Define the market to add
    let market = config_pb::Market {
        name: "BASE_SEPOLIA_BTC/OP_SEPOLIA_USDC".to_string(),
        slug: "BTC/USDC".to_string(),
        base_chain_network: "base-sepolia".to_string(),
        quote_chain_network: "optimism-sepolia".to_string(),
        base_chain_token_symbol: "BTC".to_string(),
        quote_chain_token_symbol: "USDC".to_string(),
        market_id: None,
    };

    // Create a request object
    let request = tonic::Request::new(market);

    // Call the add_market rpc endpoint
    let response = client.add_market(request).await?;

    // Print the response from the server
    tracing::info!("{:#?}", response.into_inner());

    Ok(())
}
