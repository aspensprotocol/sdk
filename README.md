# Aspens SDK

A comprehensive SDK and CLI tools for interacting with Aspens Markets Stack, providing cross-chain trading capabilities with proper decimal handling and market operations.

## Project Structure

- **`sdk/`** - Core Rust SDK with trading logic and gRPC client
- **`wrappers/`** - CLI and REPL tools built on top of the SDK
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
cp sdk/.env.sample sdk/.env

# Edit the configuration values in sdk/.env
# Then source the environment
source sdk/.env
```

## Building the Project

```bash
# Build the core SDK
just build

# Build release version
just release

# Build the CLI wrappers
cd wrappers
cargo build --release
```

## Usage

The CLI can be used in two modes: interactive (REPL) and scripted.

### Interactive Mode (REPL)

Interactive mode provides a REPL (Read-Eval-Print Loop) interface where you can execute commands one at a time:

```bash
# Start the REPL
❯ cd wrappers/
❯ cargo run --bin aspens-repl

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

Options:
  -h, --help     Print help
  -V, --version  Print version
aspens> quit
```

### Scripted Mode

Scripted mode allows you to execute commands directly from the command line, which is useful for automation and scripting:

```bash
❯ cd wrappers/ 
❯ cargo run --bin aspens-cli

Aspens CLI for trading operations

Usage: aspens-cli [OPTIONS] <COMMAND>

Commands:
  initialize       Initialize a new trading session
  get-config       Config: Fetch the current configuration from the arborter server
  download-config  Download configuration to a file
  deposit          Deposit token(s) to make them available for trading
  withdraw         Withdraw token(s) to a local wallet
  buy              Send a BUY order
  sell             Send a SELL order
  get-orders       Get a list of all active orders
  cancel-order     Cancel an order
  balance          Fetch the balances
  get-orderbook    Fetch the latest top of book
  help             Print this message or the help of the given subcommand(s)
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
just cli-anvil
just cli-testnet

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

## Support and Documentation

- [Examples Directory](examples/README.md) - Practical usage examples
- [Decimal Conversion Guide](decimals.md) - Understanding decimal handling
- [SDK Documentation](sdk/src/lib.rs) - Core SDK API reference
- [CLI Documentation](wrappers/src/bin/cli.rs) - Command-line interface reference

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests: `just test`
5. Format code: `just fmt`
6. Submit a pull request

## License

This project is licensed under the MIT License.
