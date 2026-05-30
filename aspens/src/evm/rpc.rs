//! On-chain (RPC) sol! bindings for Midrib V3 + IERC20.
//!
//! `#[sol(rpc)]` so callers can build alloy contract handles
//! (`MidribV3::new(addr, provider)`) and dispatch on-chain calls
//! (deposit, withdraw-voucher, tradeBalance). Pulls `alloy-contract`,
//! which is why this submodule is gated on the `client` feature.
//!
//! MidribV3 is the optimistic-ledger contract — the V2 on-chain order
//! machinery is burned. The kept surface the SDK calls here is deposit /
//! withdraw(voucher, sig) / tradeBalance.

use alloy_sol_types::sol;

sol!(
    #[derive(Debug)]
    #[allow(missing_docs)]
    #[sol(rpc)]
    MidribV3,
    "artifacts/MidribV3.json"
);

sol! {
    #[sol(abi, rpc)]
    contract IERC20 {
        #[derive(Debug)]
        function allowance(address owner, address spender) view returns (uint256);
        #[derive(Debug)]
        function approve(address spender, uint256 amount) external returns (bool);
        #[derive(Debug)]
        function balanceOf(address) external view returns (uint256);
    }
}
