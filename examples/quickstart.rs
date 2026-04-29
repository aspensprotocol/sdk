//! Quickstart: connect, deposit, trade, and withdraw using the Aspens SDK.
//!
//! Prerequisites:
//!   1. A running Aspens Market Stack (e.g. http://localhost:50051)
//!   2. A `.env` file with at least:
//!        ASPENS_MARKET_STACK_URL=http://localhost:50051
//!        TRADER_PRIVKEY=<your-64-char-hex-private-key>
//!
//! Run:
//!   cargo run --example quickstart

use aspens::commands::config;
use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor};
use eyre::Result;

fn main() -> Result<()> {
    let executor = BlockingExecutor::new();

    // ── 1. Connect ──────────────────────────────────────────────────────
    // Builds a client from .env (reads ASPENS_MARKET_STACK_URL automatically).
    let client = AspensClient::builder().build()?;
    let stack_url = client.stack_url().to_string();
    let privkey = client
        .get_env("TRADER_PRIVKEY")
        .expect("TRADER_PRIVKEY must be set in .env")
        .clone();

    // Fetch the server configuration (chains, tokens, markets).
    let cfg = executor.execute(config::get_config(stack_url.clone()))?;
    println!("Connected to {}", stack_url);

    // ── 2. Check balances ───────────────────────────────────────────────
    executor.execute(balance::balance_from_config(cfg.clone(), privkey.clone()))?;

    // ── 3. Deposit ──────────────────────────────────────────────────────
    // Deposit 1000 units of USDC on the "anvil-1" network.
    // The amount is in the token's smallest unit (e.g. 1000 = 0.001 USDC if 6 decimals).
    executor.execute(deposit::call_deposit_from_config(
        "anvil-1".into(),
        "USDC".into(),
        1000,
        privkey.clone(),
        cfg.clone(),
    ))?;
    println!("Deposit successful");

    // ── 4. Trade ────────────────────────────────────────────────────────
    // Place a limit BUY order: buy 1.5 at price 100.50 on the given market.
    // Amounts are human-readable strings; the SDK converts to pair decimals.
    let market_id = "your-market-id"; // replace with an actual market ID from `config`
    let result = executor.execute(send_order::send_order(
        stack_url.clone(),
        market_id.into(),
        1,                       // side: 1 = BUY, 2 = SELL
        "1.5".into(),            // quantity
        Some("100.50".into()),   // limit price (None for market order)
        privkey.clone(),
        cfg.clone(),
    ))?;
    println!("Order placed (order_id: {})", result.order_id);

    // ── 5. Withdraw ─────────────────────────────────────────────────────
    // Withdraw 500 units of USDC back to your wallet.
    executor.execute(withdraw::call_withdraw_from_config(
        "anvil-1".into(),
        "USDC".into(),
        500,
        privkey.clone(),
        cfg,
    ))?;
    println!("Withdrawal successful");

    Ok(())
}
