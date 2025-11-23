use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor};
use clap::Parser;
use clap_repl::reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory};
use clap_repl::ClapEditor;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

struct AppState {
    client: Arc<Mutex<AspensClient>>,
}

impl AppState {
    fn new(client: AspensClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }

    fn stack_url(&self) -> String {
        let guard = self.client.lock().unwrap();
        guard.stack_url().to_string()
    }

    fn get_env(&self, key: &str) -> Option<String> {
        let guard = self.client.lock().unwrap();
        guard.get_env(key).cloned()
    }

    fn resolve_token_address(&self, chain: &str, token: &str) -> eyre::Result<String> {
        let guard = self.client.lock().unwrap();
        guard.resolve_token_address(chain, token)
    }

    fn get_chain_rpc_url(&self, chain: &str) -> eyre::Result<String> {
        let guard = self.client.lock().unwrap();
        guard.get_chain_rpc_url(chain)
    }

    fn get_chain_contract_address(&self, chain: &str) -> eyre::Result<String> {
        let guard = self.client.lock().unwrap();
        guard.get_chain_contract_address(chain)
    }
}

#[derive(Debug, Parser)]
#[command(name = "aspens-repl")]
#[command(about = "Aspens REPL for interactive trading operations")]
struct ReplCli {
    /// Environment configuration to use
    #[arg(short, long, default_value = "anvil")]
    env: String,
}

#[derive(Debug, Parser)]
#[command(name = "", author, version, about, long_about = None)]
enum ReplCommand {
    /// Fetch and display the configuration from the server
    Config {
        /// Optional path to save the configuration file (supports .json or .toml)
        #[arg(short, long)]
        output_file: Option<String>,
    },
    /// Deposit tokens to make them available for trading (requires chain, token, amount)
    Deposit {
        /// The chain network to deposit to
        chain: String,
        /// Token symbol to deposit (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to deposit
        amount: u64,
    },
    /// Withdraw tokens to a local wallet (requires chain, token, amount)
    Withdraw {
        /// The chain network to withdraw from
        chain: String,
        /// Token symbol to withdraw (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to withdraw
        amount: u64,
    },
    /// Send a BUY order with amount and optional limit price
    Buy {
        /// Amount to buy
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
        /// Market ID to trade on (defaults to MARKET_ID_1 from environment)
        #[arg(short, long)]
        market: Option<String>,
    },
    /// Send a SELL order with amount and optional limit price
    Sell {
        /// Amount to sell
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
        /// Market ID to trade on (defaults to MARKET_ID_1 from environment)
        #[arg(short, long)]
        market: Option<String>,
    },
    /// Fetch the current balances across all chains
    Balance,
    /// Show current configuration and connection status
    Status,
    /// Quit the REPL
    Quit,
}

