//! Aspens Admin CLI
//!
//! Administrative command-line interface for managing Aspens Market Stacks  configuration.
//! Requires authentication via EIP-712 signature to perform admin operations.

use aspens::commands::admin::{self, SetMarketParams, Chain, Token};
use aspens::commands::auth;
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eyre::Result;
use std::collections::HashMap;
use tracing::info;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::FmtSubscriber;
use url::Url;

/// Format a Unix timestamp as a human-readable datetime string
fn format_expiry(timestamp: u64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{} (invalid timestamp)", timestamp))
}

#[derive(Debug, Parser)]
#[command(name = "aspens-admin")]
#[command(about = "Admin CLI for Aspens Markets Stacks configuration")]
#[command(version)]
struct Cli {
    /// The Aspens stack URL
    #[arg(short = 's', long = "stack")]
    stack_url: Option<Url>,

    /// JWT token for authentication (can also be set via ASPENS_JWT env var)
    #[arg(long, env = "ASPENS_JWT")]
    jwt: Option<String>,

    #[command(flatten)]
    verbose: clap_verbosity::Verbosity,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    // ========================================================================
    // Authentication Commands
    // ========================================================================
    /// Initialize the first admin (only works on fresh stack)
    InitAdmin {
        /// Ethereum address to set as initial admin
        #[arg(long)]
        address: String,
    },

    /// Authenticate with EIP-712 signature to obtain JWT token
    Login {
        /// Chain ID for EIP-712 domain (default: 1)
        #[arg(long, default_value = "1")]
        chain_id: u64,
    },

    // ========================================================================
    // Admin Management Commands
    // ========================================================================
    /// Update the admin address
    UpdateAdmin {
        /// New admin Ethereum address
        address: String,
    },

    // ========================================================================
    // Chain Commands
    // ========================================================================
    /// Set a chain in the configuration
    SetChain {
        /// Chain architecture (e.g., "EVM", "Hedera")
        #[arg(long)]
        architecture: String,

        /// Canonical name (e.g., "Base Sepolia")
        #[arg(long)]
        canonical_name: String,

        /// Network identifier (e.g., "base-sepolia")
        #[arg(long)]
        network: String,

        /// Chain ID
        #[arg(long)]
        chain_id: u32,

        /// Instance signer address
        #[arg(long)]
        instance_signer_address: String,

        /// RPC URL for the chain
        #[arg(long)]
        rpc_url: String,

        /// Factory service contract address
        #[arg(long)]
        service_address: String,

        /// Permit2 contract address
        #[arg(long)]
        permit2_address: String,

        /// Optional block explorer URL
        #[arg(long)]
        explorer_url: Option<String>,
    },

    /// Delete a chain from the configuration
    DeleteChain {
        /// Network identifier to delete (e.g., "base-sepolia")
        network: String,
    },

    // ========================================================================
    // Token Commands
    // ========================================================================
    /// Set a token on a chain
    SetToken {
        /// Network to add token to (e.g., "base-sepolia")
        #[arg(long)]
        network: String,

        /// Token name (e.g., "USD Coin")
        #[arg(long)]
        name: String,

        /// Token symbol (e.g., "USDC")
        #[arg(long)]
        symbol: String,

        /// Token contract address
        #[arg(long)]
        address: String,

        /// Token decimals
        #[arg(long)]
        decimals: u32,

        /// Trade precision (decimal places used in trading)
        #[arg(long, default_value = "6")]
        trade_precision: i32,

        /// Optional token ID (for Hedera)
        #[arg(long)]
        token_id: Option<String>,
    },

    /// Delete a token from a chain
    DeleteToken {
        /// Network where token exists
        #[arg(long)]
        network: String,

        /// Token symbol to delete
        #[arg(long)]
        symbol: String,
    },

    // ========================================================================
    // Market Commands
    // ========================================================================
    /// Set a market
    SetMarket {
        /// Base chain network (e.g., "base-sepolia")
        #[arg(long)]
        base_network: String,

        /// Quote chain network (e.g., "op-sepolia")
        #[arg(long)]
        quote_network: String,

        /// Base token symbol (e.g., "USDC")
        #[arg(long)]
        base_symbol: String,

        /// Quote token symbol (e.g., "USDT")
        #[arg(long)]
        quote_symbol: String,

        /// Base token address
        #[arg(long)]
        base_address: String,

        /// Quote token address
        #[arg(long)]
        quote_address: String,

        /// Base token decimals
        #[arg(long)]
        base_decimals: i32,

        /// Quote token decimals
        #[arg(long)]
        quote_decimals: i32,

        /// Pair decimals for trading
        #[arg(long)]
        pair_decimals: i32,
    },

    /// Delete a market
    DeleteMarket {
        /// Market ID to delete
        market_id: String,
    },

    // ========================================================================
    // Contract Commands
    // ========================================================================
    /// Deploy a trade contract on a chain
    DeployContract {
        /// Network to deploy on (e.g., "base-sepolia")
        network: String,
    },

