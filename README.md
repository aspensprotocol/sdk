# Aspens SDK

A comprehensive SDK and CLI tools for interacting with an [Aspens Markets Stack](https://docs.aspens.xyz).

## Available Commands

| Command | Description |
|---------|-------------|
| `config` | Fetch and display the configuration from the server |
| `deposit <network> <token> <amount>` | Deposit tokens to make them available for trading |
| `withdraw <network> <token> <amount>` | Withdraw tokens to a local wallet |
| `buy-market <market> <amount>` | Send a market BUY order (executes at best available price) |
| `buy-limit <market> <amount> <price>` | Send a limit BUY order (executes at specified price or better) |
| `sell-market <market> <amount>` | Send a market SELL order (executes at best available price) |
| `sell-limit <market> <amount> <price>` | Send a limit SELL order (executes at specified price or better) |
| `cancel-order <market> <side> <order_id>` | Cancel an existing order by its ID |
| `stream-orderbook <market> [--historical] [--trader <addr>]` | Stream orderbook entries in real-time |
| `stream-trades <market> [--historical] [--trader <addr>]` | Stream executed trades in real-time |
| `balance` | Fetch the current balances for all supported tokens across all chains |
| `status` | Show current configuration and connection status |
| `trader-public-key` | Get the public key and address for the trader wallet |
| `signer-public-key [--chain-id <id>]` | Get the signer public key(s) for the trading instance |

All commands are available in both `aspens-cli` and `aspens-repl`.

## Project Structure

This is a Cargo workspace with four main components:

- **`aspens/`** - Core Rust library crate with trading logic and gRPC client
- **`aspens-cli/`** - Command-line interface binary for scripted operations
- **`aspens-repl/`** - Interactive REPL binary for manual trading
- **`aspens-admin/`** - Administrative CLI for stack configuration (chains, tokens, markets)

## Prerequisites

1. **Install Rust:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. **Install Just (Optional but Recommended):**

[Just](https://github.com/casey/just) is a command runner that simplifies common development tasks.

```bash
brew install just          # macOS
cargo install just         # Any platform
```

3. **Configure environment:**
```bash
cp .env.sample .env        # Copy the template
# Edit .env with your configuration (ASPENS_MARKET_STACK_URL, TRADER_PRIVKEY, etc.)
```

## Building

```bash
just build                 # Build entire workspace
just release               # Build release version
just build-lib             # Build core library only
just build-cli             # Build CLI only
just build-repl            # Build REPL only
just build-admin           # Build Admin CLI only
```

## Usage

### 1. As a Rust Library

Add to your `Cargo.toml`:
```toml
[dependencies]
aspens = { path = "../aspens" }
```

Use in your code:
```rust
use aspens::{AspensClient, DirectExecutor};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let client = AspensClient::builder()
        .with_url("http://localhost:50051")?
        .build()?;

    // Use trading operations...
    Ok(())
}
```

### 2. Interactive Mode (REPL)

```bash
cargo run --bin aspens-repl

# Inside the REPL
aspens> help
aspens> balance
aspens> deposit base-sepolia USDC 1000
aspens> buy-market USDC/USDT 100
aspens> quit
```

### 3. Scripted Mode (CLI)

```bash
cargo run --bin aspens-cli -- balance
cargo run --bin aspens-cli -- deposit base-sepolia USDC 1000
cargo run --bin aspens-cli -- buy-market USDC/USDT 100
```

### 4. Admin CLI

```bash
# Initialize admin (first time only)
cargo run --bin aspens-admin -- init-admin --address 0xYourAddress

# Login to get JWT
cargo run --bin aspens-admin -- login

# Admin commands (JWT set in .env or via --jwt flag)
cargo run --bin aspens-admin -- set-chain --network base-sepolia ...
cargo run --bin aspens-admin -- set-token --network base-sepolia --symbol USDC ...
cargo run --bin aspens-admin -- status
```

## Just Commands

```bash
just                       # List all available commands
just build                 # Build the project
just test                  # Run all tests
just test-lib              # Run library tests only
just fmt                   # Format code
just check                 # Check code style
just lint                  # Run linter
just clean                 # Clean build artifacts
```

## Architecture

### Core Library (`aspens/`)

- **AspensClient** - Main client with builder pattern for configuration
- **Trading operations** - Deposit, withdraw, buy, sell, balance queries
- **Executor pattern** - Async/sync execution strategies
- **gRPC client** - Protocol buffer communication with an Aspens Market Stack
- **Smart contract integration** - Alloy-based Ethereum interactions

### CLI Binary (`aspens-cli/`)

Command-line interface for scripted trading operations.

### REPL Binary (`aspens-repl/`)

Interactive Read-Eval-Print Loop for manual trading with command history and session state.

### Admin Binary (`aspens-admin/`)

Administrative CLI for managing stack configuration with EIP-712 signature authentication.

## Decimal Handling

Aspens handles tokens with different decimal places across chains. The SDK works in "pair decimals" format internally. See `decimals.md` for detailed conversion examples.

## Documentation

- [Decimal Conversion Guide](decimals.md) - Understanding decimal handling
- [CLAUDE.md](CLAUDE.md) - Architecture guide for development

## License

This project is licensed under the Apache License 2.0. See the [LICENSE](LICENSE) file for details.
