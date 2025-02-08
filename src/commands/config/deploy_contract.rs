pub mod config_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_config.rs");
}

use anyhow::Result;
use config_pb::config_service_client::ConfigServiceClient;

pub(crate) async fn call_deploy_contract(
    url: String,
    chain_network: &str,
    base_or_quote: &str,
) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ConfigServiceClient::new(channel);

    // Create a request object
    let request = tonic::Request::new(config_pb::DeployContractRequest {
        chain_network: chain_network.into(),
        base_or_quote: base_or_quote.into(),
    });

    // Call the add_market rpc endpoint
    let response = client.deploy_contract(request).await?;

    // Print the response from the server
    println!("{:#?}", response.into_inner());

    Ok(())
}
