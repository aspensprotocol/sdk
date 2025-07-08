// Example demonstrating transaction hash display in CLI and REPL
// 
// This example shows how transaction hashes are now included in the response
// when sending orders through the CLI or REPL.

use aspens::commands::trading::send_order::arborter_pb::{SendOrderResponse, TransactionHash};

fn main() {
    // Simulate a SendOrderResponse with transaction hashes
    let response = SendOrderResponse {
        order_in_book: true,
        order: None,
        trades: vec![],
        transaction_hashes: vec![
            TransactionHash {
                hash_type: "deposit".to_string(),
                hash_value: "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_string(),
            },
            TransactionHash {
                hash_type: "settlement".to_string(),
                hash_value: "0xfedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321".to_string(),
            },
        ],
    };

    println!("=== CLI Output Example ===");
    println!("{}", response);
    
    println!("\n=== Formatted Transaction Hashes ===");
    for formatted_hash in response.get_formatted_transaction_hashes() {
        println!("  {}", formatted_hash);
    }
    
    println!("\n=== REPL Output Example ===");
    println!("aspens> buy 1000000000000000000 --limit-price 2500000000000000000000000000000000000");
    println!("Order sent successfully!");
    println!("Transaction hashes:");
    for formatted_hash in response.get_formatted_transaction_hashes() {
        println!("  {}", formatted_hash);
    }
} 