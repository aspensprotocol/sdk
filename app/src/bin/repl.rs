use alloy::primitives::Uint;
use alloy_chains::NamedChain;
use anyhow::{Context, Result};
use aspens::commands::config::{self, add_market, add_token, deploy_contract};
use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use clap::{Parser, ValueEnum};
use clap_repl::reedline::{
    DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal,
};
use clap_repl::ClapEditor;
use dotenv::dotenv;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;

struct AppState {
    url: Arc<Mutex<Url>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            url: Arc::new(Mutex::new(Url::parse("http://0.0.0.0:50051").unwrap())),
        }
    }

    fn with_url(&mut self, url: Url) {
        let mut guard = self.url.lock().unwrap();
        *guard = url;
    }

    fn url(&self) -> String {
        let guard = self.url.lock().unwrap();
        guard.to_string()
    }
}

#[derive(Debug, Parser)]
#[command(name = "", author, version, about, long_about = None)]
enum ReplCommand {
    /// Initialize a new trading session by (optionally) defining the arborter URL
    Initialize {
        /// The URL of the arborter server
        #[arg(short, long, default_value_t = Url::parse("http://0.0.0.0:50051").unwrap())]
        url: Url,
    },
    /// Config: Fetch the current configuration from the arborter server
    GetConfig,
    /// Config: Download configuration to a file
    DownloadConfig {
        /// Path to save the configuration file
        #[arg(short, long)]
        path: String,
    },
    /// Config: Add a new market to the arborter service
    AddMarket,
    /// Config: Add a new token to the arborter service
    AddToken {
        /// The chain network to add the token to
        chain: String,
    },
    /// Deploy the trade contract onto the given chain
    DeployContract {
        /// The chain network to deploy the contract to
        chain: String,
        base_or_quote: BaseOrQuote,
    },
    /// Deposit token(s) to make them available for trading
    Deposit {
        /// The chain network to deposit to
        chain: String,
        token: String,
        amount: u64,
    },
    /// Withdraw token(s) to a local wallet
    Withdraw {
        /// The chain network to withdraw from
        chain: String,
        token: String,
        amount: u64,
    },
    /// Send a BUY order
    Buy {
        /// Amount to buy
        amount: u64,
        #[arg(short, long)]
        limit_price: Option<u64>,
        #[arg(short, long)]
        matching_order_ids: Option<u64>,
    },
    /// Send a SELL order
    Sell {
        /// Amount to sell
        amount: u64,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<u64>,
        #[arg(short, long)]
        matching_order_ids: Option<u64>,
    },
    /// Get a list of all active orders
    GetOrders,
    /// Cancel an order
    CancelOrder {
        /// Order ID to cancel
        order_id: u64,
    },
    /// Fetch the balances
    Balance,
    /// Fetch the latest top of book
    GetOrderbook {
        /// Market ID to fetch orderbook for
        market_id: String,
    },
    /// Quit the REPL
    Quit,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum BaseOrQuote {
    Base,
    Quote,
}

impl std::fmt::Display for BaseOrQuote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BaseOrQuote::Base => write!(f, "base"),
            BaseOrQuote::Quote => write!(f, "quote"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Side {
    /// (bid to) buy the base token by selling the quote token
    Buy,
    /// (offer to) sell the quote token by buying the base token
    Sell,
}

// Helper function to parse chain string into NamedChain
fn parse_chain(chain_str: &str) -> Result<NamedChain> {
    NamedChain::from_str(chain_str).with_context(|| {
        format!("Invalid chain name: {chain_str}. Valid chains are: base-goerli or base-sepolia")
    })
}

fn main() {
    dotenv().ok();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    let mut app_state = AppState::new();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("aspens".to_owned()),
        ..DefaultPrompt::default()
    };

    let rl = ClapEditor::<ReplCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            reed.with_history(Box::new(
                FileBackedHistory::with_file(10000, "/tmp/aspens-cli-history".into()).unwrap(),
            ))
        })
        .build();

    rl.repl(|command| match command {
        ReplCommand::Initialize { url } => {
            app_state.with_url(url.clone());
            info!("Initialized session at {url:?}");
            info!("Available config for {url:?} is <TODO!!>");
        }
        ReplCommand::GetConfig => {
            info!("Fetching config...");
            let url = app_state.url();
            let result = rt.block_on(config::call_get_config(url));
            info!("GetConfig result: {result:#?}");
        }
        ReplCommand::DownloadConfig { path } => {
            info!("Downloading config to file: {path}");
            let url = app_state.url();
            let result = rt.block_on(config::download_config_to_file(url, path));
            info!("DownloadConfig result: {result:?}");
        }
        ReplCommand::AddMarket => {
            info!("Adding market...");
            let url = app_state.url();
            let result = rt.block_on(add_market::call_add_market(url));
            info!("AddMarket result: {result:?}");
        }
        ReplCommand::AddToken { chain } => {
            info!("Adding token ___ on {chain:?}");
            let url = app_state.url();
            let result = rt.block_on(add_token::call_add_token(url, chain.as_ref()));
            info!("AddToken result: {result:?}");
        }
        ReplCommand::DeployContract {
            chain,
            base_or_quote,
        } => {
            info!("Deploying contract on {chain:?}");
            let url = app_state.url();
            let result = rt.block_on(deploy_contract::call_deploy_contract(
                url,
                chain.as_ref(),
                &base_or_quote.to_string(),
            ));
            info!("DeployContract result: {result:?}");
        }
        ReplCommand::Deposit {
            chain,
            token,
            amount,
        } => {
            let chain = parse_chain(&chain).unwrap();
            info!("Depositing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let rpc_url = match chain {
                NamedChain::BaseGoerli => base_chain_rpc_url,
                NamedChain::BaseSepolia => quote_chain_rpc_url,
                _ => unreachable!(),
            };

            let token_address = match chain {
                NamedChain::BaseGoerli => base_chain_usdc_token_address,
                NamedChain::BaseSepolia => quote_chain_usdc_token_address,
                _ => unreachable!(),
            };

            let result = rt.block_on(deposit::call_deposit(
                chain,
                &rpc_url,
                &token_address,
                amount,
            ));
            info!("Deposit result: {result:?}");
        }
        ReplCommand::Withdraw {
            chain,
            token,
            amount,
        } => {
            let chain = parse_chain(&chain).unwrap();
            info!("Withdrawing {amount:?} {token:?} on {chain:?}");
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let rpc_url = match chain {
                NamedChain::BaseGoerli => base_chain_rpc_url,
                NamedChain::BaseSepolia => quote_chain_rpc_url,
                _ => unreachable!(),
            };

            let token_address = match chain {
                NamedChain::BaseGoerli => base_chain_usdc_token_address,
                NamedChain::BaseSepolia => quote_chain_usdc_token_address,
                _ => unreachable!(),
            };

            let result = rt.block_on(withdraw::call_withdraw(
                chain,
                &rpc_url,
                &token_address,
                amount,
            ));
            info!("Withdraw result: {result:?}");
        }
        ReplCommand::Buy {
            amount,
            limit_price,
            matching_order_ids: _,
        } => {
            let mut rl = Reedline::create();
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            info!("Sending BUY order for {amount:?} at limit price {limit_price:?}");

            let url = app_state.url();
            let result = rt.block_on(send_order::call_send_order(
                url,
                1,
                amount,
                Some(limit_price),
            ));

            info!("SendOrder result: {result:?}");
            info!("Order sent");
        }
        ReplCommand::Sell {
            amount,
            limit_price,
            matching_order_ids: _,
        } => {
            let mut rl = Reedline::create();
            let limit_price = limit_price.unwrap_or_else(|| {
                let price = read_input(&mut rl, "At what price? ");
                let limit: u64 = price.parse::<u64>().unwrap();
                limit
            });

            info!("Sending SELL order for {amount:?} at limit price {limit_price:?}");

            let url = app_state.url();
            let result = rt.block_on(send_order::call_send_order(
                url,
                2,
                amount,
                Some(limit_price),
            ));

            info!("SendOrder result: {result:?}");
            info!("Order sent");
        }
        ReplCommand::GetOrders => {
            info!("Getting orders...");
            info!("TODO: Implement this");
        }
        ReplCommand::CancelOrder { order_id } => {
            info!("Order canceled: {order_id:?}");
            info!("TODO: Implement this");
        }
        ReplCommand::Balance => {
            info!("Getting balance");
            let error_val = Uint::from(99999);
            let base_chain_rpc_url = std::env::var("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address =
                std::env::var("BASE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();
            let quote_chain_rpc_url = std::env::var("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address =
                std::env::var("QUOTE_CHAIN_USDC_TOKEN_ADDRESS").unwrap();

            let base_wallet_balance = rt
                .block_on(balance::call_get_erc20_balance(
                    NamedChain::BaseGoerli,
                    &base_chain_rpc_url,
                    &base_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let base_available_balance = rt
                .block_on(balance::call_get_balance(
                    NamedChain::BaseGoerli,
                    &base_chain_rpc_url,
                    &base_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let base_locked_balance = rt
                .block_on(balance::call_get_locked_balance(
                    NamedChain::BaseGoerli,
                    &base_chain_rpc_url,
                    &base_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let quote_wallet_balance = rt
                .block_on(balance::call_get_erc20_balance(
                    NamedChain::BaseSepolia,
                    &quote_chain_rpc_url,
                    &quote_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let quote_available_balance = rt
                .block_on(balance::call_get_balance(
                    NamedChain::BaseSepolia,
                    &quote_chain_rpc_url,
                    &quote_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let quote_locked_balance = rt
                .block_on(balance::call_get_locked_balance(
                    NamedChain::BaseSepolia,
                    &quote_chain_rpc_url,
                    &quote_chain_usdc_token_address,
                ))
                .unwrap_or(error_val);

            let balance_table = balance::balance_table(
                vec!["USDC", "Base Chain", "Quote Chain"],
                base_wallet_balance,
                base_available_balance,
                base_locked_balance,
                quote_wallet_balance,
                quote_available_balance,
                quote_locked_balance,
            );
            if base_wallet_balance.eq(&error_val)
                | base_available_balance.eq(&error_val)
                | base_locked_balance.eq(&error_val)
                | quote_wallet_balance.eq(&error_val)
                | quote_available_balance.eq(&error_val)
                | quote_locked_balance.eq(&error_val)
            {
                info!("** A '99999' value represents an error in fetching the actual value");
            }

            info!("\n{balance_table}");
        }
        ReplCommand::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
        ReplCommand::Quit => {
            info!("goodbye");
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
