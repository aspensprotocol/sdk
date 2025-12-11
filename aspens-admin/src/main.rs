//! Aspens Admin CLI
//!
//! Administrative command-line interface for managing Aspens Market Stacks  configuration.
//! Requires authentication via EIP-712 signature to perform admin operations.

use aspens::commands::admin::{self, Chain, CreateInstanceParams, SetMarketParams, Token};
use aspens::commands::auth;
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eyre::Result;
use std::collections::HashMap;
use std::process::ExitCode;
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

/// Analyze an error and return a user-friendly message with hints
fn format_error(err: &eyre::Report, context: &str) -> String {
    let err_string = err.to_string().to_lowercase();
    let root_cause = err.root_cause().to_string().to_lowercase();

    // Connection errors
    if err_string.contains("failed to connect")
        || err_string.contains("connection refused")
        || root_cause.contains("connection refused")
    {
        return format!(
            "Failed to {}: Could not connect to the server\n\n\
             Possible causes:\n\
             - The Aspens server is not running\n\
             - The server URL is incorrect\n\
             - A firewall is blocking the connection\n\n\
             Hints:\n\
             - Check that the server is running\n\
             - Verify the stack URL with 'aspens-admin status'\n\
             - Current URL: Check your ASPENS_MARKET_STACK_URL or --stack flag",
            context
        );
    }

    // DNS/hostname resolution errors
    if err_string.contains("dns error")
        || err_string.contains("no such host")
        || err_string.contains("name or service not known")
        || root_cause.contains("dns")
    {
        return format!(
            "Failed to {}: Could not resolve server hostname\n\n\
             Possible causes:\n\
             - The server hostname is incorrect\n\
             - DNS is not configured properly\n\
             - No internet connection\n\n\
             Hints:\n\
             - Verify the stack URL is correct\n\
             - Check your internet connection\n\
             - Try using an IP address instead of hostname",
            context
        );
    }

    // TLS/SSL errors
    if err_string.contains("tls")
        || err_string.contains("ssl")
        || err_string.contains("certificate")
        || root_cause.contains("certificate")
    {
        return format!(
            "Failed to {}: TLS/SSL error\n\n\
             Possible causes:\n\
             - The server's SSL certificate is invalid or expired\n\
             - Certificate chain is incomplete\n\
             - Using HTTP URL for HTTPS server or vice versa\n\n\
             Hints:\n\
             - Verify you're using the correct protocol (http:// vs https://)\n\
             - Check if the server's certificate is valid\n\
             - For local development, use http://localhost:50051",
            context
        );
    }

    // Authentication errors
    if err_string.contains("unauthenticated")
        || err_string.contains("unauthorized")
        || err_string.contains("401")
        || err_string.contains("invalid token")
        || err_string.contains("token expired")
    {
        return format!(
            "Failed to {}: Authentication failed\n\n\
             Possible causes:\n\
             - JWT token is missing, invalid, or expired\n\
             - You don't have admin privileges\n\n\
             Hints:\n\
             - Run 'aspens-admin login' to get a fresh JWT token\n\
             - Set ASPENS_JWT in your .env file or use --jwt flag\n\
             - Verify ADMIN_PRIVKEY is set correctly",
            context
        );
    }

    // Not authorized as admin (specific login error)
    if err_string.contains("not authorized as an admin")
        || err_string.contains("address is not authorized")
    {
        return format!(
            "Failed to {}: Address is not authorized as admin\n\n\
             The wallet address derived from ADMIN_PRIVKEY is not registered as an admin\n\
             on this Aspens server.\n\n\
             Possible causes:\n\
             - Using the wrong private key (not the admin wallet)\n\
             - The admin address was changed on the server\n\
             - This is a fresh server and admin hasn't been initialized\n\n\
             Hints:\n\
             - Run 'aspens-admin admin-public-key' to see your wallet address\n\
             - Compare with the registered admin address on the server\n\
             - If this is a new server, use 'aspens-admin init-admin --address <your-address>'\n\
             - Check that ADMIN_PRIVKEY in .env matches the expected admin wallet",
            context
        );
    }

    // Permission errors
    if err_string.contains("permission denied")
        || err_string.contains("forbidden")
        || err_string.contains("403")
    {
        return format!(
            "Failed to {}: Permission denied\n\n\
             Possible causes:\n\
             - Your account doesn't have admin privileges\n\
             - The operation requires a different permission level\n\n\
             Hints:\n\
             - Verify you are using the correct admin wallet\n\
             - Contact the system administrator",
            context
        );
    }

    // Already exists errors
    if err_string.contains("already exists") || err_string.contains("duplicate") {
        return format!(
            "Failed to {}: Resource already exists\n\n\
             Hints:\n\
             - Use the appropriate delete command first if you want to replace it\n\
             - Check existing configuration with the config command",
            context
        );
    }

    // Not found errors
    if err_string.contains("not found") || err_string.contains("404") {
        return format!(
            "Failed to {}: Resource not found\n\n\
             Hints:\n\
             - Verify the resource name/ID is correct\n\
             - Check existing configuration with the config command\n\
             - The resource may have been deleted",
            context
        );
    }

    // Admin already initialized
    if err_string.contains("admin already") || err_string.contains("already initialized") {
        return format!(
            "Failed to {}: Admin has already been initialized\n\n\
             Hints:\n\
             - Use 'aspens-admin login' to authenticate with the existing admin\n\
             - Use 'aspens-admin update-admin' to change the admin address (requires auth)",
            context
        );
    }

    // Invalid address format
    if err_string.contains("invalid address") || err_string.contains("invalid checksum") {
        return format!(
            "Failed to {}: Invalid Ethereum address format\n\n\
             Hints:\n\
             - Ensure the address starts with '0x'\n\
             - Verify the address is 42 characters long (including '0x')\n\
             - Use a checksummed address format",
            context
        );
    }

    // Private key errors
    if err_string.contains("invalid private key")
        || err_string.contains("privkey")
        || err_string.contains("secret key")
    {
        return format!(
            "Failed to {}: Invalid private key\n\n\
             Hints:\n\
             - Ensure ADMIN_PRIVKEY is set correctly in your .env file\n\
             - The private key should be a 64-character hex string\n\
             - Do not include the '0x' prefix for private keys",
            context
        );
    }

    // Timeout errors
    if err_string.contains("timeout") || err_string.contains("timed out") {
        return format!(
            "Failed to {}: Request timed out\n\n\
             Possible causes:\n\
             - The server is overloaded or unresponsive\n\
             - Network latency is too high\n\
             - The operation is taking longer than expected\n\n\
             Hints:\n\
             - Try again in a few moments\n\
             - Check server status with 'aspens-admin status'\n\
             - Verify network connectivity",
            context
        );
    }

    // Protocol/compression errors (like the original issue)
    if err_string.contains("compression flag")
        || err_string.contains("protocol error")
        || err_string.contains("invalid compression")
    {
        return format!(
            "Failed to {}: Protocol mismatch\n\n\
             Possible causes:\n\
             - Using HTTP to connect to an HTTPS server\n\
             - Using HTTPS to connect to an HTTP server\n\
             - The server is not a gRPC endpoint\n\n\
             Hints:\n\
             - Check if the URL uses the correct protocol (http:// vs https://)\n\
             - For remote servers, use https://\n\
             - For local development, use http://",
            context
        );
    }

    // Generic fallback with the original error
    format!(
        "Failed to {}\n\n\
         Error: {}\n\n\
         Hints:\n\
         - Check server status with 'aspens-admin status'\n\
         - Verify your configuration in .env file\n\
         - Use -v flag for more detailed output",
        context, err
    )
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

        /// Factory contract address
        #[arg(long)]
        factory_address: String,

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

        /// Fee percentage for the trading instance (uint16: 0-65535)
        /// Represents the fee in basis points (e.g., 100 = 1%)
        #[arg(long, default_value = "0")]
        fees: u16,
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

    /// Get the public key and address for the admin wallet (from ADMIN_PRIVKEY)
    AdminPublicKey,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
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

    if let Some(ref url) = cli.stack_url {
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
                    "JWT token required\n\n\
                     Hints:\n\
                     - Run 'aspens-admin login' to authenticate and get a JWT token\n\
                     - Set ASPENS_JWT in your .env file\n\
                     - Use the --jwt flag to provide a token directly"
                )
            })
    };

    match cli.command {
        // ====================================================================
        // Authentication Commands
        // ====================================================================
        Commands::InitAdmin { address } => {
            info!("Initializing admin with address: {}", address);
            let result = executor
                .execute(auth::initialize_admin(stack_url, address))
                .map_err(|e| eyre::eyre!(format_error(&e, "initialize admin")))?;
            println!("Admin initialized successfully!");
            println!("JWT Token: {}", result.jwt_token);
            println!("Expires at: {}", format_expiry(result.expires_at));
            println!("Address: {}", result.address);
            println!("\nTo use this token, set ASPENS_JWT environment variable or use --jwt flag");
        }

        Commands::Login { chain_id } => {
            use alloy::signers::local::PrivateKeySigner;

            let privkey = client.get_env("ADMIN_PRIVKEY").ok_or_else(|| {
                eyre::eyre!(
                    "ADMIN_PRIVKEY not found\n\n\
                     Hints:\n\
                     - Set ADMIN_PRIVKEY in your .env file\n\
                     - The private key should be a 64-character hex string (without 0x prefix)\n\
                     - This should be the private key for the admin wallet"
                )
            })?;

            // Parse the private key to show the derived address
            let signer: PrivateKeySigner = privkey.parse().map_err(|e| {
                eyre::eyre!(
                    "Invalid ADMIN_PRIVKEY format\n\n\
                     Error: {}\n\n\
                     Hints:\n\
                     - The private key should be a 64-character hex string\n\
                     - Do not include the '0x' prefix\n\
                     - Check for extra whitespace or newlines",
                    e
                )
            })?;
            let address = signer.address();

            info!("Authenticating with EIP-712 signature...");
            info!("  Wallet address: {}", address);

            let result = executor
                .execute(auth::authenticate_with_signature(
                    stack_url.clone(),
                    privkey.clone(),
                    Some(chain_id),
                ))
                .map_err(|e| {
                    // Include the address in the error context for better debugging
                    let err_msg = format_error(&e, "authenticate");
                    if err_msg.contains("not authorized as admin") {
                        eyre::eyre!(
                            "{}\n\n\
                             Your wallet address: {}",
                            err_msg,
                            address
                        )
                    } else {
                        eyre::eyre!(err_msg)
                    }
                })?;

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
            let result = executor
                .execute(admin::update_admin(stack_url.clone(), jwt, address))
                .map_err(|e| eyre::eyre!(format_error(&e, "update admin")))?;
            println!("Admin updated successfully to: {}", result.admin_address);
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
            factory_address,
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
                factory_address,
                permit2_address,
                trade_contract: None,
                tokens: HashMap::new(),
            };

            let result = executor
                .execute(admin::set_chain(stack_url.clone(), jwt, chain))
                .map_err(|e| eyre::eyre!(format_error(&e, &format!("set chain '{}'", network))))?;
            if result.success {
                println!("Chain '{}' set successfully!", network);
            } else {
                return Err(eyre::eyre!(
                    "Failed to set chain '{}'\n\n\
                     The server returned success=false. This may indicate:\n\
                     - Invalid chain configuration\n\
                     - A conflict with existing configuration\n\n\
                     Hints:\n\
                     - Check the server logs for more details\n\
                     - Verify all chain parameters are correct",
                    network
                ));
            }
        }

        Commands::DeleteChain { network } => {
            let jwt = get_jwt()?;
            info!("Deleting chain: {}", network);
            let result = executor
                .execute(admin::delete_chain(stack_url.clone(), jwt, network.clone()))
                .map_err(|e| {
                    eyre::eyre!(format_error(&e, &format!("delete chain '{}'", network)))
                })?;
            if result.success {
                println!("Chain '{}' deleted successfully!", network);
            } else {
                return Err(eyre::eyre!(
                    "Failed to delete chain '{}'\n\n\
                     Hints:\n\
                     - Verify the chain network name is correct\n\
                     - The chain may not exist or may have dependent resources",
                    network
                ));
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
            };

            let result = executor
                .execute(admin::set_token(
                    stack_url.clone(),
                    jwt,
                    network.clone(),
                    token,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("set token '{}' on '{}'", symbol, network)
                    ))
                })?;
            if result.success {
                println!("Token '{}' set on '{}' successfully!", symbol, network);
            } else {
                return Err(eyre::eyre!(
                    "Failed to set token '{}' on '{}'\n\n\
                     Hints:\n\
                     - Verify the chain '{}' exists\n\
                     - Check the token address is valid\n\
                     - Ensure decimals value is correct for this token",
                    symbol,
                    network,
                    network
                ));
            }
        }

        Commands::DeleteToken { network, symbol } => {
            let jwt = get_jwt()?;
            info!("Deleting token {} from {}", symbol, network);
            let result = executor
                .execute(admin::delete_token(
                    stack_url.clone(),
                    jwt,
                    network.clone(),
                    symbol.clone(),
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("delete token '{}' from '{}'", symbol, network)
                    ))
                })?;
            if result.success {
                println!(
                    "Token '{}' deleted from '{}' successfully!",
                    symbol, network
                );
            } else {
                return Err(eyre::eyre!(
                    "Failed to delete token '{}' from '{}'\n\n\
                     Hints:\n\
                     - Verify the token symbol is correct\n\
                     - Check that the token exists on this chain\n\
                     - The token may be used by active markets",
                    symbol,
                    network
                ));
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
            let market_name = format!("{}/{}", base_symbol, quote_symbol);
            info!(
                "Setting market: {} ({}/{})",
                market_name, base_network, quote_network
            );

            let params = SetMarketParams {
                base_chain_network: base_network.clone(),
                quote_chain_network: quote_network.clone(),
                base_chain_token_symbol: base_symbol.clone(),
                quote_chain_token_symbol: quote_symbol.clone(),
                base_chain_token_address: base_address,
                quote_chain_token_address: quote_address,
                base_chain_token_decimals: base_decimals,
                quote_chain_token_decimals: quote_decimals,
                pair_decimals,
            };

            let result = executor
                .execute(admin::set_market(stack_url.clone(), jwt, params))
                .map_err(|e| {
                    eyre::eyre!(format_error(&e, &format!("set market '{}'", market_name)))
                })?;
            if result.success {
                println!("Market '{}' set successfully!", market_name);
            } else {
                return Err(eyre::eyre!(
                    "Failed to set market '{}'\n\n\
                     Hints:\n\
                     - Verify both chains '{}' and '{}' exist\n\
                     - Check that tokens '{}' and '{}' are configured on their respective chains\n\
                     - Ensure token addresses and decimals are correct",
                    market_name,
                    base_network,
                    quote_network,
                    base_symbol,
                    quote_symbol
                ));
            }
        }

        Commands::DeleteMarket { market_id } => {
            let jwt = get_jwt()?;
            info!("Deleting market: {}", market_id);
            let result = executor
                .execute(admin::delete_market(
                    stack_url.clone(),
                    jwt,
                    market_id.clone(),
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(&e, &format!("delete market '{}'", market_id)))
                })?;
            if result.success {
                println!("Market '{}' deleted successfully!", market_id);
            } else {
                return Err(eyre::eyre!(
                    "Failed to delete market '{}'\n\n\
                     Hints:\n\
                     - Verify the market ID is correct\n\
                     - Check existing markets with the config command\n\
                     - The market may have active orders",
                    market_id
                ));
            }
        }

        // ====================================================================
        // Contract Commands
        // ====================================================================
        Commands::DeployContract { network, fees } => {
            let jwt = get_jwt()?;

            // Get the admin private key for signing the transaction
            let privkey = client.get_env("ADMIN_PRIVKEY").ok_or_else(|| {
                eyre::eyre!(
                    "ADMIN_PRIVKEY not found\n\n\
                     This command requires ADMIN_PRIVKEY to sign the deployment transaction.\n\n\
                     Hints:\n\
                     - Set ADMIN_PRIVKEY in your .env file\n\
                     - The private key should be a 64-character hex string (without 0x prefix)\n\
                     - This wallet will pay the gas fees for the deployment"
                )
            })?;

            info!("Fetching deploy calldata from server for: {}", network);

            // Step 1: Get deploy calldata from the server
            let calldata_response = executor
                .execute(admin::get_deploy_calldata(
                    stack_url.clone(),
                    jwt.clone(),
                    network.clone(),
                    fees as u32,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("fetch deploy calldata for '{}'", network)
                    ))
                })?;

            info!(
                "Building createInstance transaction for factory: {}",
                calldata_response.factory_address
            );
            info!(
                "  Instance signer: {}",
                calldata_response.instance_signer_address
            );
            info!("  Fees: {} bps", fees);
            info!("  Chain ID: {}", calldata_response.chain_id);

            // Step 2: Fetch the chain configuration to get the RPC URL
            let config = executor
                .execute(aspens::commands::config::get_config(stack_url.clone()))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("fetch configuration for '{}'", network)
                    ))
                })?;

            let chain = config.get_chain(&network).ok_or_else(|| {
                let available_chains = config
                    .config
                    .as_ref()
                    .map(|c| {
                        c.chains
                            .iter()
                            .map(|ch| ch.network.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                eyre::eyre!(
                    "Chain '{}' not found in configuration\n\n\
                     Available chains: {}\n\n\
                     Hints:\n\
                     - Use 'aspens-admin set-chain' to add the chain first",
                    network,
                    available_chains
                )
            })?;

            // Step 3: Build and sign the createInstance transaction using server-provided calldata
            let params = CreateInstanceParams {
                factory_address: calldata_response.factory_address.clone(),
                calldata: calldata_response.calldata.clone(),
                rpc_url: chain.rpc_url.clone(),
                chain_id: calldata_response.chain_id as u64,
                privkey: privkey.clone(),
            };

            let signed_tx = executor
                .execute(admin::build_create_instance_tx(params))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("build createInstance transaction for '{}'", network)
                    ))
                })?;

            info!(
                "Transaction signed ({} bytes), broadcasting to chain...",
                signed_tx.len()
            );

            // Step 4: Broadcast the transaction to the chain
            let tx_hash = executor
                .execute(admin::broadcast_transaction(
                    chain.rpc_url.clone(),
                    signed_tx,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("broadcast transaction to '{}'", network)
                    ))
                })?;

            info!("Transaction broadcast with hash: {}", tx_hash);

            // Step 5: Send the tx hash to the backend to wait for confirmation and extract contract address
            let result = executor
                .execute(admin::deploy_contract(
                    stack_url.clone(),
                    jwt,
                    network.clone(),
                    tx_hash,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("wait for contract deployment on '{}'", network)
                    ))
                })?;
            println!("Trade contract deployed at: {}", result.contract_address);
        }

        Commands::SetTradeContract { address, chain_id } => {
            let jwt = get_jwt()?;
            info!("Setting trade contract {} on chain {}", address, chain_id);
            let result = executor
                .execute(admin::set_trade_contract(
                    stack_url.clone(),
                    jwt,
                    address.clone(),
                    chain_id,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("set trade contract on chain {}", chain_id)
                    ))
                })?;
            if let Some(tc) = result.trade_contract {
                println!("Trade contract set: {}", tc.address);
            } else {
                println!("Trade contract set successfully");
            }
        }

        Commands::DeleteTradeContract { chain_id } => {
            let jwt = get_jwt()?;
            info!("Deleting trade contract from chain {}", chain_id);
            let result = executor
                .execute(admin::delete_trade_contract(
                    stack_url.clone(),
                    jwt,
                    chain_id,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("delete trade contract from chain {}", chain_id)
                    ))
                })?;
            if result.success {
                println!(
                    "Trade contract deleted from chain {} successfully!",
                    chain_id
                );
            } else {
                return Err(eyre::eyre!(
                    "Failed to delete trade contract from chain {}\n\n\
                     Hints:\n\
                     - Verify the chain ID is correct\n\
                     - Check that a trade contract exists on this chain\n\
                     - The contract may have active trades",
                    chain_id
                ));
            }
        }

        // ====================================================================
        // Info Commands
        // ====================================================================
        Commands::Version => {
            let version = executor
                .execute(admin::get_version(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "get server version")))?;
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
            println!("Configuration Status:");
            println!("  Stack URL: {}", client.stack_url());

            // Ping the gRPC server
            let ping_result = executor.execute(aspens::health::ping_grpc_server(
                client.stack_url().to_string(),
            ));
            if ping_result.success {
                println!(
                    "  Connection: OK ({}ms)",
                    ping_result.latency_ms.unwrap_or(0)
                );
            } else {
                let error_msg = ping_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());

                // Provide friendly hints based on the error
                println!("  Connection: FAILED");
                println!();

                if error_msg.contains("Connection refused") {
                    println!("Could not connect to the server.");
                    println!();
                    println!("Possible causes:");
                    println!("  - The Aspens server is not running");
                    println!("  - The server URL is incorrect");
                    println!("  - A firewall is blocking the connection");
                } else if error_msg.contains("dns") || error_msg.contains("resolve") {
                    println!("Could not resolve the server hostname.");
                    println!();
                    println!("Possible causes:");
                    println!("  - The hostname is incorrect");
                    println!("  - DNS is not configured properly");
                    println!("  - No internet connection");
                } else if error_msg.contains("tls")
                    || error_msg.contains("ssl")
                    || error_msg.contains("certificate")
                {
                    println!("TLS/SSL error: {}", error_msg);
                    println!();
                    println!("Possible causes:");
                    println!("  - Using wrong protocol (http vs https)");
                    println!("  - Server certificate is invalid");
                } else if error_msg.contains("timeout") {
                    println!("Connection timed out.");
                    println!();
                    println!("Possible causes:");
                    println!("  - Server is overloaded or unresponsive");
                    println!("  - Network latency is too high");
                } else {
                    println!("Error: {}", error_msg);
                }

                println!();
                println!("Hints:");
                println!("  - Verify ASPENS_MARKET_STACK_URL in your .env file");
                println!("  - Use --stack flag to specify a different URL");
                println!("  - For local: http://localhost:50051");
                println!("  - For remote: https://your-server:50051");
            }
        }

        Commands::AdminPublicKey => {
            use alloy::signers::local::PrivateKeySigner;

            let privkey = client.get_env("ADMIN_PRIVKEY").ok_or_else(|| {
                eyre::eyre!(
                    "ADMIN_PRIVKEY not found\n\n\
                     Hints:\n\
                     - Set ADMIN_PRIVKEY in your .env file\n\
                     - The private key should be a 64-character hex string (without 0x prefix)\n\
                     - This should be the private key for the admin wallet"
                )
            })?;

            let signer: PrivateKeySigner = privkey.parse().map_err(|e| {
                eyre::eyre!(
                    "Invalid ADMIN_PRIVKEY format\n\n\
                     Error: {}\n\n\
                     Hints:\n\
                     - The private key should be a 64-character hex string\n\
                     - Do not include the '0x' prefix\n\
                     - Check for extra whitespace or newlines",
                    e
                )
            })?;

            let address = signer.address();
            let pubkey = signer.credential().verifying_key();

            println!("Admin Wallet:");
            println!("  Address:    {}", address);
            println!(
                "  Public Key: 0x{}",
                hex::encode(pubkey.to_encoded_point(false).as_bytes())
            );
        }
    }

    Ok(())
}
