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

pub mod balance;
pub mod cancel_order;
pub mod deposit;
pub mod send_order;
pub mod withdraw;
