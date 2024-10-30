pub mod arborter_pb {
    include!("../../proto/generated/xyz.aspens.arborter.rs");
}

use std::fmt;

use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderReply};

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Order {{\n  side: {},\n  quantity: {},\n  price: {},\n  market_name: {},\n  trade_symbol: {},\n  market_hash: {},\n  base_account_address: {},\n  quote_account_address: {},\n  execution_type: {},\n  matching_order_id: {},\n  signature_hash: {}\n}}",
            self.side,
            self.quantity,
            self.price.map_or("None".to_string(), |p| p.to_string()),
            self.market_name,
            self.trade_symbol,
            self.market_hash,
            self.base_account_address,
            self.quote_account_address,
            self.execution_type,
            self.matching_order_id.as_deref().unwrap_or("None"),
            hex::encode(&self.signature_hash)
        )
    }
}

impl fmt::Display for SendOrderReply {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SendOrderReply {{\n  order_in_book: {},\n  order: {},\n  trades: [{}]\n}}",
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

pub(crate) async fn call_send_order(
    side: i32,
    quantity: u64,
    price: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_static("http://localhost:50051")
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Create a request object
    let request = tonic::Request::new(Order {
        side,
        quantity,
        price,
        market_name: "m1-irrelevant".to_owned(),
        trade_symbol: "irrelevant".to_owned(),
        market_hash: "1c4fd355c7cfbe92dbdb2dcc1f24dd83456c9b3c362949a92d15f915de9666af".to_owned(),
        base_account_address: "<replace-me>".to_owned(),
        quote_account_address: "<replace-me>".to_owned(),
        execution_type: 0,
        matching_order_id: None,
        signature_hash: [1, 2, 3].to_vec(),
    });

    // Call the send_order endpoint
    let response = client.send_order(request).await?;

    // Print the response from the server
    println!("Response received: {}", response.into_inner());

    Ok(())
}