fn main() {
    let cli = ReplCli::parse();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    // Build the client
    let client = AspensClient::builder()
        .with_environment(&cli.env)
        .build()
        .expect("Failed to build AspensClient");

    let app_state = AppState::new(client);
    let executor = BlockingExecutor::new();

    let prompt = DefaultPrompt {
        left_prompt: DefaultPromptSegment::Basic("aspens".to_owned()),
        ..DefaultPrompt::default()
    };

    let rl = ClapEditor::<ReplCommand>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            reed.with_history(Box::new(
                FileBackedHistory::with_file(10000, "/tmp/aspens-repl-history".into()).unwrap(),
            ))
        })
        .build();

    rl.repl(|command| match command {
        ReplCommand::Config { output_file } => {
            use aspens::commands::config;

            let stack_url = app_state.stack_url();
            info!("Fetching configuration from {}", stack_url);
            match executor.execute(config::get_config(stack_url.clone())) {
                Ok(config) => {
                    // If output_file is provided, save to file
                    if let Some(path) = output_file {
                        if let Err(e) =
                            executor.execute(config::download_config(stack_url.clone(), path.clone()))
                        {
                            info!("Failed to save configuration: {e:?}");
                        } else {
                            info!("Configuration saved to: {}", path);
                        }
                    } else {
                        // Display config as JSON
                        match serde_json::to_string_pretty(&config) {
                            Ok(json) => println!("{}", json),
                            Err(e) => info!("Failed to serialize config: {e:?}"),
                        }
                    }
                }
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                }
            }
        }
        ReplCommand::Deposit {
            chain,
            token,
            amount,
        } => {
            info!("Depositing {amount:?} {token:?} on {chain:?}");

            // Resolve chain-specific configuration
            let rpc_url = match app_state.get_chain_rpc_url(&chain) {
                Ok(url) => url,
                Err(e) => {
                    info!("Failed to resolve chain RPC URL: {e:?}");
                    return;
                }
            };
            let contract_address = match app_state.get_chain_contract_address(&chain) {
                Ok(addr) => addr,
                Err(e) => {
                    info!("Failed to resolve chain contract address: {e:?}");
                    return;
                }
            };
            let token_address = match app_state.resolve_token_address(&chain, &token) {
                Ok(addr) => addr,
                Err(e) => {
                    info!("Failed to resolve token address: {e:?}");
                    info!("Hint: Ensure {} has a configured token address for {}", chain, token);
                    return;
                }
            };
            let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();

            let chain_type = alloy_chains::NamedChain::from_str(&chain).unwrap_or_else(|_| {
                info!("Invalid chain name: {}, using BaseGoerli as default", chain);
                alloy_chains::NamedChain::BaseGoerli
            });
            if let Err(e) = executor.execute(deposit::call_deposit(
                chain_type,
                rpc_url,
                token_address,
                contract_address,
                privkey,
                amount,
            )) {
                info!("Failed to deposit: {e:?}");
                info!("Hint: Check your balance with the 'balance' command");
                info!("Hint: Verify server connection with 'initialize'");
                info!("Hint: Ensure you have sufficient token balance in your wallet");
            }
        }
        ReplCommand::Withdraw {
            chain,
            token,
            amount,
        } => {
            info!("Withdrawing {amount:?} {token:?} on {chain:?}");

            // Resolve chain-specific configuration
            let rpc_url = match app_state.get_chain_rpc_url(&chain) {
                Ok(url) => url,
                Err(e) => {
                    info!("Failed to resolve chain RPC URL: {e:?}");
                    return;
                }
            };
            let contract_address = match app_state.get_chain_contract_address(&chain) {
                Ok(addr) => addr,
                Err(e) => {
                    info!("Failed to resolve chain contract address: {e:?}");
                    return;
                }
            };
            let token_address = match app_state.resolve_token_address(&chain, &token) {
                Ok(addr) => addr,
                Err(e) => {
                    info!("Failed to resolve token address: {e:?}");
                    info!("Hint: Ensure {} has a configured token address for {}", chain, token);
                    return;
                }
            };
            let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();

            let chain_type = alloy_chains::NamedChain::from_str(&chain).unwrap_or_else(|_| {
                info!("Invalid chain name: {}, using BaseGoerli as default", chain);
                alloy_chains::NamedChain::BaseGoerli
            });
            if let Err(e) = executor.execute(withdraw::call_withdraw(
                chain_type,
                rpc_url,
                token_address,
                contract_address,
                privkey,
                amount,
            )) {
                info!("Failed to withdraw: {e:?}");
                info!("Hint: Check your available balance with the 'balance' command");
                info!("Hint: Ensure you have sufficient balance in the contract");
                info!("Hint: Verify server connection with 'initialize'");
            }
        }
        ReplCommand::Buy {
            amount,
            limit_price,
            market,
        } => {
            let market_id = market.unwrap_or_else(|| app_state.get_env("MARKET_ID_1").unwrap());
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?} on market {market_id}");
            let pubkey = app_state.get_env("EVM_TESTNET_PUBKEY").unwrap();
            let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();

            match executor.execute(send_order::call_send_order(
                app_state.stack_url(),
                1, // Buy side
                amount,
                limit_price,
                market_id,
                pubkey.clone(),
                pubkey,
                privkey,
            )) {
                Ok(result) => {
                    info!("✓ Buy order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send buy order: {e:?}");
                    info!("Hint: Check your balance with the 'balance' command");
                    info!("Hint: Ensure you have sufficient quote token for the buy");
                    info!("Hint: Verify server connection with 'initialize'");
                    info!("Hint: Check market status with 'status' command");
                }
            }
        }
        ReplCommand::Sell {
            amount,
            limit_price,
            market,
        } => {
            let market_id = market.unwrap_or_else(|| app_state.get_env("MARKET_ID_1").unwrap());
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?} on market {market_id}");
            let pubkey = app_state.get_env("EVM_TESTNET_PUBKEY").unwrap();
            let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();

            match executor.execute(send_order::call_send_order(
                app_state.stack_url(),
                2, // Sell side
                amount,
                limit_price,
                market_id,
                pubkey.clone(),
                pubkey,
                privkey,
            )) {
                Ok(result) => {
                    info!("✓ Sell order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send sell order: {e:?}");
                    info!("Hint: Check your balance with the 'balance' command");
                    info!("Hint: Ensure you have sufficient base token for the sell");
                    info!("Hint: Verify server connection with 'initialize'");
                    info!("Hint: Check market status with 'status' command");
                }
            }
        }
        ReplCommand::Balance => {
            info!("Getting balance");
            let base_chain_rpc_url = app_state.get_env("BASE_CHAIN_RPC_URL").unwrap();
            let base_chain_usdc_token_address = app_state
                .get_env("BASE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap();
            let quote_chain_rpc_url = app_state.get_env("QUOTE_CHAIN_RPC_URL").unwrap();
            let quote_chain_usdc_token_address = app_state
                .get_env("QUOTE_CHAIN_USDC_TOKEN_ADDRESS")
                .unwrap();
            let base_chain_contract_address =
                app_state.get_env("BASE_CHAIN_CONTRACT_ADDRESS").unwrap();
            let quote_chain_contract_address = app_state
                .get_env("QUOTE_CHAIN_CONTRACT_ADDRESS")
                .unwrap();
            let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();

            if let Err(e) = executor.execute(balance::balance(
                base_chain_rpc_url,
                base_chain_usdc_token_address,
                quote_chain_rpc_url,
                quote_chain_usdc_token_address,
                base_chain_contract_address,
                quote_chain_contract_address,
                privkey,
            )) {
                info!("Failed to get balance: {e:?}");
                info!("Hint: Check your RPC URLs with 'status' command");
                info!("Hint: Ensure your private key is correctly configured");
                info!("Hint: Verify the contract addresses are correct");
            }
        }
        ReplCommand::Status => {
            info!("Configuration Status:");
            info!("  Environment: {}", app_state.client.lock().unwrap().environment());
            info!("  Server URL: {}", app_state.stack_url());
            info!("  Market ID 1: {}", app_state.get_env("MARKET_ID_1").unwrap_or("not set".to_string()));
            info!("  Market ID 2: {}", app_state.get_env("MARKET_ID_2").unwrap_or("not set".to_string()));
            info!("  Base Chain RPC: {}", app_state.get_env("BASE_CHAIN_RPC_URL").unwrap_or("not set".to_string()));
            info!("  Quote Chain RPC: {}", app_state.get_env("QUOTE_CHAIN_RPC_URL").unwrap_or("not set".to_string()));
            info!("  Public Key: {}", app_state.get_env("EVM_TESTNET_PUBKEY").unwrap_or("not set".to_string()));
        }
        ReplCommand::Quit => {
            info!("goodbye");
            std::process::exit(0)
        }
    });
}
