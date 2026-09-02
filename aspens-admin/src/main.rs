//! Aspens Admin CLI
//!
//! Administrative command-line interface for managing Aspens Market Stacks  configuration.
//! Requires authentication via EIP-712 signature to perform admin operations.

use aspens::chain_client;
use aspens::commands::admin::{
    self, Chain, CreateInstanceParams, RpcAuthScheme, RpcEndpoint, SetMarketParams, Token,
};
use aspens::commands::auth;
use aspens::commands::config;
use aspens::commands::trading::balance;
use aspens::{AspensClient, AsyncExecutor, DirectExecutor};
use aspens_cliutil::BinaryContext;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use comfy_table::{Table, presets::UTF8_BORDERS_ONLY};
use eyre::Result;
use std::collections::HashMap;
use std::process::ExitCode;
use tracing::info;
use tracing_subscriber::FmtSubscriber;
use tracing_subscriber::filter::LevelFilter;
use url::Url;

/// Format a Unix timestamp as a human-readable datetime string
fn format_expiry(timestamp: u64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{} (invalid timestamp)", timestamp))
}

/// Local thin wrapper over [`aspens_cliutil::format_error`].
fn format_error(err: &eyre::Report, context: &str) -> String {
    aspens_cliutil::format_error(err, context, &BinaryContext::ADMIN)
}

#[derive(Debug, Parser)]
#[command(name = "aspens-admin")]
#[command(about = "Admin CLI for Aspens Markets Stacks configuration")]
#[command(version)]
struct Cli {
    /// The Aspens stack URL
    #[arg(short = 's', long = "stack", global = true)]
    stack_url: Option<Url>,

    /// Path to environment file (defaults to .env in current directory)
    #[arg(short = 'e', long = "env-file", global = true)]
    env_file: Option<String>,

    /// JWT token for authentication (can also be set via ASPENS_JWT in .env file)
    #[arg(long, global = true)]
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

        /// RPC URL for the chain
        #[arg(long)]
        rpc_url: String,

        /// Factory contract address
        #[arg(long)]
        factory_address: String,

        /// Optional block explorer URL
        #[arg(long)]
        explorer_url: Option<String>,

        /// Instance signer address (server-side address of the trade
        /// contract's authorized signer; the SDK's gasless-lock EIP-712
        /// digest uses this as `originSettler`'s arborter field).
        /// Optional — the server has a code path that derives this from
        /// the signer service on first chain registration, but on
        /// subsequent set-chain calls (e.g. after a DB reset where the
        /// signer's persisted keys survive) it's left empty and the
        /// SDK fails with "invalid instance_signer_address". Supply it
        /// explicitly to break that asymmetry; query via
        /// `aspens-cli signer-public-key --chain-network <network>`.
        #[arg(long)]
        instance_signer_address: Option<String>,
    },

    /// Delete a chain from the configuration
    DeleteChain {
        /// Network identifier to delete (e.g., "base-sepolia")
        network: String,
    },

    /// Manage a chain's RPC endpoint set (list / set / probe)
    Rpc {
        #[command(subcommand)]
        action: RpcCommand,
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
    ///
    /// Register both tokens with `set-token` FIRST. The market no longer
    /// carries token decimals — the arborter reads them from the `tokens`
    /// table — and it matches the addresses below against that table
    /// byte-for-byte, case included, rejecting the market when they differ.
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

        /// Base token address — must match the `set-token` row exactly, case included
        #[arg(long)]
        base_address: String,

        /// Quote token address — must match the `set-token` row exactly, case included
        #[arg(long)]
        quote_address: String,

        /// Pair decimals for trading (the market's own scale, not a token's)
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

        /// Chain network to associate with (e.g., "base-sepolia")
        #[arg(long)]
        chain_network: String,
    },

    /// Set an instance's operator fee (recipient + bps). The arborter submits the
    /// on-chain setOperatorFee as the instance's operator_admin.
    SetOperatorFee {
        /// Chain network whose instance to update (e.g., "base-sepolia")
        #[arg(long)]
        chain_network: String,

        /// Operator-fee recipient address (0x-hex EVM / base58 Solana)
        #[arg(long)]
        recipient: String,

        /// Operator fee in basis points
        #[arg(long)]
        bps: u32,
    },

    /// Rotate an instance's operator_admin key. After rotation the new admin
    /// (not the arborter) gates operator-fee changes.
    RotateOperatorAdmin {
        /// Chain network whose instance to update
        #[arg(long)]
        chain_network: String,

        /// The new operator_admin address (0x-hex EVM / base58 Solana)
        #[arg(long)]
        new_admin: String,
    },

    /// Delete a trade contract from a chain
    DeleteTradeContract {
        /// Chain network to remove contract from (e.g., "base-sepolia")
        chain_network: String,
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

    /// Show balances for owner, signers, and contracts across all chains
    Balances,
}

