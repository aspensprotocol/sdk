# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Aspens SDK is a cross-chain trading platform SDK written in Rust, organized as a Cargo workspace with three main components:

- **`aspens/`** - Core library crate with trading logic and gRPC client implementation
- **`aspens-cli/`** - Command-line interface binary for scripted operations
- **`aspens-repl/`** - Interactive REPL binary for manual trading
- **`examples/`** - Practical examples and decimal conversion guides
- **`scripts/`** - Utility scripts for environment management (`env-switch.sh`) and testing (`ammit.sh`)

## Build Commands

```bash
# Build entire workspace
just build

# Build release version
just release

# Build specific crates
just build-lib      # Core library only
just build-cli      # CLI binary only
just build-repl     # REPL binary only

# Run tests
just test           # All tests
just test-lib       # Library tests only

# Format code
just fmt

# Check code style
just check

# Run linter
just lint

# Clean build artifacts
just clean
```

## Running CLI and REPL

```bash
# CLI with specific environment
just cli-anvil [args]
just cli-testnet [args]

# REPL with specific environment
just repl-anvil
just repl-testnet

# Direct cargo commands
cargo run -p aspens-cli -- --env anvil [args]
cargo run -p aspens-repl -- --env testnet
```

## Testing

```bash
# Run all tests
just test

# Run library tests only
just test-lib

# Run tests with specific environment
just test-anvil
just test-testnet
```

## Architecture

### Cargo Workspace Structure

This is a proper Cargo workspace (not separate crates). The root `Cargo.toml` defines:
- Workspace members: `aspens`, `aspens-cli`, `aspens-repl`
- Shared dependencies across all crates
- Common workspace metadata (version, edition, license)

### Core Library (`aspens/`)

The core library is a pure Rust library with NO CLI dependencies. It provides:

**Public API:**
- **`AspensClient`** - Main client with builder pattern for configuration
  - `AspensClient::builder()` - Create a new client builder
  - `.with_url()` - Set Arborter server URL
  - `.with_environment()` - Set environment name (anvil, testnet, etc.)
  - `.with_env_file()` - Set custom .env file path
  - `.build()` - Build the client
- **Executors** - Async/sync execution strategies
  - `DirectExecutor` - Single-threaded executor for CLI
  - `BlockingExecutor` - Multi-threaded executor for REPL

**Internal modules in `aspens/src/`:**
- **`client.rs`**: AspensClient and builder implementation
- **`executor.rs`**: Async executor pattern implementations
- **`commands/trading/`**: Core trading logic
  - `balance.rs` - Query balances across chains
  - `deposit.rs` - Deposit tokens to make them available for trading
  - `send_order.rs` - Submit buy/sell orders
  - `withdraw.rs` - Withdraw tokens to local wallets
- **`commands/config/`**: Configuration fetching and management (admin feature)
- **`proto/`**: gRPC protocol buffer definitions (internal, not exposed)

**Key design principles:**
- Environment configuration is handled in the library, not binaries
- Client loads `.env.{environment}.local` files automatically
- Environment variables are stored in the client for easy access
- Protocol buffers are kept internal; clean Rust types are exposed

### CLI Binary (`aspens-cli/`)

Standalone binary for command-line usage. Structure:
- Single `main.rs` file
- Uses `clap` for argument parsing
- Builds `AspensClient` and uses `DirectExecutor`
- Calls into `aspens` library functions directly

### REPL Binary (`aspens-repl/`)

Standalone binary for interactive usage. Structure:
- Single `main.rs` file
- Uses `clap-repl` for REPL functionality
- Maintains session state with `AppState`
- Uses `BlockingExecutor` for synchronous execution in REPL context

### Smart Contract Integration

The SDK uses Alloy for Ethereum-compatible chain interactions:
- **MidribV2**: Main trading contract (loaded from `artifacts/MidribV2.json`)
- **IERC20**: Standard ERC20 interface for token operations (approve, allowance, balanceOf)

