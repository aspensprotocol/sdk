//! On-chain (RPC) sol! bindings for Midrib V2 + IERC20.
//!
//! Mirrors the struct-only bindings in the parent `aspens::evm`
//! module, but adds `#[sol(rpc)]` so callers can build alloy contract
//! handles (`MidribV2::new(addr, provider)`) and dispatch on-chain
//! calls. Pulls `alloy-contract`, which is why this submodule is gated
//! on the `client` feature.
//!
//! Lean-signing consumers stay on `aspens::evm::MidribV2` — same JSON
//! artifact, no RPC traits — and avoid the `alloy-contract` /
//! `alloy-provider` dependency cone.

use alloy_sol_types::sol;

sol!(
    #[derive(Debug)]
    #[allow(missing_docs)]
    #[sol(rpc)]
    MidribV2,
    "artifacts/MidribV2.json"
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
