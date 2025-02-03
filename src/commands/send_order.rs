pub mod arborter_pb {
    include!("../../proto/generated/xyz.aspens.arborter.rs");
}

use std::env;
use std::fmt;

use alloy::primitives::PrimitiveSignature;
use alloy::signers::Signer;
use alloy_signer_local::PrivateKeySigner;
use anyhow::Result;
use arborter_pb::arborter_service_client::ArborterServiceClient;
use arborter_pb::{Order, SendOrderReply};
use prost::Message;

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let o_ids = self
            .matching_order_ids
            .clone()
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let matching_order_ids = format!("[{}]", o_ids);

        write!(
            f,
            "Order {{\n  side: {},\n  quantity: {},\n  price: {},\n  market_name: {},\n  trade_symbol: {},\n  market_hash: {},\n  base_account_address: {},\n  quote_account_address: {},\n  execution_type: {},\n  matching_order_ids: {},\n  signature_hash: {}\n}}",
            self.side,
            self.quantity,
            self.price.map_or("None".to_string(), |p| p.to_string()),
            self.market_name,
            self.trade_symbol,
            self.market_hash,
            self.base_account_address,
            self.quote_account_address,
            self.execution_type,
            matching_order_ids,
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

pub(crate) async fn call_send_order(side: i32, quantity: u64, price: Option<u64>) -> Result<()> {
    // Create a channel to connect to the gRPC server
    let channel = tonic::transport::Channel::from_static("http://localhost:50051")
        .connect()
        .await?;

    // Instantiate the client
    let mut client = ArborterServiceClient::new(channel);

    // Craft the order
    let mut order = Order {
        side,
        quantity,
        price,
        market_name: "not-considered".to_owned(),
        trade_symbol: "not-considered".to_owned(),
        market_hash: env::var("MARKET_HASH")?,
        base_account_address: env::var("EVM_TESTNET_PUBKEY")?,
        quote_account_address: env::var("EVM_TESTNET_PUBKEY")?,
        execution_type: 0,
        matching_order_ids: vec![],
        signature_hash: vec![],
    };

    // Serialize the order to a byte vector
    let mut buffer = Vec::new();
    order.encode(&mut buffer)?;

    // Sign the order
    let signature = sign_transaction(&buffer).await?;
    order.signature_hash = signature.as_bytes().to_vec();

    // Create a request object
    let request = tonic::Request::new(order);

    // Call the send_order endpoint
    let response = client.send_order(request).await?;

    // Print the response from the server
    println!("Response received: {}", response.into_inner());

    Ok(())
}

async fn sign_transaction(msg_bytes: &[u8]) -> Result<PrimitiveSignature> {
    let signer = env::var("EVM_TESTNET_PRIVKEY")?.parse::<PrivateKeySigner>()?;
    let signature = signer.sign_message(msg_bytes).await?;
    Ok(signature)
}
