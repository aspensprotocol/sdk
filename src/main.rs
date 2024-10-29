mod commands;

use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use clap::{Parser, ValueEnum};
use clap_repl::reedline::{
    DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal,
};
use clap_repl::ClapEditor;
use url::Url;

use crate::commands::get_balance;

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
        #[arg(short, long, value_enum, default_value_t = SupportedChain::BaseSepolia)]
        chain: SupportedChain,
        #[arg(short, long)]
        token: String,
        #[arg(short, long)]
        amount: u64,
    },
    /// Withdraw token(s) to a local wallet
    Withdraw {
        #[arg(short, long)]
        chain: SupportedChain,
        #[arg(short, long)]
        token: String,
        #[arg(short, long)]
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

enum State {}

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
        }
        CliTraderCommand::Withdraw {
            chain,
            token,
            amount,
        } => {
            println!("Withdrawing {amount:?} {token:?} on {chain:?}");
        }
        CliTraderCommand::SendOrder {
            side,
            amount,
            limit_price,
        } => {
            let mut rl = Reedline::create();
            //let buy_or_sell = read_input(&mut rl, "Do you wish to BUY or SELL? ");
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            println!("Sending order to {side:?} {amount:?} at limit price {limit_price:?}");

            println!("Order sent");
        }
        CliTraderCommand::GetOrders => {
            println!("Getting orders...");
        }
        CliTraderCommand::CancelOrder { order_id } => {
            println!("Order canceled: {order_id:?}");
        }
        CliTraderCommand::GetBalance => {
            let op_available_balance = rt.block_on(get_balance::call_get_balance(
                NamedChain::OptimismSepolia,
                OP_SEPOLIA_RPC_URL,
                OP_SEPOLIA_USDC_TOKEN_ADDRESS,
            ));

            let op_locked_balance = rt.block_on(get_balance::call_get_locked_balance(
                NamedChain::OptimismSepolia,
                OP_SEPOLIA_RPC_URL,
                OP_SEPOLIA_USDC_TOKEN_ADDRESS,
            ));

            let base_available_balance = rt.block_on(get_balance::call_get_balance(
                NamedChain::BaseSepolia,
                BASE_SEPOLIA_RPC_URL,
                BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            ));
            let base_locked_balance = rt.block_on(get_balance::call_get_locked_balance(
                NamedChain::BaseSepolia,
                BASE_SEPOLIA_RPC_URL,
                BASE_SEPOLIA_USDC_TOKEN_ADDRESS,
            ));

            let balance_table = get_balance::get_balance_table(
                Uint::from(9999),
                op_available_balance.unwrap_or(Uint::from(9999)),
                op_locked_balance.unwrap_or(Uint::from(9999)),
                Uint::from(9999),
                base_available_balance.unwrap_or(Uint::from(9999)),
                base_locked_balance.unwrap_or(Uint::from(9999)),
            );
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
