use alloy_sol_types::sol;

sol! {
    #[sol(rpc)]
    Midrib,
    "artifacts/Midrib.json"
}

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

const BASE_SEPOLIA_CONTRACT_ADDRESS: &str = "0x1F18C30358761eb1B4e2d088327e0fA7D2ea3303";
const OP_SEPOLIA_CONTRACT_ADDRESS: &str = "0x59305e29A1d409494937FB6EaED32187e143fac1";

pub(crate) mod balance;
pub(crate) mod deposit;
pub(crate) mod send_order;
pub(crate) mod withdraw;
