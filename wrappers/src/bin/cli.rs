use anyhow::Result;
use clap::{Parser, ValueEnum};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;
#[cfg(feature = "admin")]
use wrappers::utils::wrap_admin::*;
use wrappers::utils::{executor::DirectExecutor, wrap_trader::*};

#[derive(Debug, Parser)]
#[command(name = "aspens-cli")]
#[command(about = "Aspens CLI for trading operations")]
struct Cli {
    /// The URL of the arborter server
    #[arg(short, long, default_value_t = Url::parse("http://localhost:50051").unwrap())]
    url: Url,

    #[command(flatten)]
    verbose: clap_verbosity::Verbosity,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Parser)]
enum Commands {
    /// Initialize a new trading session
    Initialize,
    /// Config: Fetch the current configuration from the arborter server
    #[cfg(feature = "admin")]
    GetConfig,
    /// Download configuration to a file
    #[cfg(feature = "admin")]
    DownloadConfig {
        /// Path to save the configuration file
        #[arg(short, long)]
        path: String,
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
        amount: String,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
    },
    /// Send a SELL order
    Sell {
        /// Amount to sell
        amount: String,
        /// Limit price for the order
        #[arg(short, long)]
        limit_price: Option<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables
    dotenv::from_filename(".env.anvil.local").ok();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    let cli = Cli::parse();
    let executor = DirectExecutor;

    match cli.command {
        Commands::Initialize => {
            info!("Initialized session at {}", cli.url);
            info!("Available config for {} is <TODO!!>", cli.url);
        }
        #[cfg(feature = "admin")]
        Commands::GetConfig => {
            let _ = wrap_get_config(&executor, cli.url.to_string());
            ()
        }
        Commands::Deposit {
            chain,
            token,
            amount,
        } => {
            let _ = wrap_deposit(&executor, chain, token, amount);
            ()
        }
        Commands::Withdraw {
            chain,
            token,
            amount,
        } => {
            let _ = wrap_withdraw(&executor, chain, token, amount);
            ()
        }
        Commands::Buy {
            amount,
            limit_price,
        } => {
            let _ = wrap_buy(&executor, cli.url.to_string(), amount, limit_price);
            ()
        }
        Commands::Sell {
            amount,
            limit_price,
        } => {
            let _ = wrap_sell(&executor, cli.url.to_string(), amount, limit_price);
            ()
        }
        Commands::GetOrders => {
            info!("Getting orders...");
            info!("TODO: Implement this");
        }
        Commands::CancelOrder { order_id } => {
            info!("Order canceled: {order_id:?}");
            info!("TODO: Implement this");
        }
        Commands::Balance => {
            let _ = wrap_balance(&executor);
            ()
        }
        Commands::GetOrderbook { market_id } => {
            info!("Getting orderbook: {market_id:?}");
            info!("TODO: Implement this");
        }
        #[cfg(feature = "admin")]
        Commands::DownloadConfig { path } => {
            let _ = wrap_download_config(&executor, cli.url.to_string(), path);
            ()
        }
    }

    Ok(())
}
