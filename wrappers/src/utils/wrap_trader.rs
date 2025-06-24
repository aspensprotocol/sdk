use alloy_chains::NamedChain;
use anyhow::{Context, Result};
#[cfg(feature = "admin")]
use aspens::commands::config::{self, config_pb};

#[cfg(any(feature = "admin", feature = "trader"))]
use aspens::commands::trading::{balance, deposit, send_order, withdraw};

use std::str::FromStr;
use tracing::info;

use crate::utils::executor::AsyncExecutor;

pub fn wrap_deposit<E: AsyncExecutor>(
    executor: &E,
    chain: String,
    token: String,
    amount: u64,
) -> Result<()> {
    let chain = parse_chain(&chain)?;
    info!("Depositing {amount:?} {token:?} on {chain:?}");
    let (rpc_url, token_address) = get_chain_config(chain)?;
    let (contract_address, privkey) = get_chain_contract_and_privkey(chain);
    let result = executor.execute(deposit::call_deposit(
        chain,
        rpc_url,
        token_address,
        contract_address,
        privkey,
        amount,
    ))?;
    info!("Deposit result: {result:?}");
    Ok(())
}

pub fn wrap_withdraw<E: AsyncExecutor>(
    executor: &E,
    chain: String,
    token: String,
    amount: u64,
) -> Result<()> {
    let chain = parse_chain(&chain)?;
    info!("Withdrawing {amount:?} {token:?} on {chain:?}");
    let (rpc_url, token_address) = get_chain_config(chain)?;
    let (contract_address, privkey) = get_chain_contract_and_privkey(chain);
    let result = executor.execute(withdraw::call_withdraw(
        chain,
        rpc_url,
        token_address,
        contract_address,
        privkey,
        amount,
    ));
    info!("Withdraw result: {result:?}");
    Ok(())
}

pub fn wrap_buy<E: AsyncExecutor>(
    executor: &E,
    url: String,
    amount: String,
    limit_price: Option<String>,
) -> Result<()> {
    info!("Sending BUY order for {amount:?} at limit price {limit_price:?}");
    let market_id = std::env::var("MARKET_ID_1").unwrap();
    let pubkey = std::env::var("EVM_TESTNET_PUBKEY").unwrap();
    let privkey = std::env::var("EVM_TESTNET_PRIVKEY").unwrap();
    let result = executor.execute(send_order::call_send_order(
        url,
        1, // Buy side
        amount,
        limit_price,
        market_id.clone(),
        pubkey.clone(),
        pubkey.clone(),
        privkey,
    ))?;
    info!("SendOrder result: {result:?}");
    info!("Order sent");
    Ok(())
}

pub fn wrap_sell<E: AsyncExecutor>(
    executor: &E,
    url: String,
    amount: String,
    limit_price: Option<String>,
) -> Result<()> {
    info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");
    let market_id = std::env::var("MARKET_ID_1").unwrap();
    let pubkey = std::env::var("EVM_TESTNET_PUBKEY").unwrap();
    let privkey = std::env::var("EVM_TESTNET_PRIVKEY").unwrap();
    let result = executor.execute(send_order::call_send_order(
        url,
        2, // Sell side
        amount,
        limit_price,
        market_id.clone(),
        pubkey.clone(),
        pubkey.clone(),
        privkey,
    ))?;
    info!("SendOrder result: {result:?}");
    info!("Order sent");
    Ok(())
}

pub fn wrap_balance<E: AsyncExecutor>(executor: &E) -> Result<()> {
    info!("Getting balance");
    let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
    let base_chain_usdc_token_address = std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
    let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
    let quote_chain_usdc_token_address = std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
    let base_chain_contract_address = std::env::var("BASE_CHAIN_CONTRACT_ADDRESS").unwrap();
    let quote_chain_contract_address = std::env::var("QUOTE_CHAIN_CONTRACT_ADDRESS").unwrap();
    let privkey = std::env::var("EVM_TESTNET_PRIVKEY").unwrap();

    let result = executor.execute(balance::balance(
        base_chain_rpc_url,
        base_chain_usdc_token_address,
        quote_chain_rpc_url,
        quote_chain_usdc_token_address,
        base_chain_contract_address,
        quote_chain_contract_address,
        privkey,
    ))?;
    info!("Balance result: {result:?}");
    Ok(())
}

fn get_chain_config(chain: NamedChain) -> Result<(String, String)> {
    let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
    let base_chain_usdc_token_address = std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
    let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
    let quote_chain_usdc_token_address = std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
    let rpc_url = match chain {
        NamedChain::BaseGoerli => base_chain_rpc_url,
        NamedChain::BaseSepolia => quote_chain_rpc_url,
        _ => unreachable!(),
    };
    let token_address = match chain {
        NamedChain::BaseGoerli => base_chain_usdc_token_address,
        NamedChain::BaseSepolia => quote_chain_usdc_token_address,
        _ => unreachable!(),
    };
    Ok((rpc_url, token_address))
}

fn get_chain_contract_and_privkey(chain: NamedChain) -> (String, String) {
    let base_chain_contract_address = std::env::var("BASE_CHAIN_CONTRACT_ADDRESS").unwrap();
    let quote_chain_contract_address = std::env::var("QUOTE_CHAIN_CONTRACT_ADDRESS").unwrap();
    let privkey = std::env::var("EVM_TESTNET_PRIVKEY").unwrap();
    let contract_address = match chain {
        NamedChain::BaseGoerli => base_chain_contract_address,
        NamedChain::BaseSepolia => quote_chain_contract_address,
        _ => unreachable!(),
    };
    (contract_address, privkey)
}

// Helper function to parse chain string into NamedChain
fn parse_chain(chain_str: &str) -> Result<NamedChain> {
    NamedChain::from_str(chain_str).with_context(|| {
        format!("Invalid chain name: {chain_str}. Valid chains are: base-goerli or base-sepolia")
    })
}