Contract ABIs are generated at compile time using `alloy-sol-types` macros in `aspens/src/commands/trading/mod.rs`.

### Protocol Buffers

gRPC communication is generated from `.proto` files during build:
- **`proto/arborter.proto`**: Trading API definitions
- **`proto/arborter_config.proto`**: Configuration API with serde serialization

Build script (`aspens/build.rs`) uses `tonic-prost-build` to generate client code into `proto/generated/`.

### Decimal Handling

**Critical**: Aspens handles tokens with different decimal places across chains. The SDK works in "pair decimals" format internally, not native token decimals:
- Base token (e.g., WBTC: 8 decimals)
- Quote token (e.g., USDT: 6 decimals)
- Pair decimals (configured per market, may differ from both)

All order quantities and prices must be in pair decimal format when sent to the gRPC API. See `decimals.md` for detailed conversion examples.

### Environment Configuration

The project uses environment-specific `.env` files:
- `.env.sample` - Template with required variables
- `.env.anvil.local` - Local Anvil configuration
- `.env.testnet.local` - Testnet configuration

Environment variables include:
- `MARKET_ID` - Format: `chain_id::token_address::chain_id::token_address`
- Private keys for wallet operations (Hedera and EVM accounts)
- RPC URLs and contract addresses

The `AspensClient` automatically loads the appropriate `.env.{environment}.local` file based on the environment parameter.

Use `scripts/env-switch.sh` to switch between environments (or `just env-switch <env>`).

## Development Workflow

### Adding New Trading Commands

1. Add core logic to `aspens/src/commands/trading/` as a new module
2. Export the module in `aspens/src/commands/trading/mod.rs`
3. Add CLI command in `aspens-cli/src/main.rs`
4. Add REPL command in `aspens-repl/src/main.rs`
5. Update both binaries to call the new function

**Note:** The new architecture eliminates the wrapper layer - binaries call library functions directly.

### Adding Features to AspensClient

To add new capabilities to the client:
1. Add the implementation in `aspens/src/client.rs` or related module
2. Update the public API exports in `aspens/src/lib.rs`
3. Update binaries to use the new functionality
4. Add tests in the library crate

### Modifying Protocol Buffers

1. Edit `.proto` files in `aspens/proto/`
2. Update `aspens/build.rs` if adding new types that need serde support
3. Run `cargo build` to regenerate Rust code
4. Update usages in library and binaries

### Working with Smart Contracts

Contract ABIs are stored in `artifacts/`. To update:
1. Place new JSON ABI in `artifacts/`
2. Update the `sol!` macro in `aspens/src/commands/trading/mod.rs`
3. Rebuild to regenerate contract bindings

### Testing

When adding tests:
- Library tests go in `aspens/src/` modules or `aspens/tests/`
- Binary-specific tests go in respective binary crates
- Use `just test-lib` to run only library tests
- Use feature flags to conditionally compile admin features

## Common Patterns

### Using AspensClient in code

```rust
use aspens::{AspensClient, DirectExecutor};

let client = AspensClient::builder()
    .with_url("https://<aspens-stack-url.com>")?
    .with_environment("testnet")
    .build()?;

// Use executor for async operations
let executor = DirectExecutor;
let result = executor.execute(some_async_function())?;
```

### Adding a new command to both CLI and REPL

1. Implement function in library (e.g., `aspens/src/commands/trading/new_feature.rs`)
2. Add to CLI enum and match statement in `aspens-cli/src/main.rs`
3. Add to REPL enum and match statement in `aspens-repl/src/main.rs`
4. Both should use the same library function

## Important Notes

- This IS a Cargo workspace (changed from previous non-workspace structure)
- The library has NO dependencies on CLI tools (clap, clap-repl)
- Environment configuration is now handled by AspensClient
- Binaries are thin wrappers around library functionality
- Protocol buffers are internal implementation details
