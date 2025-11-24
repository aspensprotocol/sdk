use aspens::commands::trading::{balance, deposit, send_order, withdraw};
use aspens::{AspensClient, AsyncExecutor, BlockingExecutor};
use clap::Parser;
use clap_repl::reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory};
use clap_repl::ClapEditor;
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

    fn get_config_sync(
        &self,
    ) -> eyre::Result<aspens::commands::config::config_pb::GetConfigResponse> {
        let guard = self.client.lock().unwrap();
        let url = guard.stack_url().to_string();
        drop(guard); // Release lock before async call

        // Use tokio runtime to block on async operation
        tokio::runtime::Runtime::new()?
            .block_on(async { aspens::commands::config::call_get_config(url).await })
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
    /// Deposit tokens to make them available for trading (requires network, token, amount)
    Deposit {
        /// The network name to deposit to (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to deposit (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to deposit
        amount: u64,
    },
    /// Withdraw tokens to a local wallet (requires network, token, amount)
    Withdraw {
        /// The network name to withdraw from (e.g., anvil-1, base-sepolia)
        network: String,
        /// Token symbol to withdraw (e.g., USDC, WETH, WBTC)
        token: String,
        /// Amount to withdraw
        amount: u64,
    },
    /// Send a BUY order with amount and optional limit price
    Buy {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
    },
    /// Send a SELL order with amount and optional limit price
    Sell {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
        /// Optional limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
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
            network,
            token,
            amount,
        } => {
            info!("Depositing {amount} {token} on {network}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("EVM_TESTNET_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("EVM_TESTNET_PRIVKEY not found in environment");
                    return;
                }
            };

            if let Err(e) = executor.execute(deposit::call_deposit_from_config(
                network,
                token,
                amount,
                privkey,
                config,
            )) {
                info!("Failed to deposit: {e:?}");
                info!("Hint: Check your balance with the 'balance' command");
                info!("Hint: Verify server connection and configuration");
                info!("Hint: Ensure you have sufficient token balance in your wallet");
            } else {
                info!("Deposit successful");
            }
        }
        ReplCommand::Withdraw {
            network,
            token,
            amount,
        } => {
            info!("Withdrawing {amount} {token} from {network}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("EVM_TESTNET_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("EVM_TESTNET_PRIVKEY not found in environment");
                    return;
                }
            };

            if let Err(e) = executor.execute(withdraw::call_withdraw_from_config(
                network,
                token,
                amount,
                privkey,
                config,
            )) {
                info!("Failed to withdraw: {e:?}");
                info!("Hint: Check your balance with the 'balance' command");
                info!("Hint: Verify server connection and configuration");
            } else {
                info!("Withdraw successful");
            }
        }
        ReplCommand::Buy {
            market,
            amount,
            limit_price,
        } => {
            info!("Sending BUY order for {amount:?} at limit price {limit_price:?} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("EVM_TESTNET_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("EVM_TESTNET_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                1, // Buy side
                amount,
                limit_price,
                privkey,
                config,
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
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::Sell {
            market,
            amount,
            limit_price,
        } => {
            info!("Sending SELL order for {amount:?} at limit price {limit_price:?} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("EVM_TESTNET_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("EVM_TESTNET_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                2, // Sell side
                amount,
                limit_price,
                privkey,
                config,
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
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::Balance => {
            use aspens::commands::config;

            info!("Fetching balances for all tokens across all chains");
            let stack_url = app_state.stack_url();
            match executor.execute(config::get_config(stack_url.clone())) {
                Ok(config) => {
                    let privkey = app_state.get_env("EVM_TESTNET_PRIVKEY").unwrap();
                    if let Err(e) = executor.execute(balance::balance_from_config(config, privkey)) {
                        info!("Failed to get balances: {e:?}");
                        info!("Hint: Check your RPC URLs with 'status' command");
                        info!("Hint: Ensure your private key is correctly configured");
                        info!("Hint: Verify the contract addresses are correct");
                    }
                }
                Err(e) => {
                    info!("Failed to fetch configuration: {e:?}");
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::Status => {
            info!("Configuration Status:");
            info!("  Environment: {}", app_state.client.lock().unwrap().environment());
            info!("  Server URL: {}", app_state.stack_url());
        }
        ReplCommand::Quit => {
            info!("goodbye");
            std::process::exit(0)
        }
    });
}
