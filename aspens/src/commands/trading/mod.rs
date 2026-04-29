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

/// Query balances across chains (native gas, ERC-20 / SPL, locked / withdrawable).
pub mod balance;
/// Submit a `cancel_order` request and decode the gRPC response.
pub mod cancel_order;
/// Deposit tokens into the trading contract so they're available to trade.
pub mod deposit;
/// Build the gasless cross-chain order envelope used by `send_order`.
pub mod gasless;
/// Build, sign, and submit a buy/sell order envelope.
pub mod send_order;
/// Subscribe to the orderbook stream for a given market.
pub mod stream_orderbook;
/// Subscribe to the trades stream for a given market.
pub mod stream_trades;
/// Withdraw tokens from the trading contract back to the user's wallet.
pub mod withdraw;
