use alloy_sol_types::sol;

sol! {
    #[sol(rpc)]
    Midrib,
    "artifacts/Midrib.json"
}

pub(crate) mod get_balance;
