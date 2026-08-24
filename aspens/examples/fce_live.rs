//! Live FCE direct-action harness — the first end-to-end round-trip of the
//! `fce` transport against a production ext-proxy (fce-95a1bf89, 2026-07-27).
//!
//! Reads the standard .env (ASPENS_MARKET_STACK_URL for gRPC config discovery,
//! EXT_PROXY_URL + DIRECT_API_KEY to auto-select the FCE transport,
//! TRADER_PRIVKEY for the signing wallet).
//!
//! Usage (cwd holds the .env):
//!   fce_live book     <market_id> [depth]
//!   fce_live place    <side> <qty> [price]      (market_id via MARKET_ID env)
//!   fce_live cancel   <side> <token_addr> <order_id>
//!   fce_live mystate  [trader]
//!   fce_live history  [trader]
//!   fce_live withdraw <network> <token_addr> <amount_raw>

use aspens::client::AspensClient;
use aspens::commands::trading::fce_actions;
use aspens::wallet::{CurveType, load_trader_wallet};
use eyre::{Result, eyre};

fn market_id() -> String {
    std::env::var("MARKET_ID").unwrap_or_default()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("");

    let client = AspensClient::builder().build()?;
    eprintln!("transport uses FCE: {}", client.uses_fce());

    match action {
        "book" => {
            let mid = args.get(1).cloned().unwrap_or_else(market_id);
            let depth = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(0);
            let out = fce_actions::book_state(&client, &mid, depth).await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        "place" => {
            let wallet = load_trader_wallet(CurveType::Secp256k1)?;
            eprintln!("wallet: {}", wallet.address());
            let side = args.get(1).ok_or_else(|| eyre!("need side"))?;
            let qty = args.get(2).ok_or_else(|| eyre!("need qty"))?;
            let price = args.get(3).map(String::as_str);
            let mid = market_id();
            let out = fce_actions::place_order(&client, &[&wallet], &mid, side, qty, price, false)
                .await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        "cancel" => {
            let wallet = load_trader_wallet(CurveType::Secp256k1)?;
            let side = args.get(1).ok_or_else(|| eyre!("need side"))?;
            let token = args.get(2).ok_or_else(|| eyre!("need token addr"))?;
            let oid_str = args.get(3).ok_or_else(|| eyre!("need order id"))?;
            let oid_body = oid_str.strip_prefix("0x").unwrap_or(oid_str);
            let oid_bytes =
                hex::decode(oid_body).map_err(|e| eyre!("order id '{oid_str}' is not hex: {e}"))?;
            let oid: [u8; 32] = oid_bytes.try_into().map_err(|v: Vec<u8>| {
                eyre!(
                    "order_id must be exactly 32 bytes, got {} (from '{oid_str}')",
                    v.len()
                )
            })?;
            let out =
                fce_actions::cancel_order(&client, &wallet, &market_id(), side, token, oid).await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        "mystate" => {
            let trader = match args.get(1) {
                Some(t) => t.clone(),
                None => load_trader_wallet(CurveType::Secp256k1)?.address(),
            };
            let out = fce_actions::my_state(&client, &market_id(), &trader).await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        "history" => {
            let trader = match args.get(1) {
                Some(t) => t.clone(),
                None => load_trader_wallet(CurveType::Secp256k1)?.address(),
            };
            let out = fce_actions::export_history(&client, &market_id(), &trader).await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        "withdraw" => {
            let wallet = load_trader_wallet(CurveType::Secp256k1)?;
            let network = args.get(1).ok_or_else(|| eyre!("need network"))?;
            let token = args.get(2).ok_or_else(|| eyre!("need token addr"))?;
            let amount = args.get(3).ok_or_else(|| eyre!("need raw amount"))?;
            let account = wallet.address();
            let out =
                fce_actions::withdraw(&client, &wallet, network, token, &account, amount).await?;
            println!("status={} log={}", out.status, out.log);
            println!("{}", serde_json::to_string_pretty(&out.data)?);
        }
        other => return Err(eyre!("unknown action '{other}'")),
    }
    Ok(())
}
