//! Quickstart: connect, deposit, trade, and withdraw using the Aspens SDK.
//!
//! Prerequisites:
//!   1. A running Aspens Market Stack (e.g. http://localhost:50051)
//!   2. A `.env` file with at least:
//!      ASPENS_MARKET_STACK_URL=http://localhost:50051
//!      TRADER_PRIVKEY=<your-64-char-hex-private-key>
//!
//! Run:
//!   cargo run -p aspens --example quickstart

use aspens::commands::config;
use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor, Wallet};
use eyre::Result;

fn main() -> Result<()> {
    let executor = BlockingExecutor::new();

    // ── 1. Connect ──────────────────────────────────────────────────────
    // Builds a client from .env (reads ASPENS_MARKET_STACK_URL automatically).
    let client = AspensClient::builder().build()?;
    let stack_url = client.stack_url().to_string();
    // Hold the privkey as a String — `Wallet` is not Clone (Solana
    // keypairs intentionally aren't), so we re-build a fresh `Wallet`
    // inside each `async move` block below.
    let privkey = client
        .get_env("TRADER_PRIVKEY")
        .expect("TRADER_PRIVKEY must be set in .env")
        .clone();

    // Fetch the server configuration (chains, tokens, markets).
    let cfg = executor.execute(config::get_config(stack_url.clone()))?;
    println!("Connected to {}", stack_url);

    // ── 2. Check balances ───────────────────────────────────────────────
    let cfg_clone = cfg.clone();
    let pk = privkey.clone();
    executor.execute(async move {
        let wallet = Wallet::from_evm_hex(&pk)?;
        let wallets: [&Wallet; 1] = [&wallet];
        balance::balance_from_config_with_wallets(cfg_clone, &wallets).await
    })?;

    // ── 3. Deposit ──────────────────────────────────────────────────────
    // Deposit 1000 units of USDC on the "anvil-1" network.
    // The amount is in the token's smallest unit (e.g. 1000 = 0.001 USDC if 6 decimals).
    let cfg_clone = cfg.clone();
    let pk = privkey.clone();
    executor.execute(async move {
        let wallet = Wallet::from_evm_hex(&pk)?;
        deposit::call_deposit_from_config_with_wallet(
            "anvil-1".into(),
            "USDC".into(),
            1000,
            &wallet,
            cfg_clone,
        )
        .await
    })?;
    println!("Deposit successful");

    // ── 4. Trade ────────────────────────────────────────────────────────
    // Place a limit BUY order: buy 1.5 at price 100.50 on the given market.
    // Amounts are human-readable strings; the SDK converts to pair decimals.
    //
    // The last arg, `post_only`, when true asks arborter to reject the
    // order (with FAILED_PRECONDITION, no on-chain lock) if the price
    // would cross at submission. Use it for guaranteed maker-side
    // execution; leave it false for the normal take-or-rest behavior.
    let market_id = "your-market-id"; // replace with an actual market ID from `config`
    let cfg_clone = cfg.clone();
    let stack_url_clone = stack_url.clone();
    let pk = privkey.clone();
    let result = executor.execute(async move {
        let wallet = Wallet::from_evm_hex(&pk)?;
        send_order::send_order_with_wallet(
            stack_url_clone,
            market_id.into(),
            1,                     // side: 1 = BUY, 2 = SELL
            "1.5".into(),          // quantity
            Some("100.50".into()), // limit price (None for market order)
            &wallet,
            cfg_clone,
            false, // post_only
            false, // hidden
        )
        .await
    })?;
    println!("Order placed (order_id: {})", result.order_id);

    // ── 5. Withdraw ─────────────────────────────────────────────────────
    // Withdraw 500 units of USDC back to your wallet.
    executor.execute(async move {
        let wallet = Wallet::from_evm_hex(&privkey)?;
        withdraw::call_withdraw_from_config_with_wallet(
            stack_url,
            "anvil-1".into(),
            "USDC".into(),
            500,
            &wallet,
            cfg,
        )
        .await
    })?;
    println!("Withdrawal successful");

    Ok(())
}
