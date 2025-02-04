mod commands;

use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use clap::{Parser, ValueEnum};
use clap_repl::reedline::{
    DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal,
};
use clap_repl::ClapEditor;
use dotenv::dotenv;
use url::Url;

use crate::commands::{balance, deposit, send_order, withdraw};

//const BASE_SEPOLIA_RPC_URL: &str = "https://sepolia.base.org";
//const BASE_SEPOLIA_RPC_URL: &str = "https://base-sepolia-rpc.publicnode.com";
const BASE_SEPOLIA_RPC_URL: &str = "http://localhost:8545";
const BASE_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "036CbD53842c5426634e7929541eC2318f3dCF7e";

//const OP_SEPOLIA_RPC_URL: &str = "https://sepolia.optimism.io";
//const OP_SEPOLIA_RPC_URL: &str = "https://optimism-sepolia-rpc.publicnode.com";
const OP_SEPOLIA_RPC_URL: &str = "http://localhost:8546";
const OP_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "5fd84259d66Cd46123540766Be93DFE6D43130D7";

#[derive(Debug, Parser)]
#[command(name = "")]
enum CliCommand {
    /// Initialize a new trading session by (optionally) defining the arborter gRPC URL endpoint
    Initialize {
        /// The URL of the arborter server
        #[arg(short, long, default_value_t = Url::parse("http://localhost:50051").unwrap())]
        url: Url,
    },
    /// Deposit token(s) to make them available for trading
    Deposit {
        //#[arg(short, long, value_enum)]
        chain: SupportedChain,
        token: String,
        amount: u64,
    },
    /// Withdraw token(s) to a local wallet
    Withdraw {
        chain: SupportedChain,
        token: String,
        amount: u64,
    },
    /// Send a BUY order
    Buy {
        //#[arg(short, long)]
        amount: u64,
        #[arg(short, long)]
        limit_price: Option<u64>,
        #[arg(short, long)]
        matching_order_ids: Option<u64>,
    },
    /// Send a SELL order
    Sell {
        //#[arg(short, long)]
        amount: u64,
        #[arg(short, long)]
        limit_price: Option<u64>,
        #[arg(short, long)]
        matching_order_ids: Option<u64>,
    },
    /// Get a list of all active orders
    GetOrders,
    /// Cancel an order
    CancelOrder {
        /// You will be prompted if you don't provide it.
        #[arg(short, long)]
        order_id: Option<u64>,
    },
    /// Fetch the balances
    GetBalance,
    /// Fetch the latest top of book
    GetOrderbook {
        #[arg(short, long)]
        market_id: Option<String>,
    },
    /// Close the session and quit
    Quit,
}

//enum State {}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum SupportedChain {
    /// Base Sepolia (testnet)
    BaseSepolia,
    /// Optimism Sepolia (testnet)
    OptimismSepolia,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Side {
    /// buy the base token by selling the quote token
    Buy,
    /// sell the quote token by buying the base token
    Sell,
}