    /// Set a trade contract on a chain
    SetTradeContract {
        /// Contract address
        #[arg(long)]
        address: String,

        /// Chain ID to associate with
        #[arg(long)]
        chain_id: u32,
    },

    /// Delete a trade contract from a chain
    DeleteTradeContract {
        /// Chain ID to remove contract from
        chain_id: u32,
    },

    // ========================================================================
    // Info Commands
    // ========================================================================
    /// Get server version information
    Version,

    /// Show current configuration and connection status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Configure log level - convert from clap-verbosity's log::LevelFilter to tracing's LevelFilter
    let log_level = if cli.verbose.is_silent() {
        LevelFilter::ERROR
    } else {
        // clap-verbosity uses log crate's LevelFilter, convert to tracing's
        match cli.verbose.log_level_filter().as_str() {
            "OFF" => LevelFilter::OFF,
            "ERROR" => LevelFilter::ERROR,
            "WARN" => LevelFilter::WARN,
            "INFO" => LevelFilter::INFO,
            "DEBUG" => LevelFilter::DEBUG,
            "TRACE" => LevelFilter::TRACE,
            _ => LevelFilter::ERROR,
        }
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    // Build the client
    let mut builder = AspensClient::builder();

    if let Some(url) = cli.stack_url {
        builder = builder.with_url(url.to_string())?;
    }

    let client = builder.build()?;
    let executor = DirectExecutor;
    let stack_url = client.stack_url().to_string();

    // Helper to get JWT (from CLI arg, env var, or .env file)
    let get_jwt = || -> Result<String> {
        cli.jwt
            .clone()
            .or_else(|| client.get_env("ASPENS_JWT").cloned())
            .ok_or_else(|| {
                eyre::eyre!(
                    "JWT token required. Use --jwt flag, set ASPENS_JWT in .env, or run 'aspens-admin login' first"
                )
            })
    };

    match cli.command {
        // ====================================================================
        // Authentication Commands
        // ====================================================================
        Commands::InitAdmin { address } => {
            info!("Initializing admin with address: {}", address);
            let result = executor.execute(auth::initialize_admin(stack_url, address))?;
            println!("Admin initialized successfully!");
            println!("JWT Token: {}", result.jwt_token);
            println!("Expires at: {}", format_expiry(result.expires_at));
            println!("Address: {}", result.address);
            println!("\nTo use this token, set ASPENS_JWT environment variable or use --jwt flag");
        }

        Commands::Login { chain_id } => {
            let privkey = client
                .get_env("ADMIN_PRIVKEY")
                .ok_or_else(|| eyre::eyre!("ADMIN_PRIVKEY not found in environment"))?
                .clone();

            info!("Authenticating with EIP-712 signature...");
            let result = executor.execute(auth::authenticate_with_signature(
                stack_url.clone(),
                privkey,
                Some(chain_id),
            ))?;

            println!("Authentication successful!");
            println!("JWT Token: {}", result.jwt_token);
            println!("Expires at: {}", format_expiry(result.expires_at));
            println!("Address: {}", result.address);
            println!("\nTo use this token:");
            println!("  export ASPENS_JWT=\"{}\"", result.jwt_token);
        }

        // ====================================================================
        // Admin Management Commands
        // ====================================================================
        Commands::UpdateAdmin { address } => {
            let jwt = get_jwt()?;
            info!("Updating admin to: {}", address);
            let result =
                executor.execute(admin::update_admin(stack_url.clone(), jwt, address))?;
            println!(
                "Admin updated successfully to: {}",
                result.admin_address
            );
        }

        // ====================================================================
        // Chain Commands
        // ====================================================================
        Commands::SetChain {
            architecture,
            canonical_name,
            network,
            chain_id,
            instance_signer_address,
            rpc_url,
            service_address,
            permit2_address,
            explorer_url,
        } => {
            let jwt = get_jwt()?;
            info!("Setting chain: {} ({})", canonical_name, network);

            let chain = Chain {
                architecture,
                canonical_name,
                network: network.clone(),
                chain_id,
                instance_signer_address,
                explorer_url,
                rpc_url,
                service_address,
                permit2_address,
                trade_contract: None,
                tokens: HashMap::new(),
            };

            let result = executor.execute(admin::set_chain(stack_url.clone(), jwt, chain))?;
            if result.success {
                println!("Chain '{}' set successfully!", network);
            } else {
                println!("Failed to set chain");
            }
        }

        Commands::DeleteChain { network } => {
            let jwt = get_jwt()?;
            info!("Deleting chain: {}", network);
            let result =
                executor.execute(admin::delete_chain(stack_url.clone(), jwt, network.clone()))?;
            if result.success {
                println!("Chain '{}' deleted successfully!", network);
            } else {
                println!("Failed to delete chain");
            }
        }

        // ====================================================================
        // Token Commands
        // ====================================================================
        Commands::SetToken {
            network,
            name,
            symbol,
            address,
            decimals,
            trade_precision,
            token_id,
        } => {
            let jwt = get_jwt()?;
            info!("Setting token {} ({}) on {}", name, symbol, network);

            let token = Token {
                name,
                symbol: symbol.clone(),
                address,
                token_id,
                decimals,
                trade_precision,
            };

            let result = executor.execute(admin::set_token(
                stack_url.clone(),
                jwt,
                network.clone(),
                token,
            ))?;
            if result.success {
                println!("Token '{}' set on '{}' successfully!", symbol, network);
            } else {
                println!("Failed to set token");
            }
        }

        Commands::DeleteToken { network, symbol } => {
            let jwt = get_jwt()?;
            info!("Deleting token {} from {}", symbol, network);
            let result = executor.execute(admin::delete_token(
                stack_url.clone(),
                jwt,
                network.clone(),
                symbol.clone(),
            ))?;
            if result.success {
                println!(
                    "Token '{}' deleted from '{}' successfully!",
                    symbol, network
                );
            } else {
                println!("Failed to delete token");
            }
        }

        // ====================================================================
        // Market Commands
        // ====================================================================
        Commands::SetMarket {
            base_network,
            quote_network,
            base_symbol,
            quote_symbol,
            base_address,
            quote_address,
            base_decimals,
            quote_decimals,
            pair_decimals,
        } => {
            let jwt = get_jwt()?;
            info!(
                "Setting market: {}/{} ({}/{})",
                base_symbol, quote_symbol, base_network, quote_network
            );

            let params = SetMarketParams {
                base_chain_network: base_network,
                quote_chain_network: quote_network,
                base_chain_token_symbol: base_symbol.clone(),
                quote_chain_token_symbol: quote_symbol.clone(),
                base_chain_token_address: base_address,
                quote_chain_token_address: quote_address,
                base_chain_token_decimals: base_decimals,
                quote_chain_token_decimals: quote_decimals,
                pair_decimals,
            };

            let result = executor.execute(admin::set_market(stack_url.clone(), jwt, params))?;
            if result.success {
                println!(
                    "Market '{}/{}' set successfully!",
                    base_symbol, quote_symbol
                );
            } else {
                println!("Failed to set market");
            }
        }

        Commands::DeleteMarket { market_id } => {
            let jwt = get_jwt()?;
            info!("Deleting market: {}", market_id);
            let result = executor.execute(admin::delete_market(
                stack_url.clone(),
                jwt,
                market_id.clone(),
            ))?;
            if result.success {
                println!("Market '{}' deleted successfully!", market_id);
            } else {
                println!("Failed to delete market");
            }
        }

        // ====================================================================
        // Contract Commands
        // ====================================================================
        Commands::DeployContract { network } => {
            let jwt = get_jwt()?;
            info!("Deploying trade contract on: {}", network);
            let result =
                executor.execute(admin::deploy_contract(stack_url.clone(), jwt, network))?;
            println!("Trade contract deployed at: {}", result.contract_address);
        }

        Commands::SetTradeContract { address, chain_id } => {
            let jwt = get_jwt()?;
            info!("Setting trade contract {} on chain {}", address, chain_id);
            let result = executor.execute(admin::set_trade_contract(
                stack_url.clone(),
                jwt,
                address,
                chain_id,
            ))?;
            if let Some(tc) = result.trade_contract {
                println!("Trade contract set: {}", tc.address);
            } else {
                println!("Trade contract set successfully");
            }
        }

        Commands::DeleteTradeContract { chain_id } => {
            let jwt = get_jwt()?;
            info!("Deleting trade contract from chain {}", chain_id);
            let result = executor.execute(admin::delete_trade_contract(
                stack_url.clone(),
                jwt,
                chain_id,
            ))?;
            if result.success {
                println!(
                    "Trade contract deleted from chain {} successfully!",
                    chain_id
                );
            } else {
                println!("Failed to delete trade contract");
            }
        }

        // ====================================================================
        // Info Commands
        // ====================================================================
        Commands::Version => {
            let version = executor.execute(admin::get_version(stack_url.clone()))?;
            println!("Server Version Information:");
            println!("  Version: {}", version.version);
            println!("  Git Commit: {}", version.git_commit_hash);
            println!("  Git Branch: {}", version.git_branch);
            println!("  Commit Date: {}", version.git_commit_date);
            println!("  Build Time: {}", version.build_timestamp);
            println!("  Target: {}", version.target_triple);
            println!("  Rustc: {}", version.rustc_version);
            if !version.cargo_features.is_empty() {
                println!("  Features: {}", version.cargo_features.join(", "));
            }
        }

        Commands::Status => {
            info!("Configuration Status:");
            println!("Stack URL: {}", client.stack_url());
            if client.is_jwt_valid() {
                println!("JWT: Valid");
                if let Some(expiry) = client.get_jwt_expiry() {
                    println!("JWT Expires: {}", format_expiry(expiry));
                }
            } else if cli.jwt.is_some() {
                println!("JWT: Provided (validity not checked until used)");
            } else {
                println!("JWT: Not set");
            }
        }
    }

    Ok(())
}
