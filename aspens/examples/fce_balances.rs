//! Wallet + venue balances for one or more traders, with the venue config
//! discovered over FCE.
//!
//! The stack's arborter gRPC is not network-reachable, so config comes through
//! the ext-proxy (`GET_CONFIG`) and each balance is then read from that chain's
//! own RPC. Reports three numbers per (chain, token):
//!
//!   * wallet    — ERC-20 held by the address (or native gas for the sentinel)
//!   * deposited — `MidribV3.tradeBalance`, i.e. custody inside the venue
//!   * native    — the chain's gas token, per chain
//!
//! Usage (cwd holds the .env with EXT_PROXY_URL + DIRECT_API_KEY):
//!   fce_balances <TRADER_PRIVKEY> [TRADER_PRIVKEY ...]

use alloy::primitives::Address;
use aspens::chain_client::resolve_rpc_url;
use aspens::client::AspensClient;
use aspens::commands::trading::balance::{
    call_get_balance_for_address, call_get_erc20_balance_for_address,
    call_get_native_balance_for_address, format_balance,
};
use aspens::wallet::Wallet;
use eyre::Result;

/// The EVM sentinel for a chain's NATIVE token (see native-token support).
const NATIVE_SENTINEL: &str = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE";

#[tokio::main]
async fn main() -> Result<()> {
    let client = AspensClient::builder().build()?;
    eprintln!("transport uses FCE: {}", client.uses_fce());

    // Routes over FCE when the transport is FCE — no arborter gRPC needed.
    let config = client
        .get_config()
        .await?
        .config
        .ok_or_else(|| eyre::eyre!("no configuration in the response"))?;

    let keys: Vec<String> = std::env::args().skip(1).collect();
    if keys.is_empty() {
        eyre::bail!("usage: fce_balances <TRADER_PRIVKEY> [TRADER_PRIVKEY ...]");
    }

    for key in keys {
        let wallet = Wallet::from_evm_hex(&key)?;
        let addr: Address = wallet.address().parse()?;
        println!("\n══ {addr} ══");

        for chain in &config.chains {
            if chain.architecture != "evm" {
                continue;
            }
            // The arborter MASKS each endpoint's url in GetConfig (query
            // values / userinfo become "***") — RPC URLs can carry API keys.
            // `resolve_rpc_url` takes the client-side `ASPENS_RPC_URL_<NETWORK>`
            // override and rejects an unusable masked value, so each chain
            // needs its own override in the .env.
            let primary_url = chain
                .rpcs
                .iter()
                .find(|e| e.enabled)
                .map(|e| e.url.as_str())
                .unwrap_or("");
            let rpc = match resolve_rpc_url(&chain.network, primary_url) {
                Ok(u) => u,
                Err(e) => {
                    println!("\n  {} (chain {})", chain.network, chain.chain_id);
                    println!("    no usable RPC: {e}");
                    continue;
                }
            };
            let native = call_get_native_balance_for_address(&rpc, addr)
                .await
                .map_or_else(|e| format!("error: {e}"), |v| format_balance(v, 18));
            println!("\n  {} (chain {})", chain.network, chain.chain_id);
            println!("    native gas       {native}");

            for (symbol, token) in &chain.tokens {
                let is_native = token.address.eq_ignore_ascii_case(NATIVE_SENTINEL);
                let wallet_bal = if is_native {
                    // The sentinel has no ERC-20 contract; its wallet balance
                    // IS the native balance reported above.
                    "(native, above)".to_string()
                } else {
                    call_get_erc20_balance_for_address(&rpc, &token.address, addr)
                        .await
                        .map_or_else(
                            |e| format!("error: {e}"),
                            |v| format_balance(v, token.decimals),
                        )
                };

                // The venue instance for this chain. Absent => nothing deployed
                // here, so there is no custody to read.
                let deposited = match &chain.trade_contract {
                    Some(tc) => call_get_balance_for_address(
                        chain.chain_id as u64,
                        &rpc,
                        &token.address,
                        &tc.address,
                        addr,
                    )
                    .await
                    .map_or_else(
                        |e| format!("error: {e}"),
                        |v| format_balance(v, token.decimals),
                    ),
                    None => "no instance".to_string(),
                };

                println!("    {symbol:<8} wallet {wallet_bal:>22}  deposited {deposited:>22}");
            }
        }
    }
    Ok(())
}
