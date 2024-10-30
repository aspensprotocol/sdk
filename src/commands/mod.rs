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
        function approve(address spender, uint256 amount) external returns (bool);
        #[derive(Debug)]
        function allowance(address owner, address spender) view returns (uint256);
        #[derive(Debug)]
        function balanceOf(address) external view returns (uint256);
    }
}

const OP_SEPOLIA_CONTRACT_ADDRESS: &str = "0x59305e29A1d409494937FB6EaED32187e143fac1";
//const BASE_SEPOLIA_CONTRACT_ADDRESS: &str = "0x2D8d92AD00609f2fC5Cc7B10cEC9013bD3A4f9F2";
const BASE_SEPOLIA_CONTRACT_ADDRESS: &str = "0x8B9A3a5e445a6810a0F7CfF01B26e79dc62841e1";

pub(crate) mod deposit;
pub(crate) mod withdraw;
pub(crate) mod get_balance;
