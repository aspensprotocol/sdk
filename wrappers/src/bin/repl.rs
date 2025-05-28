use clap::Parser;
use clap_repl::reedline::{
    DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal,
};
use clap_repl::ClapEditor;
use std::sync::{Arc, Mutex};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use url::Url;
#[cfg(feature = "admin")]
use wrappers::utils::wrap_admin::*;
use wrappers::utils::{executor::BlockingExecutor, wrap_trader::*};

struct AppState {
    url: Arc<Mutex<Url>>,
    #[cfg(feature = "admin")]
    config: Arc<Mutex<Option<config_pb::Configuration>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            url: Arc::new(Mutex::new(Url::parse("http://0.0.0.0:50051").unwrap())),
            #[cfg(feature = "admin")]
            config: Arc::new(Mutex::new(None)),
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

    #[cfg(feature = "admin")]
    fn update_config(&self, config: config_pb::Configuration) {
        let mut guard = self.config.lock().unwrap();
        *guard = Some(config);
    }

    #[cfg(feature = "admin")]
    fn get_config(&self) -> Option<config_pb::Configuration> {
        let guard = self.config.lock().unwrap();
        guard.clone()
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
    #[cfg(feature = "admin")]
    GetConfig,
    /// Config: Download configuration to a file
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

fn main() {
    dotenv::from_filename(".env.anvil.local").ok();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    let mut app_state = AppState::new();
    let executor = BlockingExecutor::new();

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
        #[cfg(feature = "admin")]
        ReplCommand::GetConfig => {
            if let Ok(Some(config)) = wrap_get_config(&executor, app_state.url()) {
                app_state.update_config(config);
                let updated_config = app_state.get_config();
                info!("GetConfig result: {updated_config:#?}");
            } else {
                info!("GetConfigResponse did not contain a configuration");
            }
        }
        #[cfg(feature = "admin")]
        ReplCommand::DownloadConfig { path } => {
            if let Err(e) = wrap_download_config(&executor, app_state.url(), path) {
                info!("Failed to download config: {e:?}");
            }
        }
        ReplCommand::Deposit {
            chain,
            token,
            amount,
        } => {
            if let Err(e) = wrap_deposit(&executor, chain, token, amount) {
                info!("Failed to deposit: {e:?}");
            }
        }
        ReplCommand::Withdraw {
            chain,
            token,
            amount,
        } => {
            if let Err(e) = wrap_withdraw(&executor, chain, token, amount) {
                info!("Failed to withdraw: {e:?}");
            }
        }
        ReplCommand::Buy {
            amount,
            limit_price,
            matching_order_ids: _,
        } => {
            let limit_price = if limit_price.is_none() {
                let mut rl = Reedline::create();
                let price = read_input(&mut rl, "At what price? ");
                Some(price.parse::<u64>().unwrap())
            } else {
                limit_price
            };

            if let Err(e) = wrap_buy(&executor, app_state.url(), amount, limit_price) {
                info!("Failed to send buy order: {e:?}");
            }
        }
        ReplCommand::Sell {
            amount,
            limit_price,
            matching_order_ids: _,
        } => {
            let limit_price = if limit_price.is_none() {
                let mut rl = Reedline::create();
                let price = read_input(&mut rl, "At what price? ");
                Some(price.parse::<u64>().unwrap())
            } else {
                limit_price
            };

            if let Err(e) = wrap_sell(&executor, app_state.url(), amount, limit_price) {
                info!("Failed to send sell order: {e:?}");
            }
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
            if let Err(e) = wrap_balance(&executor) {
                info!("Failed to get balance: {e:?}");
            }
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