fn main() {
    dotenv().ok();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("aspens".to_owned()),
        ..DefaultPrompt::default()
    };

    let rl = ClapEditor::<CliCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            reed.with_history(Box::new(
                FileBackedHistory::with_file(10000, "/tmp/aspens-cli-history".into()).unwrap(),
            ))
        })
        .build();

    rl.repl(|command| match command {
        CliCommand::Initialize { url } => {
            println!("Initialized with {url:?}");
        }
        CliCommand::Deposit {
            chain,
            token,
            amount,
        } => {
            println!("Depositing {amount:?} {token:?} on {chain:?}");
            let named_chain = match chain {
                SupportedChain::OptimismSepolia => NamedChain::OptimismSepolia,
                SupportedChain::BaseSepolia => NamedChain::BaseSepolia,
            };

            let rpc_url = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_RPC_URL,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_RPC_URL,
            };

            let token_address = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            };

            let call_deposit_result = rt.block_on(deposit::call_deposit(
                named_chain,
                rpc_url,
                token_address,
                amount,
            ));
            println!("Deposit result: {call_deposit_result:?}");
        }
        CliCommand::Withdraw {
            chain,
            token,
            amount,
        } => {
            println!("Withdrawing {amount:?} {token:?} on {chain:?}");
            let named_chain = match chain {
                SupportedChain::OptimismSepolia => NamedChain::OptimismSepolia,
                SupportedChain::BaseSepolia => NamedChain::BaseSepolia,
            };

            let rpc_url = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_RPC_URL,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_RPC_URL,
            };

            let token_address = match chain {
                SupportedChain::OptimismSepolia => OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                SupportedChain::BaseSepolia => BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            };

            let call_withdraw_result = rt.block_on(withdraw::call_withdraw(
                named_chain,
                rpc_url,
                token_address,
                amount,
            ));
            println!("Withdraw result: {call_withdraw_result:?}");
        }
        CliCommand::Buy {
            amount,
            limit_price,
            matching_order_ids,
        } => {
            let mut rl = Reedline::create();
            //let buy_or_sell = read_input(&mut rl, "Do you wish to BUY or SELL? ");
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            println!("Sending BUY order for {amount:?} at limit price {limit_price:?}");

            let result = rt.block_on(send_order::call_send_order(1, amount, Some(limit_price)));

            println!("SendOrder result: {result:?}");
            println!("Order sent");
        }
        CliCommand::Sell {
            amount,
            limit_price,
            matching_order_ids,
        } => {
            let mut rl = Reedline::create();
            //let buy_or_sell = read_input(&mut rl, "Do you wish to BUY or SELL? ");
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            println!("Sending SELL order for {amount:?} at limit price {limit_price:?}");

            let result = rt.block_on(send_order::call_send_order(2, amount, Some(limit_price)));

            println!("SendOrder result: {result:?}");
            println!("Order sent");
        }
        CliCommand::GetOrders => {
            println!("Getting orders...");
            println!("TODO: Implement this");
        }
        CliCommand::CancelOrder { order_id } => {
            println!("Order canceled: {order_id:?}");
            println!("TODO: Implement this");
        }
        CliCommand::GetBalance => {
            let error_val = Uint::from(99999);
            let op_wallet_balance = rt
                .block_on(balance::call_get_erc20_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let op_available_balance = rt
                .block_on(balance::call_get_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let op_locked_balance = rt
                .block_on(balance::call_get_locked_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_wallet_balance = rt
                .block_on(balance::call_get_erc20_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_available_balance = rt
                .block_on(balance::call_get_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_locked_balance = rt
                .block_on(balance::call_get_locked_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let balance_table = balance::balance_table(
                vec!["USDC", "Base Sepolia", "Optimism Sepolia"],
                base_wallet_balance,
                base_available_balance,
                base_locked_balance,
                op_wallet_balance,
                op_available_balance,
                op_locked_balance,
            );
            if op_wallet_balance.eq(&error_val)
                | op_available_balance.eq(&error_val)
                | op_locked_balance.eq(&error_val)
                | base_wallet_balance.eq(&error_val)
                | base_available_balance.eq(&error_val)
                | base_locked_balance.eq(&error_val)
            {
                println!("** A '99999' value represents an error in fetching the actual value");
            }

            println!("{balance_table}");
        }
        CliCommand::GetOrderbook { market_id } => {
            println!("Getting orderbook: {market_id:?}");
            println!("TODO: Implement this");
        }
        CliCommand::Quit => {
            println!("goodbye");
            std::process::exit(0)
        }
    });
}

fn read_input(rl: &mut Reedline, prompt: &str) -> String {
    let Signal::Success(line) = rl
        .read_line(&DefaultPrompt::new(
            DefaultPromptSegment::Basic(prompt.to_owned()),
            DefaultPromptSegment::Empty,
        ))
        .unwrap()
    else {
        panic!();
    };
    line
}