/// `aspens-admin rpc <list|set|probe>` — per-chain RPC endpoint management.
#[derive(Debug, Subcommand)]
enum RpcCommand {
    /// List a chain's current RPC endpoint set (via `GetConfig`, unauthenticated).
    ///
    /// The result is MASKED exactly as the server returns it — `auth_secret`
    /// reads "***" and a url's query values/userinfo read "***". There is no
    /// unmask path; this prints the masked set as-is.
    List {
        /// Network identifier (e.g., "base-sepolia")
        network: String,
    },

    /// Replace a chain's complete RPC endpoint set (full replace, one call).
    ///
    /// Repeat `--endpoint` for multiple endpoints; the order given is the
    /// priority order the arborter fails over in. Requires at least one
    /// enabled endpoint — refused client-side here, and again by the server.
    Set {
        /// Network identifier (e.g., "base-sepolia")
        network: String,

        /// One endpoint: `label=url[,scheme=none|header|basic|bearer]\
        ///   [,key=<header-name-or-username>][,secret=<value>][,disabled]`.
        /// `scheme` defaults to `none`; omit `disabled` to leave it enabled.
        /// Repeatable — order sets priority.
        #[arg(long = "endpoint", value_parser = parse_rpc_endpoint_flag, required = true)]
        endpoint: Vec<RpcEndpoint>,
    },

    /// Probe a candidate RPC endpoint from the arborter's own network
    /// position, before committing it with `rpc set`. Never stored.
    Probe {
        /// Network identifier, used only to compare chain ids
        network: String,

        /// The endpoint URL to probe
        url: String,

        /// Auth scheme: none, header, basic, or bearer
        #[arg(long, default_value = "none", value_parser = parse_auth_scheme)]
        scheme: RpcAuthScheme,

        /// auth_key: header name for `header`, username for `basic`;
        /// unused for `bearer`/`none`
        #[arg(long, default_value = "")]
        key: String,

        /// auth_secret: header value / password / bearer token; unused for `none`
        #[arg(long, default_value = "")]
        secret: String,
    },
}

/// Parse a `scheme=...` value shared by `rpc probe --scheme` and
/// [`parse_rpc_endpoint_flag`]'s `scheme=` attribute — the two accept
/// exactly the same vocabulary.
fn parse_auth_scheme(s: &str) -> Result<RpcAuthScheme, String> {
    match s {
        "none" => Ok(RpcAuthScheme::RpcAuthNone),
        "header" => Ok(RpcAuthScheme::RpcAuthHeader),
        "basic" => Ok(RpcAuthScheme::RpcAuthBasic),
        "bearer" => Ok(RpcAuthScheme::RpcAuthBearer),
        other => Err(format!(
            "unrecognized auth scheme '{other}' (expected: none, header, basic, bearer)"
        )),
    }
}

