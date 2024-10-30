mod commands;

use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use clap::{Parser, ValueEnum};
use clap_repl::reedline::{
    DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal,
};
use clap_repl::ClapEditor;
use url::Url;

use crate::commands::{deposit, get_balance, send_order, withdraw};

const OP_SEPOLIA_RPC_URL: &str = "https://sepolia.optimism.io";
const OP_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "5fd84259d66Cd46123540766Be93DFE6D43130D7";

const BASE_SEPOLIA_RPC_URL: &str = "https://sepolia.base.org";
const BASE_SEPOLIA_USDC_TOKEN_ADDRESS: &str = "036CbD53842c5426634e7929541eC2318f3dCF7e";

#[derive(Debug, Parser)]
#[command(name = "")]
enum CliTraderCommand {
    /// Initialize a new trading session
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
    /// Send an order
    SendOrder {
        //#[arg(short, long, value_enum)]
        side: Side,
        //#[arg(short, long)]
        amount: u64,
        #[arg(short, long)]
        limit_price: Option<u64>,
    },
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
    /// go long: buy the base token by selling the quote token
    BUY,
    /// go short: sell the quote token by buying the base token
    SELL,
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("cli-trader".to_owned()),
        ..DefaultPrompt::default()
    };

    let rl = ClapEditor::<CliTraderCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            // Do custom things with `Reedline` instance here
            reed.with_history(Box::new(
                FileBackedHistory::with_file(10000, "/tmp/cli-trader-history".into()).unwrap(),
            ))
        })
        .build();

    rl.repl(|command| match command {
        CliTraderCommand::Initialize { url } => {
            println!("Initialized with {url:?}");
        }
        CliTraderCommand::Deposit {
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
        CliTraderCommand::Withdraw {
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
        CliTraderCommand::SendOrder {
            side,
            amount,
            limit_price,
        } => {
            let side = match side {
                Side::BUY => 1,
                Side::SELL => 2,
            };

            let mut rl = Reedline::create();
            //let buy_or_sell = read_input(&mut rl, "Do you wish to BUY or SELL? ");
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            println!("Sending order to {side:?} {amount:?} at limit price {limit_price:?}");

            let result = rt.block_on(send_order::call_send_order(side, amount, Some(limit_price)));
            println!("SendOrder result: {result:?}");

            println!("Order sent");
        }
        CliTraderCommand::GetOrders => {
            println!("Getting orders...");
        }
        CliTraderCommand::CancelOrder { order_id } => {
            println!("Order canceled: {order_id:?}");
        }
        CliTraderCommand::GetBalance => {
            let error_val = Uint::from(99999);
            let op_wallet_balance = rt
                .block_on(get_balance::call_get_erc20_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let op_available_balance = rt
                .block_on(get_balance::call_get_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let op_locked_balance = rt
                .block_on(get_balance::call_get_locked_balance(
                    NamedChain::OptimismSepolia,
                    OP_SEPOLIA_RPC_URL,
                    OP_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_wallet_balance = rt
                .block_on(get_balance::call_get_erc20_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_available_balance = rt
                .block_on(get_balance::call_get_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let base_locked_balance = rt
                .block_on(get_balance::call_get_locked_balance(
                    NamedChain::BaseSepolia,
                    BASE_SEPOLIA_RPC_URL,
                    BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
                ))
                .unwrap_or(error_val);

            let balance_table = get_balance::get_balance_table(
                op_wallet_balance,
                op_available_balance,
                op_locked_balance,
                base_wallet_balance,
                base_available_balance,
                base_locked_balance,
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
        CliTraderCommand::GetOrderbook { market_id } => {
            println!("Getting orderbook: {market_id:?}");
        }
        CliTraderCommand::Quit => {
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
