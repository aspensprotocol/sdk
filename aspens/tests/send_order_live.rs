//! Live SDK ↔ arborter gasless-order round-trip.
//!
//! Step 4 of the gasless send_order migration (see
//! `GASLESS_MIGRATION_PLAN.md` at repo root). Drives a real gRPC
//! `SendOrderRequest` through `send_order_with_wallet` against a
//! running arborter and verifies the arborter accepted the
//! `GaslessAuthorization` (both chain paths: EVM `lock_for_order_gasless`
//! on-chain, Solana `open_for_ixs` on-chain).
//!
//! # Prerequisites (external — this test does NOT spin them up)
//!
//! 1. **Arborter gRPC** reachable at `$ASPENS_MARKET_STACK_URL`.
//! 2. Arborter admin-initialized with:
//!    - the chains + tokens + markets you intend to trade on
//!    - factory + MidribV2 instance deployed per chain (fee_bps set)
//!    - per-chain signer key loaded (`just init-test-env` territory)
//! 3. **Solana side** (if the trade origin is Solana):
//!    - `solana-test-validator` running
//!    - Midrib program deployed (`anchor deploy`)
//!    - mock-signer running at `/tmp/signer/mock-signer.sock`
//! 4. **EVM side** (if the trade origin is EVM):
//!    - anvil or a devnet reachable at the configured `rpc_url`
//!    - `MidribFactory` + `MidribV2` deployed + `createInstance` called
//! 5. **Trader wallet** already has **deposited** balance on the
//!    origin chain's `tradeBalance` (EVM) or `UserBalance` PDA (Solana)
//!    — gasless flow locks from deposited, it doesn't deposit for you.
//! 6. `TRADER_PRIVKEY` or `TRADER_PRIVKEY_SOLANA` in the env, matching
//!    the curve of the origin chain for the market under test.
//!
//! The `infra/scenarios/solana-evm-local.toml` scenario wires all of
//! this for a local stack.
//!
//! # Env vars read
//!
//! - `ASPENS_MARKET_STACK_URL` — arborter gRPC URL (e.g. `http://localhost:50051`)
//! - `SDK_LIVE_TEST_MARKET_ID` — market to trade (shorthand OK:
//!   `base_network/SYMBOL::quote_network/SYMBOL`)
//! - `SDK_LIVE_TEST_SIDE` — `BID` or `ASK` (default `ASK`)
//! - `SDK_LIVE_TEST_QUANTITY` — pair-decimal quantity, e.g. `"0.001"` (default `"0.001"`)
//! - `SDK_LIVE_TEST_PRICE` — pair-decimal limit price; omit for a market order
//! - `TRADER_PRIVKEY` / `TRADER_PRIVKEY_SOLANA` — at least one, matching
//!   origin-chain curve
//!
//! # Run
//!
//! ```text
//! just test-live-send-order
//! ```

#![cfg(all(feature = "client", feature = "evm", feature = "solana"))]

use std::env;

use aspens::commands::trading::send_order::send_order_with_wallet;
use aspens::wallet::{load_trader_wallet, CurveType};
use aspens::AspensClient;
use eyre::{eyre, Result};

const DEFAULT_QUANTITY: &str = "0.001";
const DEFAULT_SIDE: &str = "ASK";

/// Origin-chain architecture drives which wallet curve we load.
const ARCH_EVM: &str = "EVM";
const ARCH_SOLANA: &str = "Solana";

/// Submits a gasless send_order through the SDK and asserts the arborter
/// accepted it. **#[ignore]d** — depends on external services; run
/// explicitly via `cargo test --test send_order_live -- --ignored`.
#[tokio::test]
#[ignore = "requires a live arborter + configured chains + deposited trader balance"]
async fn send_order_roundtrip_against_live_stack() -> Result<()> {
    let client = AspensClient::builder().build()?;
    let config = client.get_config().await?;

    let market_id = env::var("SDK_LIVE_TEST_MARKET_ID")
        .map_err(|_| eyre!("SDK_LIVE_TEST_MARKET_ID env var is required (see test doc)"))?;
    let market = aspens::commands::trading::send_order::lookup_market(&config, &market_id)?.clone();

    let side_s = env::var("SDK_LIVE_TEST_SIDE").unwrap_or_else(|_| DEFAULT_SIDE.to_string());
    let side = match side_s.to_ascii_uppercase().as_str() {
        "BID" => 1i32,
        "ASK" => 2i32,
        other => {
            return Err(eyre!(
                "unknown SDK_LIVE_TEST_SIDE {other:?} — want BID or ASK"
            ))
        }
    };

    // Origin chain = where the user locks = handler convention: Bid→quote, Ask→base.
    let origin_network = if side == 1 {
        &market.quote_chain_network
    } else {
        &market.base_chain_network
    };
    let origin_chain = config
        .get_chain(origin_network)
        .ok_or_else(|| eyre!("origin chain {origin_network:?} not in config"))?;

    let curve = match origin_chain.architecture.as_str() {
        ARCH_EVM => CurveType::Secp256k1,
        ARCH_SOLANA => CurveType::Ed25519,
        other => return Err(eyre!("unsupported architecture {other:?}")),
    };
    let wallet = load_trader_wallet(curve)?;

    let quantity =
        env::var("SDK_LIVE_TEST_QUANTITY").unwrap_or_else(|_| DEFAULT_QUANTITY.to_string());
    let price = env::var("SDK_LIVE_TEST_PRICE").ok();

    tracing::info!(
        market = %market.market_id,
        %side,
        %quantity,
        ?price,
        origin = %origin_network,
        arch = %origin_chain.architecture,
        addr = %wallet.address(),
        "submitting gasless order against live arborter"
    );

    let response = send_order_with_wallet(
        client.stack_url().to_string(),
        market_id,
        side,
        quantity,
        price,
        &wallet,
        config,
    )
    .await?;

    // The arborter returns at least a `send_order_tx` hash once the
    // gasless authorization is accepted and `lock_for_order_gasless`
    // submitted. If the legacy path had been hit instead, we'd see the
    // typed-error surface from chain-evm's P1 stub (or Solana's
    // pre-gasless error) propagate up before this point.
    let tx_hashes: Vec<&str> = response
        .transaction_hashes
        .iter()
        .map(|t| t.hash_type.as_str())
        .collect();
    assert!(
        tx_hashes.iter().any(|h| *h == "send_order_tx"),
        "expected a send_order_tx in response.transaction_hashes, got: {tx_hashes:?}"
    );
    // Order must either have landed in the book OR matched (producing trades).
    assert!(
        response.order_in_book || !response.trades.is_empty(),
        "order didn't land in the book and didn't match — arborter may have rejected: {response:?}"
    );
    Ok(())
}