/// Parse one `--endpoint` flag value for `rpc set`:
/// `label=url[,scheme=none|header|basic|bearer][,key=...][,secret=...][,disabled]`.
///
/// The label is everything before the FIRST `=`; the URL is everything up to
/// the first `,` after that (so a URL's own `=`, e.g. a query string, is
/// fine — only a literal `,` inside the URL would be misread as an attribute
/// separator, which this format does not attempt to escape). Remaining
/// comma-separated segments are `key=value` attributes, except the bare
/// literal `disabled`, which clears `enabled`.
fn parse_rpc_endpoint_flag(input: &str) -> Result<RpcEndpoint, String> {
    let (label, rest) = input.split_once('=').ok_or_else(|| {
        format!("invalid --endpoint '{input}': expected 'label=url[,attr=value...][,disabled]'")
    })?;
    if label.is_empty() {
        return Err(format!(
            "invalid --endpoint '{input}': label must not be empty"
        ));
    }

    let mut parts = rest.split(',');
    let url = parts.next().unwrap_or("").to_string();
    if url.is_empty() {
        return Err(format!(
            "invalid --endpoint '{input}': url must not be empty"
        ));
    }

    let mut auth_scheme = RpcAuthScheme::RpcAuthNone;
    let mut auth_key = String::new();
    let mut auth_secret = String::new();
    let mut enabled = true;

    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "disabled" {
            enabled = false;
            continue;
        }
        let (key, value) = part.split_once('=').ok_or_else(|| {
            format!(
                "invalid --endpoint '{input}': unrecognized attribute '{part}' \
                 (expected 'scheme=...', 'key=...', 'secret=...', or 'disabled')"
            )
        })?;
        match key {
            "scheme" => {
                auth_scheme = parse_auth_scheme(value)
                    .map_err(|e| format!("invalid --endpoint '{input}': {e}"))?;
            }
            "key" => auth_key = value.to_string(),
            "secret" => auth_secret = value.to_string(),
            other => {
                return Err(format!(
                    "invalid --endpoint '{input}': unrecognized attribute '{other}' \
                     (expected 'scheme=...', 'key=...', 'secret=...', or 'disabled')"
                ));
            }
        }
    }

    Ok(RpcEndpoint {
        label: label.to_string(),
        url,
        auth_scheme: auth_scheme as i32,
        auth_key,
        auth_secret,
        enabled,
    })
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
    // Best-effort: failing here only means logs aren't captured (e.g. a
    // subscriber is already set in-process) — don't abort the command over it.
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Build the client
    let mut builder = AspensClient::builder();

    if let Some(ref env_file) = cli.env_file {
        builder = builder.with_env_file(env_file);
    }

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

            // Re-build the admin wallet from the same privkey — the
            // address parse above was just for the user-facing message.
            let wallet = aspens::Wallet::from_evm_hex(privkey)?;
            let url = stack_url.clone();
            let result = executor
                .execute(async move {
                    auth::authenticate_with_wallet(url, &wallet, Some(chain_id)).await
                })
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
            rpc_url,
            factory_address,
            explorer_url,
            instance_signer_address,
        } => {
            let jwt = get_jwt()?;
            info!("Setting chain: {} ({})", canonical_name, network);

            let chain = Chain {
                architecture,
                canonical_name,
                network: network.clone(),
                chain_id,
                instance_signer_address: instance_signer_address.unwrap_or_default(),
                explorer_url,
                // `--rpc-url` is sugar for a single-endpoint list labeled
                // "primary", unauthenticated. Use `rpc set` for a
                // multi-endpoint / authenticated set.
                rpcs: vec![RpcEndpoint {
                    label: "primary".to_string(),
                    url: rpc_url,
                    auth_scheme: RpcAuthScheme::RpcAuthNone as i32,
                    auth_key: String::new(),
                    auth_secret: String::new(),
                    enabled: true,
                }],
                factory_address,
                trade_contract: None,
                tokens: HashMap::new(),
                // 0 = FINALITY_POLICY_UNSPECIFIED, which the arborter resolves
                // to FINALIZED — the safe default, and what every existing
                // chain row already reads back as.
                //
                // NOTE: this is the ONLY place an operator could set a
                // per-chain finality policy, so until `set-chain` grows flags
                // for it (and `set_chain` on the arborter side supports
                // update, not just insert), FINALITY_POLICY_CONFIRMATIONS is
                // unreachable in practice. Do not describe it as an escape
                // hatch until both land.
                finality: 0,
                finality_confirmations: 0,
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
        // RPC Commands
        // ====================================================================
        Commands::Rpc { action } => match action {
            RpcCommand::List { network } => {
                let endpoints = executor
                    .execute(admin::get_chain_rpcs(stack_url.clone(), network.clone()))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("list RPC endpoints for '{}'", network)
                        ))
                    })?;

                println!(
                    "RPC endpoints for '{}' (masked — secrets and url query/userinfo read \"***\"):",
                    network
                );
                if endpoints.is_empty() {
                    println!("  (none configured)");
                } else {
                    let mut table = Table::new();
                    table.load_style(UTF8_BORDERS_ONLY);
                    table.set_header(vec![
                        "#", "Label", "URL", "Scheme", "Key", "Secret", "Enabled",
                    ]);
                    for (i, ep) in endpoints.iter().enumerate() {
                        let scheme = RpcAuthScheme::try_from(ep.auth_scheme)
                            .map(|s| s.as_str_name().to_string())
                            .unwrap_or_else(|_| ep.auth_scheme.to_string());
                        table.add_row(vec![
                            (i + 1).to_string(),
                            ep.label.clone(),
                            ep.url.clone(),
                            scheme,
                            ep.auth_key.clone(),
                            ep.auth_secret.clone(),
                            ep.enabled.to_string(),
                        ]);
                    }
                    println!("{}", table);
                }
            }

            RpcCommand::Set { network, endpoint } => {
                let jwt = get_jwt()?;

                // clap's `required = true` already refuses zero `--endpoint`
                // flags; this catches the remaining zero-EFFECTIVE-endpoint
                // case clap cannot see: every endpoint given, but all marked
                // `disabled`.
                if !endpoint.iter().any(|e| e.enabled) {
                    return Err(eyre::eyre!(
                        "at least one --endpoint must be enabled: all {} given are `disabled`",
                        endpoint.len()
                    ));
                }

                info!(
                    "Setting {} RPC endpoint(s) for chain '{}'",
                    endpoint.len(),
                    network
                );
                let result = executor
                    .execute(admin::set_chain_rpcs(
                        stack_url.clone(),
                        jwt,
                        network.clone(),
                        endpoint,
                    ))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("set RPC endpoints for '{}'", network)
                        ))
                    })?;

                println!(
                    "RPC endpoints for '{}' updated ({} endpoint(s)):",
                    network,
                    result.rpcs.len()
                );
                for ep in &result.rpcs {
                    let suffix = if ep.enabled { "" } else { " [disabled]" };
                    println!("  - {} ({}){}", ep.label, ep.url, suffix);
                }
            }

            RpcCommand::Probe {
                network,
                url,
                scheme,
                key,
                secret,
            } => {
                let jwt = get_jwt()?;
                let endpoint = RpcEndpoint {
                    label: "probe".to_string(),
                    url: url.clone(),
                    auth_scheme: scheme as i32,
                    auth_key: key,
                    auth_secret: secret,
                    enabled: true,
                };

                let result = executor
                    .execute(admin::probe_chain_rpc(
                        stack_url.clone(),
                        jwt,
                        network.clone(),
                        endpoint,
                    ))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("probe RPC endpoint for '{}'", network)
                        ))
                    })?;

                println!("Probe result for '{}' on '{}':", url, network);
                println!("  Reachable:         {}", result.reachable);
                println!("  Reported chain id: {}", result.reported_chain_id);
                println!("  Chain id matches:  {}", result.chain_id_matches);
                println!("  Finalized tag ok:  {}", result.finalized_tag_ok);
                println!("  Latency:           {} ms", result.latency_ms);
            }
        },

        // ====================================================================
        // Token Commands
        // ====================================================================
        Commands::SetToken {
            network,
            name,
            symbol,
            address,
            decimals,
        } => {
            let jwt = get_jwt()?;
            info!("Setting token {} ({}) on {}", name, symbol, network);

            let token = Token {
                name,
                symbol: symbol.clone(),
                address,
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
                     - Check that tokens '{}' and '{}' are configured on their respective chains \
                       (run `set-token` first — the market takes its decimals from there)\n\
                     - Check each address matches its `set-token` row EXACTLY, case included: \
                       a checksummed and a lowercase spelling are different strings here",
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

            // Resolve chain architecture upfront: EVM admins sign+broadcast
            // create_instance locally and then ask arborter to confirm; Solana
            // admins authorize via JWT and arborter signs server-side, since
            // only the arborter signer satisfies the factory's `has_one = owner`
            // constraint. The two flows share nothing past this point.
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

            let is_solana = chain.architecture.eq_ignore_ascii_case("solana");

            let tx_hash = if is_solana {
                // Solana: server signs + submits, no admin private key needed.
                String::new()
            } else {
                // EVM: admin must sign + broadcast createInstance locally first.
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

                let params = CreateInstanceParams {
                    factory_address: calldata_response.factory_address.clone(),
                    calldata: calldata_response.calldata.clone(),
                    rpc_url: chain_client::chain_rpc_url(chain)?,
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
                let tx_hash = executor
                    .execute(admin::broadcast_transaction(
                        chain_client::chain_rpc_url(chain)?,
                        signed_tx,
                    ))
                    .map_err(|e| {
                        eyre::eyre!(format_error(
                            &e,
                            &format!("broadcast transaction to '{}'", network)
                        ))
                    })?;

                info!("Transaction broadcast with hash: {}", tx_hash);
                tx_hash
            };

            // Server-side handler: EVM waits on tx_hash; Solana signs + submits
            // and returns the new instance PDA + a signature receipt.
            let result = executor
                .execute(admin::deploy_contract(
                    stack_url.clone(),
                    jwt,
                    network.clone(),
                    tx_hash,
                    /* force */ false,
                    /* fee_bps */ fees as u32,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("wait for contract deployment on '{}'", network)
                    ))
                })?;
            println!("Trade contract deployed at: {}", result.contract_address);
            if !result.tx_signature.is_empty() {
                println!("Transaction: {}", result.tx_signature);
            }
        }

        Commands::SetTradeContract {
            address,
            chain_network,
        } => {
            let jwt = get_jwt()?;
            info!(
                "Setting trade contract {} on chain {}",
                address, chain_network
            );
            let result = executor
                .execute(admin::set_trade_contract(
                    stack_url.clone(),
                    jwt,
                    address.clone(),
                    chain_network.clone(),
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("set trade contract on chain {}", chain_network)
                    ))
                })?;
            if let Some(tc) = result.trade_contract {
                println!("Trade contract set: {}", tc.address);
            } else {
                println!("Trade contract set successfully");
            }
        }

        Commands::SetOperatorFee {
            chain_network,
            recipient,
            bps,
        } => {
            let jwt = get_jwt()?;
            info!(
                "Setting operator fee {} bps -> {} on chain {}",
                bps, recipient, chain_network
            );
            let result = executor
                .execute(admin::set_operator_fee(
                    stack_url.clone(),
                    jwt,
                    chain_network.clone(),
                    recipient.clone(),
                    bps,
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("set operator fee on chain {}", chain_network)
                    ))
                })?;
            if result.tx_signature.is_empty() {
                println!("Operator fee set (no on-chain tx returned)");
            } else {
                println!("Operator fee set: tx {}", result.tx_signature);
            }
        }

        Commands::RotateOperatorAdmin {
            chain_network,
            new_admin,
        } => {
            let jwt = get_jwt()?;
            info!(
                "Rotating operator admin -> {} on chain {}",
                new_admin, chain_network
            );
            let result = executor
                .execute(admin::set_operator_admin(
                    stack_url.clone(),
                    jwt,
                    chain_network.clone(),
                    new_admin.clone(),
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("rotate operator admin on chain {}", chain_network)
                    ))
                })?;
            if result.tx_signature.is_empty() {
                println!("Operator admin rotated (no on-chain tx returned)");
            } else {
                println!("Operator admin rotated: tx {}", result.tx_signature);
            }
        }

        Commands::DeleteTradeContract { chain_network } => {
            let jwt = get_jwt()?;
            info!("Deleting trade contract from chain {}", chain_network);
            let result = executor
                .execute(admin::delete_trade_contract(
                    stack_url.clone(),
                    jwt,
                    chain_network.clone(),
                ))
                .map_err(|e| {
                    eyre::eyre!(format_error(
                        &e,
                        &format!("delete trade contract from chain {}", chain_network)
                    ))
                })?;
            if result.success {
                println!(
                    "Trade contract deleted from chain {} successfully!",
                    chain_network
                );
            } else {
                return Err(eyre::eyre!(
                    "Failed to delete trade contract from chain {}\n\n\
                     Hints:\n\
                     - Verify the chain network is correct\n\
                     - Check that a trade contract exists on this chain\n\
                     - The contract may have active trades",
                    chain_network
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

        Commands::Balances => {
            use alloy::primitives::Address;

            info!("Fetching configuration and signer info...");

            let config_response = executor
                .execute(config::get_config(stack_url.clone()))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch configuration")))?;

            let configuration = config_response.config.as_ref().ok_or_else(|| {
                eyre::eyre!(
                    "the server returned no configuration.\n\nHints:\n  \
                     - the stack may be uninitialized — register a chain with `aspens-admin set-chain`\n  \
                     - or your session may have expired — run `aspens-admin login` again"
                )
            })?;

            let signer_response = executor
                .execute(config::get_signer_public_key(stack_url.clone(), None))
                .map_err(|e| eyre::eyre!(format_error(&e, "fetch signer public keys")))?;

            // Get owner address from ADMIN_PRIVKEY if available
            let owner_address: Option<Address> = client
                .get_env("ADMIN_PRIVKEY")
                .and_then(|pk| {
                    use alloy::signers::local::PrivateKeySigner;
                    pk.parse::<PrivateKeySigner>().ok()
                })
                .map(|s| s.address());

            println!("═══════════════════════════════════════════════════════════════════════════");
            println!("                            ADMIN BALANCES");
            println!("═══════════════════════════════════════════════════════════════════════════");
            println!();

            let mut fetch_warnings: Vec<String> = Vec::new();

            for chain in &configuration.chains {
                let signer_key = signer_response.chain_keys.get(&chain.network);
                let signer_addr: Option<Address> =
                    signer_key.and_then(|k| k.public_key.parse().ok());

                let contract_addr: Option<Address> = chain.trade_contract.as_ref().and_then(|tc| {
                    if tc.address.is_empty() {
                        None
                    } else {
                        tc.address.parse().ok()
                    }
                });

                let mut table = Table::new();
                table.load_style(UTF8_BORDERS_ONLY);
                table.set_header(vec![
                    "Address",
                    "Role",
                    "Gas",
                    &chain
                        .tokens
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("  |  "),
                ]);

                // Build header with individual token columns
                let token_symbols: Vec<String> = {
                    let mut syms: Vec<_> = chain.tokens.keys().cloned().collect();
                    syms.sort();
                    syms
                };
                let mut header = vec!["Role".to_string(), "Address".to_string(), "Gas".to_string()];
                for sym in &token_symbols {
                    header.push(sym.clone());
                }
                let mut table = Table::new();
                table.load_style(UTF8_BORDERS_ONLY);
                table.set_header(&header);

                println!("── {} (chain_id: {}) ──", chain.network, chain.chain_id);

                // Resolved once per chain: the arborter masks each endpoint's
                // url, so without a client-side override this errors — carry
                // that through as a per-cell "error" rather than aborting the
                // whole balances report.
                let rpc_url = chain_client::chain_rpc_url(chain);

                let addresses: Vec<(Address, &str)> = [
                    owner_address.map(|a| (a, "Owner")),
                    signer_addr.map(|a| (a, "Signer")),
                    contract_addr.map(|a| (a, "Contract")),
                ]
                .into_iter()
                .flatten()
                .collect();

                for (addr, role) in &addresses {
                    let gas = match &rpc_url {
                        Ok(rpc) => {
                            match balance::call_get_native_balance_for_address(rpc, *addr).await {
                                Ok(v) => balance::format_balance(v, 18),
                                Err(e) => {
                                    fetch_warnings
                                        .push(format!("{role} gas on {}: {e}", chain.network));
                                    "error".into()
                                }
                            }
                        }
                        Err(e) => {
                            fetch_warnings.push(format!("{role} gas on {}: {e}", chain.network));
                            "error".into()
                        }
                    };

                    let mut row = vec![
                        role.to_string(),
                        format!("{}...{}", &addr.to_string()[..6], &addr.to_string()[38..]),
                        gas,
                    ];

                    for sym in &token_symbols {
                        if let Some(token) = chain.tokens.get(sym) {
                            let bal = match &rpc_url {
                                Ok(rpc) => match balance::call_get_erc20_balance_for_address(
                                    rpc,
                                    &token.address,
                                    *addr,
                                )
                                .await
                                {
                                    Ok(v) => balance::format_balance(v, token.decimals),
                                    Err(e) => {
                                        fetch_warnings.push(format!(
                                            "{role} {sym} on {}: {e}",
                                            chain.network
                                        ));
                                        "error".into()
                                    }
                                },
                                Err(e) => {
                                    fetch_warnings
                                        .push(format!("{role} {sym} on {}: {e}", chain.network));
                                    "error".into()
                                }
                            };
                            row.push(bal);
                        } else {
                            row.push("-".into());
                        }
                    }
                    table.add_row(row);
                }

                println!("{}", table);
                println!();
            }

            if !fetch_warnings.is_empty() {
                eprintln!("Warnings — some balances could not be fetched (shown as \"error\"):");
                for w in &fetch_warnings {
                    eprintln!("  - {w}");
                }
                eprintln!(
                    "  hint: check the chain's RPC endpoints (`aspens-admin rpc list <network>`) \
                     / that one is reachable."
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod rpc_endpoint_flag_tests {
    use super::*;

    /// The simplest form: `label=url`, no auth attributes. `scheme` defaults
    /// to `none`, `key`/`secret` are empty, and the endpoint is enabled.
    #[test]
    fn parses_bare_label_and_url() {
        let ep = parse_rpc_endpoint_flag("primary=https://rpc.example.com").unwrap();
        assert_eq!(ep.label, "primary");
        assert_eq!(ep.url, "https://rpc.example.com");
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthNone as i32);
        assert_eq!(ep.auth_key, "");
        assert_eq!(ep.auth_secret, "");
        assert!(ep.enabled);
    }

    /// A url containing its own `=` (a query string) must not confuse the
    /// label/url split — only the FIRST `=` in the whole flag value is the
    /// label boundary.
    #[test]
    fn url_with_its_own_equals_sign_is_preserved() {
        let ep = parse_rpc_endpoint_flag("primary=https://rpc.example.com/v2?key=abc").unwrap();
        assert_eq!(ep.label, "primary");
        assert_eq!(ep.url, "https://rpc.example.com/v2?key=abc");
    }

    #[test]
    fn parses_header_scheme() {
        let ep = parse_rpc_endpoint_flag(
            "alchemy=https://eth.example.com/v2,scheme=header,key=x-api-key,secret=shh",
        )
        .unwrap();
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthHeader as i32);
        assert_eq!(ep.auth_key, "x-api-key");
        assert_eq!(ep.auth_secret, "shh");
        assert!(ep.enabled);
    }

    #[test]
    fn parses_basic_scheme() {
        let ep = parse_rpc_endpoint_flag(
            "basic-rpc=https://rpc.example.com,scheme=basic,key=user,secret=pass",
        )
        .unwrap();
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthBasic as i32);
        assert_eq!(ep.auth_key, "user");
        assert_eq!(ep.auth_secret, "pass");
    }

    /// `bearer` ignores `key` (per the proto's documented contract) but this
    /// parser does not enforce that — it just carries whatever `key=` was
    /// given, same as the wire message allows. Only `secret` matters here.
    #[test]
    fn parses_bearer_scheme() {
        let ep = parse_rpc_endpoint_flag(
            "bearer-rpc=https://rpc.example.com,scheme=bearer,secret=token123",
        )
        .unwrap();
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthBearer as i32);
        assert_eq!(ep.auth_key, "");
        assert_eq!(ep.auth_secret, "token123");
    }

    #[test]
    fn explicit_scheme_none_is_the_default_shape() {
        let ep = parse_rpc_endpoint_flag("primary=https://rpc.example.com,scheme=none").unwrap();
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthNone as i32);
    }

    #[test]
    fn disabled_attribute_clears_enabled() {
        let ep = parse_rpc_endpoint_flag("backup=https://rpc2.example.com,disabled").unwrap();
        assert!(!ep.enabled);
    }

    /// `disabled` combined with auth attributes, in either order — the
    /// bare-word attribute must not be confused with a `key=value` pair.
    #[test]
    fn disabled_combines_with_auth_attributes() {
        let ep = parse_rpc_endpoint_flag(
            "backup=https://rpc2.example.com,scheme=header,key=x-api-key,secret=shh,disabled",
        )
        .unwrap();
        assert!(!ep.enabled);
        assert_eq!(ep.auth_scheme, RpcAuthScheme::RpcAuthHeader as i32);
    }

    #[test]
    fn missing_equals_sign_is_an_error() {
        assert!(parse_rpc_endpoint_flag("no-equals-here").is_err());
    }

    #[test]
    fn empty_label_is_an_error() {
        assert!(parse_rpc_endpoint_flag("=https://rpc.example.com").is_err());
    }

    #[test]
    fn empty_url_is_an_error() {
        assert!(parse_rpc_endpoint_flag("primary=").is_err());
    }

    #[test]
    fn unrecognized_scheme_is_an_error() {
        let err =
            parse_rpc_endpoint_flag("primary=https://rpc.example.com,scheme=nope").unwrap_err();
        assert!(
            err.contains("nope"),
            "error should name the bad scheme: {err}"
        );
    }

    #[test]
    fn unrecognized_attribute_is_an_error() {
        assert!(parse_rpc_endpoint_flag("primary=https://rpc.example.com,bogus=1").is_err());
    }

    /// clap surfaces a malformed `--endpoint` (bad scheme) as a parse error
    /// from `Cli::try_parse_from`, not a panic or a silently-accepted value —
    /// this is what makes it "a clap error" rather than just an error from
    /// the bare parser function.
    #[test]
    fn malformed_scheme_is_a_clap_error() {
        let result = Cli::try_parse_from([
            "aspens-admin",
            "rpc",
            "set",
            "base-sepolia",
            "--endpoint",
            "primary=https://a.example.com,scheme=nope",
        ]);
        assert!(result.is_err());
    }

    /// Zero `--endpoint` flags is refused by clap itself (`required = true`),
    /// before any handler code runs.
    #[test]
    fn zero_endpoint_flags_is_a_clap_error() {
        let result = Cli::try_parse_from(["aspens-admin", "rpc", "set", "base-sepolia"]);
        assert!(result.is_err());
    }

    /// Two `--endpoint` flags land in `Vec<RpcEndpoint>` in the order given —
    /// that order IS the failover priority, so clap's natural accumulation
    /// order must not be reshuffled.
    #[test]
    fn two_endpoint_flags_preserve_order() {
        let cli = Cli::try_parse_from([
            "aspens-admin",
            "rpc",
            "set",
            "base-sepolia",
            "--endpoint",
            "primary=https://a.example.com",
            "--endpoint",
            "backup=https://b.example.com,disabled",
        ])
        .unwrap();

        match cli.command {
            Commands::Rpc {
                action: RpcCommand::Set { network, endpoint },
            } => {
                assert_eq!(network, "base-sepolia");
                assert_eq!(endpoint.len(), 2);
                assert_eq!(endpoint[0].label, "primary");
                assert_eq!(endpoint[1].label, "backup");
                assert!(endpoint[0].enabled);
                assert!(!endpoint[1].enabled);
            }
            other => panic!("expected Rpc::Set, got {other:?}"),
        }
    }

    /// `rpc probe`'s own `--scheme` flag goes through the same
    /// [`parse_auth_scheme`] vocabulary as `--endpoint`'s `scheme=` attribute.
    #[test]
    fn probe_scheme_flag_uses_the_same_vocabulary() {
        let cli = Cli::try_parse_from([
            "aspens-admin",
            "rpc",
            "probe",
            "base-sepolia",
            "https://rpc.example.com",
            "--scheme",
            "bearer",
            "--secret",
            "tok",
        ])
        .unwrap();

        match cli.command {
            Commands::Rpc {
                action:
                    RpcCommand::Probe {
                        network,
                        url,
                        scheme,
                        secret,
                        ..
                    },
            } => {
                assert_eq!(network, "base-sepolia");
                assert_eq!(url, "https://rpc.example.com");
                assert_eq!(scheme, RpcAuthScheme::RpcAuthBearer);
                assert_eq!(secret, "tok");
            }
            other => panic!("expected Rpc::Probe, got {other:?}"),
        }
    }

    /// `rpc probe` with no `--scheme` defaults to `none`.
    #[test]
    fn probe_scheme_defaults_to_none() {
        let cli = Cli::try_parse_from([
            "aspens-admin",
            "rpc",
            "probe",
            "base-sepolia",
            "https://rpc.example.com",
        ])
        .unwrap();

        match cli.command {
            Commands::Rpc {
                action: RpcCommand::Probe { scheme, .. },
            } => assert_eq!(scheme, RpcAuthScheme::RpcAuthNone),
            other => panic!("expected Rpc::Probe, got {other:?}"),
        }
    }
}
