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

Full client (gRPC + trading commands + RPC submission):
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

Stateless signing only (no gRPC, no tokio, no RPC client — e.g. browser
via `wasm-bindgen`, edge workers, or a service that submits orders over
its own transport):
```toml
[dependencies]
aspens = { path = "../aspens", default-features = false, features = ["evm", "solana"] }
```
```rust
use aspens::orders::{derive_order_id, GaslessLockParams};
use aspens::evm::gasless_lock_signing_hash;
use aspens::solana::{gasless_lock_signing_message, OpenOrderArgs};

// Build the canonical order id from a few intent fields:
let order_id = derive_order_id(
    &user_pubkey_bytes, nonce, origin_chain, dest_chain,
    &input_token_bytes, &output_token_bytes, amount_in, amount_out,
);

// EVM: produce the EIP-712 digest a wallet must sign for a gasless lock.
let digest = gasless_lock_signing_hash(&params, arborter, settler, chain_id)?;

// Solana: produce the borsh payload for Ed25519 signing of a gasless open.
let msg = gasless_lock_signing_message(&instance, &user, deadline, &order)?;
```

The pure modules:
- **`aspens::orders`** — chain-agnostic `derive_order_id`, `GaslessLockParams`.
- **`aspens::evm`** — sol! bindings for `MidribV2` / `IAllowanceTransfer` /
  `MidribDataTypes`, EIP-712 domain consts, gasless-order builder and hasher,
  EIP-191 envelope signer.
- **`aspens::solana`** — PDA derivations, instruction builders, borsh payload
  encoder, Ed25519 precompile ix, well-known program ids.

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

## Cargo Feature Flags

The `aspens` crate exposes three orthogonal feature groups, all
default-on. Consumers can trim down to just what they need:

| Feature | What it pulls in | When to keep / drop |
|---------|------------------|---------------------|
| `evm` | `aspens::evm` (sol! bindings, EIP-712 hasher, envelope signer) + `aspens::orders`. Tiny — `alloy-primitives`/`alloy-sol-types`/`alloy-signer-local`. | Keep if you build or sign EVM orders. |
| `solana` | `aspens::solana` (PDA derivations, instruction builders, borsh payload encoder, Ed25519 precompile ix). Pulls `solana-sdk`, `borsh`, `bs58`, `ed25519-dalek`. | Keep if you build or sign Solana orders. |
| `client` | Full runtime: `AspensClient`, trading commands, gRPC (`tonic`/`prost`), async runtime (`tokio`), RPC submission (`solana-client`, `alloy-contract`, `alloy-provider`). | Keep for the CLI/REPL/admin experience or anything that talks to the Aspens stack. Drop it for browser / embedded / offline-signing. |

Common configurations:
- **Default** (everything): `aspens = { path = "..." }`
- **Lean EVM signing**: `aspens = { path = "...", default-features = false, features = ["evm"] }`
- **Lean Solana signing**: `aspens = { path = "...", default-features = false, features = ["solana"] }`
- **Both chains, no client runtime**: `... features = ["evm", "solana"] }`

The `aspens-cli`, `aspens-repl`, and `aspens-admin` binaries all depend
on the default feature set.

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
- **Trading operations** - Deposit, withdraw, buy, sell, balance queries across EVM and Solana chains
- **Curve-agnostic wallet** - `Wallet::Evm` (secp256k1) and `Wallet::Solana` (Ed25519) behind one signing interface
- **Chain dispatch** - `ChainClient` routes RPC calls to Alloy (EVM) or `solana-client` based on chain architecture
- **Executor pattern** - Async/sync execution strategies
- **gRPC client** - Protocol buffer communication with an Aspens Market Stack
- **Client-side order helpers** (`aspens::orders` / `aspens::evm` / `aspens::solana`) — stateless
  builders for the gRPC order payload: `derive_order_id`, EIP-712 gasless-lock hasher (EVM),
  borsh `OpenForSignedPayload` encoder (Solana), PDA derivations, Ed25519 precompile ix.
  Available without the `client` feature for browser / embedded callers.
- **EVM integration** - Midrib V2 ABI bindings (shared JSON artifacts with arborter), Alloy signer, Permit2
- **Solana integration** - Midrib Anchor program: Anchor discriminators, PDA seeds, SPL token flow

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
