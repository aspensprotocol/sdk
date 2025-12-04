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
    /// The Aspens stack URL (overrides ASPENS_MARKET_STACK_URL from .env)
    #[arg(short = 's', long = "stack")]
    stack_url: Option<url::Url>,
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
    /// Send a market BUY order (executes at best available price)
    BuyMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
    },
    /// Send a limit BUY order (executes at specified price or better)
    BuyLimit {
        /// Market ID to trade on
        market: String,
        /// Amount to buy
        amount: String,
        /// Limit price for the order
        price: String,
    },
    /// Send a market SELL order (executes at best available price)
    SellMarket {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
    },
    /// Send a limit SELL order (executes at specified price or better)
    SellLimit {
        /// Market ID to trade on
        market: String,
        /// Amount to sell
        amount: String,
        /// Limit price for the order
        price: String,
    },
    /// Fetch the current balances for all supported tokens across all chains
    Balance,
    /// Show current configuration and connection status
    Status,
    /// Get the public key and address for the trader wallet
    TraderPublicKey,
    /// Get the signer public key(s) for the trading instance
    SignerPublicKey {
        /// Optional chain ID to filter by. If not provided, returns all chains.
        #[arg(long)]
        chain_id: Option<u32>,
    },
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
    let mut builder = AspensClient::builder();
    if let Some(url) = cli.stack_url {
        builder = builder
            .with_url(url.to_string())
            .expect("Invalid stack URL");
    }
    let client = builder.build().expect("Failed to build AspensClient");

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
                        if let Err(e) = executor
                            .execute(config::download_config(stack_url.clone(), path.clone()))
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

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            if let Err(e) = executor.execute(deposit::call_deposit_from_config(
                network, token, amount, privkey, config,
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

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            if let Err(e) = executor.execute(withdraw::call_withdraw_from_config(
                network, token, amount, privkey, config,
            )) {
                info!("Failed to withdraw: {e:?}");
                info!("Hint: Check your balance with the 'balance' command");
                info!("Hint: Verify server connection and configuration");
            } else {
                info!("Withdraw successful");
            }
        }
        ReplCommand::BuyMarket { market, amount } => {
            info!("Sending market BUY order for {amount} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                1, // Buy side
                amount,
                None, // No limit price (market order)
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!("✓ Market buy order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send market buy order: {e:?}");
                    info!("Hint: Check your balance with the 'balance' command");
                    info!("Hint: Ensure you have sufficient quote token for the buy");
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::BuyLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit BUY order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                1, // Buy side
                amount,
                Some(price),
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!("✓ Limit buy order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send limit buy order: {e:?}");
                    info!("Hint: Check your balance with the 'balance' command");
                    info!("Hint: Ensure you have sufficient quote token for the buy");
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::SellMarket { market, amount } => {
            info!("Sending market SELL order for {amount} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                2, // Sell side
                amount,
                None, // No limit price (market order)
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!("✓ Market sell order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send market sell order: {e:?}");
                    info!("Hint: Check your balance with the 'balance' command");
                    info!("Hint: Ensure you have sufficient base token for the sell");
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::SellLimit {
            market,
            amount,
            price,
        } => {
            info!("Sending limit SELL order for {amount} at price {price} on market {market}");

            // Fetch configuration from server
            let config = match app_state.get_config_sync() {
                Ok(cfg) => cfg,
                Err(e) => {
                    info!("Failed to fetch config: {e:?}");
                    info!("Hint: Ensure the Aspens server is running and accessible");
                    return;
                }
            };

            let privkey = match app_state.get_env("TRADER_PRIVKEY") {
                Some(key) => key,
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                    return;
                }
            };

            match executor.execute(send_order::call_send_order_from_config(
                app_state.stack_url(),
                market,
                2, // Sell side
                amount,
                Some(price),
                privkey,
                config,
            )) {
                Ok(result) => {
                    info!("✓ Limit sell order sent successfully");
                    if !result.transaction_hashes.is_empty() {
                        info!("Transaction hashes:");
                        for formatted_hash in result.get_formatted_transaction_hashes() {
                            info!("  {}", formatted_hash);
                        }
                        info!("💡 Paste these hashes into your chain's block explorer");
                    }
                }
                Err(e) => {
                    info!("Failed to send limit sell order: {e:?}");
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
                    let privkey = app_state.get_env("TRADER_PRIVKEY").unwrap();
                    if let Err(e) = executor.execute(balance::balance_from_config(config, privkey))
                    {
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
            info!("  Server URL: {}", app_state.stack_url());
        }
        ReplCommand::TraderPublicKey => {
            use alloy::signers::local::PrivateKeySigner;

            match app_state.get_env("TRADER_PRIVKEY") {
                Some(privkey) => match privkey.parse::<PrivateKeySigner>() {
                    Ok(signer) => {
                        let address = signer.address();
                        let pubkey = signer.credential().verifying_key();

                        println!("Trader Wallet:");
                        println!("  Address:    {}", address);
                        println!(
                            "  Public Key: 0x{}",
                            hex::encode(pubkey.to_encoded_point(false).as_bytes())
                        );
                    }
                    Err(e) => {
                        info!("Failed to parse TRADER_PRIVKEY: {e:?}");
                    }
                },
                None => {
                    info!("TRADER_PRIVKEY not found in environment");
                }
            }
        }
        ReplCommand::SignerPublicKey { chain_id } => {
            use aspens::commands::config;

            let stack_url = app_state.stack_url();
            info!("Fetching signer public key(s) from {}", stack_url);
            match executor.execute(config::get_signer_public_key(stack_url, chain_id)) {
                Ok(response) => {
                    println!("Signer Public Keys:");
                    for (id, key_info) in response.chain_keys.iter() {
                        println!(
                            "  Chain {} ({}): {}",
                            id, key_info.chain_network, key_info.public_key
                        );
                    }
                }
                Err(e) => {
                    info!("Failed to fetch signer public key(s): {e:?}");
                    info!("Hint: Verify server connection with 'status' command");
                }
            }
        }
        ReplCommand::Quit => {
            info!("goodbye");
            std::process::exit(0)
        }
    });
}
