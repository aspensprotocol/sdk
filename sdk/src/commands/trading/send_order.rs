pub mod arborter_pb {
    include!("../../../proto/generated/xyz.aspens.arborter.v1.rs");
}

use std::fmt;

use alloy::primitives::Signature;
use alloy::signers::{local::PrivateKeySigner, Signer};
use anyhow::Result;
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderRequest, SendOrderResponse};
use prost::Message;

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Order {{\n  side: {},\n  quantity: {},\n  price: {},\n  market_id: {},\n  base_account_address: {},\n  quote_account_address: {},\n  execution_type: {},\n  matching_order_ids: {:?}\n}}",
            self.side,
            self.quantity,
            self.price.map_or("None".to_string(), |p| p.to_string()),
            self.market_id,
            self.base_account_address,
            self.quote_account_address,
            self.execution_type,
            self.matching_order_ids
        )
    }
}

impl fmt::Display for SendOrderResponse {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SendOrderResponse {{\n  order_in_book: {},\n  order: {},\n  trades: [{}]\n}}",
            self.order_in_book,
            self.order
                .as_ref()
                .map_or("None".to_string(), |o| format!("{}", o)),
            self.trades
                .iter()
                .map(|t| format!("{:?}", t))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

pub async fn call_send_order(
    url: String,
    side: i32,
    quantity: u64,
    price: Option<u64>,
    market_id: String,
    base_account_address: String,
    quote_account_address: String,
    privkey: String,
) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Craft the order
    let order = Order {
        side,
        quantity,
        price,
        market_id,
        base_account_address,
        quote_account_address,
        execution_type: 0,
        matching_order_ids: vec![],
    };

    // Serialize the order to a byte vector
    let mut buffer = Vec::new();
    order.encode(&mut buffer)?;

    // Sign the order
    let signature = sign_transaction(&buffer, &privkey).await?;

    // Create the request with the order and signature
    let request = SendOrderRequest {
        order: Some(order),
        signature_hash: signature.as_bytes().to_vec(),
    };

    // Create a tonic request
    let request = tonic::Request::new(request);

    // Call the send_order endpoint
    let response = client.send_order(request).await?;

    // Print the response from the server
    tracing::info!("Response received: {}", response.into_inner());

    Ok(())
}

async fn sign_transaction(msg_bytes: &[u8], privkey: &str) -> Result<Signature> {
    let signer = privkey.parse::<PrivateKeySigner>()?;
    let signature = signer.sign_message(msg_bytes).await?;
    Ok(signature)
}
