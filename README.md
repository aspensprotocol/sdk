# Aspens SDK

A comprehensive SDK and CLI tools for interacting with Aspens Markets Stack, providing cross-chain trading capabilities with proper decimal handling and market operations.

## Project Structure

This is a Cargo workspace with three main components:

- **`aspens/`** - Core Rust library crate with trading logic and gRPC client
- **`aspens-cli/`** - Command-line interface binary for scripted operations
- **`aspens-repl/`** - Interactive REPL binary for manual trading
- **`examples/`** - Practical examples and decimal conversion guides
- **`scripts/`** - Utility scripts for environment management and testing

## Prerequisites

1. **Install Rust:**
```bash
# Install Rust using rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. **Set up environment variables:**
```bash
# Copy the environment template
cp .env.sample .env.anvil.local

# Edit the configuration values in .env.anvil.local
# The SDK will automatically load environment-specific files
```

## Building the Project

```bash
# Build the entire workspace (library + binaries)
just build

# Build release version
just release

# Build only the core library
just build-lib

# Build only the CLI
just build-cli

# Build only the REPL
just build-repl
```

## Usage

The SDK can be used in three ways: as a Rust library, as a CLI tool, or as an interactive REPL.

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
    // Build the client
    let client = AspensClient::builder()
        .with_url("http://localhost:50051")?
        .with_environment("testnet")
        .build()?;

    // Use trading operations
    // ...

    Ok(())
}
```

### 2. Interactive Mode (REPL)

Interactive mode provides a REPL (Read-Eval-Print Loop) interface where you can execute commands one at a time:

```bash
# Start the REPL with anvil environment
just repl-anvil

# Or with testnet environment
just repl-testnet

# Inside the REPL
aspens> help
Usage: <COMMAND>

Commands:
  initialize       Initialize a new trading session by (optionally) defining the arborter URL
  get-config       Config: Fetch the current configuration from the arborter server
  download-config  Config: Download configuration to a file
  deposit          Deposit token(s) to make them available for trading
  withdraw         Withdraw token(s) to a local wallet
  buy              Send a BUY order
  sell             Send a SELL order
  get-orders       Get a list of all active orders
  cancel-order     Cancel an order
  balance          Fetch the balances
  get-orderbook    Fetch the latest top of book
  quit             Quit the REPL
  help             Print this message or the help of the given subcommand(s)

aspens> quit
```

### 3. Scripted Mode (CLI)

Scripted mode allows you to execute commands directly from the command line, which is useful for automation and scripting:

```bash
# Run with anvil environment
just cli-anvil balance

# Run with testnet environment
just cli-testnet deposit base-goerli usdc 1000

# Or use cargo directly
cargo run -p aspens-cli -- --env testnet buy 100 --limit-price 50000
```

## Quick Start with Examples

For practical examples and decimal conversion guides, see the [examples directory](examples/README.md) which includes:

- Decimal conversion examples for various token pairs
- Interactive trading sessions
- Batch trading scripts
- Troubleshooting guides

## Available Commands

### Just Commands

The project includes a `justfile` with convenient commands:

```bash
# List all available commands
just

# Build the project
just build

# Run tests
just test

# Format code
just fmt

# Run CLI with specific environment
just cli-anvil [args]
just cli-testnet [args]

# Run REPL with specific environment
just repl-anvil
just repl-testnet
```

### Environment Management

```bash
# List available environments
just env-list

# Switch to specific environment
just env-switch <environment>

# Create new environment template
just create-env <name>
```

## Development

### Running Tests

```bash
# Run all tests
just test

# Run tests for library only
just test-lib

# Run tests with specific environment
just test-anvil
just test-testnet
```

### Code Quality

```bash
# Format code
just fmt

# Check code style
just check

# Run linter
just lint
```

### Clean Build

```bash
# Clean build artifacts
just clean
```

## Configuration

The SDK uses environment variables for configuration. Key variables include:

- `ARBORTER_URL` - Arborter server endpoint
- `MARKET_ID_*` - Market identifiers for trading pairs
- `*_RPC_URL` - Blockchain RPC endpoints
- `*_CONTRACT_ADDRESS` - Smart contract addresses
- `PRIVATE_KEY_*` - Wallet private keys (use test accounts only!)

Environment-specific configuration files follow the pattern `.env.{environment}.local` (e.g., `.env.testnet.local`, `.env.anvil.local`).

## Architecture

### Core Library (`aspens/`)

The core library provides:
- **AspensClient** - Main client with builder pattern for configuration
- **Trading operations** - Deposit, withdraw, buy, sell, balance queries
- **Executor pattern** - Async/sync execution strategies
- **gRPC client** - Protocol buffer communication with Arborter server
- **Smart contract integration** - Alloy-based Ethereum interactions

### CLI Binary (`aspens-cli/`)

Command-line interface for scripted trading operations. Supports all trading commands with flags and arguments.

### REPL Binary (`aspens-repl/`)

Interactive Read-Eval-Print Loop for manual trading with:
- Command history
- Interactive prompts
- Session state management

## Decimal Handling

**Critical**: Aspens handles tokens with different decimal places across chains. The SDK works in "pair decimals" format internally, not native token decimals:
- Base token (e.g., WBTC: 8 decimals)
- Quote token (e.g., USDT: 6 decimals)
- Pair decimals (configured per market, may differ from both)

All order quantities and prices must be in pair decimal format when sent to the gRPC API. See `decimals.md` for detailed conversion examples.

## Support and Documentation

- [Examples Directory](examples/README.md) - Practical usage examples
- [Decimal Conversion Guide](decimals.md) - Understanding decimal handling
- [CLAUDE.md](CLAUDE.md) - Architecture guide for AI assistants

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests: `just test`
5. Format code: `just fmt`
6. Submit a pull request

## License

This project is licensed under the MIT License.
