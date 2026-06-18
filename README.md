# Aspens SDK

[![crates.io](https://img.shields.io/crates/v/aspens.svg)](https://crates.io/crates/aspens)
[![docs.rs](https://docs.rs/aspens/badge.svg)](https://docs.rs/aspens)

A comprehensive SDK and CLI tools for interacting with an [Aspens Markets Stack](https://docs.aspens.xyz).

The core library is published on crates.io as [`aspens`](https://crates.io/crates/aspens).
The `aspens-cli`, `aspens-repl`, and `aspens-admin` binaries live in this
workspace; `aspens-cli` and `aspens-repl` ship as prebuilt release binaries
(see below), and all three can be built from source.

## Install

Install the latest `aspens-cli` and `aspens-repl` (Linux / macOS, x86_64 / aarch64):

```sh
curl -fsSL https://raw.githubusercontent.com/aspensprotocol/sdk/main/install.sh | sh
```

Overrides: `INSTALL_DIR=<dir>` picks the location (default `/usr/local/bin` on
Linux, `~/.local/bin` on macOS); `ASPENS_VERSION=vX.Y.Z` pins a specific release.
Binaries are attached to each [GitHub release](https://github.com/aspensprotocol/sdk/releases)
alongside a `SHA256SUMS` file.

Or build from source (also the path for `aspens-admin`):

```sh
cargo install --locked --git https://github.com/aspensprotocol/sdk aspens-cli aspens-repl
```

## Available Commands

### Trader commands (`aspens-cli` / `aspens-repl`)

| Command | Description |
|---------|-------------|
| `config [--output-file <path>]` | Fetch and display the configuration from the server (saves to `.json` / `.toml` if `--output-file` is set) |
| `deposit <network> <token> <amount>` | Deposit tokens to make them available for trading |
| `withdraw <network> <token> <amount>` | Withdraw tokens to a local wallet |
| `buy-market <market> <amount>` | Send a market BUY order (executes at best available price) |
| `buy-limit <market> <amount> <price> [--post-only]` | Send a limit BUY order (executes at specified price or better). With `--post-only`, the order is rejected if it would cross at submission — guarantees maker-side execution. |
| `sell-market <market> <amount>` | Send a market SELL order (executes at best available price) |
| `sell-limit <market> <amount> <price> [--post-only]` | Send a limit SELL order (executes at specified price or better). See `--post-only` above. |
| `buy-marketable <market> <amount> [--slippage-bps <bps>]` | **CLI only.** Snapshot the resting book, cap slippage above best ask (default 50 bps = 0.5%), submit as a buy-limit. The gasless cross-chain protocol rejects true market orders; this turns "take the top of book with a slippage cap" into the equivalent priced order. |
| `sell-marketable <market> <amount> [--slippage-bps <bps>]` | **CLI only.** Same as `buy-marketable`, but capping slippage below best bid. |
| `cancel-order <market> <side> <order_id>` | Cancel an existing order by its ID |
| `stream-orderbook <market> [--historical] [--trader <addr>]` | Stream orderbook entries in real-time |
| `stream-trades <market> [--historical] [--trader <addr>]` | Stream executed trades in real-time |
| `balance` | Fetch the current balances for all supported tokens across all chains |
| `status` | Show current configuration and connection status |
| `trader-public-key` | Get the public key and address for the trader wallet |
| `signer-public-key [--chain-network <network>]` | Get the signer public key(s) for the trading instance (filtered to a chain network if provided) |
| `get-attestation [--report-data <hex>] [-o text\|json]` | Fetch the TEE attestation report from the signer; optionally bind up to 64 bytes of user-supplied data into the report |

All commands above are available in both `aspens-cli` and `aspens-repl`, except `buy-marketable` / `sell-marketable` which are CLI-only. The REPL also adds a `quit` command to exit the session.

### Admin commands (`aspens-admin`)

Most commands below require a JWT (set via `--jwt`, `ASPENS_JWT` in `.env`, or the `aspens-admin login` flow).

| Command | Description |
|---------|-------------|
| `init-admin --address <eth-address>` | Initialize the first admin on a fresh stack (no JWT required) |
| `login [--chain-id <id>]` | Authenticate via EIP-712 signature using `ADMIN_PRIVKEY` and obtain a JWT |
| `update-admin --address <eth-address>` | Update the admin address |
| `set-chain --architecture … --name … --network … --chain-id … --rpc-url … --factory-address … --permit2-address … [--block-explorer-url …] [--instance-signer-address …]` | Add or update a chain entry |
| `delete-chain --network <network>` | Remove a chain from the configuration |
| `set-token --network … --name … --symbol … --address … --decimals … [--token-id …]` | Add or update a token on a chain |
| `delete-token --network <network> --symbol <symbol>` | Remove a token from a chain |
| `set-market --base-network … --quote-network … --base-symbol … --quote-symbol … --base-address … --quote-address … --base-decimals … --quote-decimals … --pair-decimals …` | Add or update a market |
| `delete-market --market-id <id>` | Remove a market |
| `deploy-contract --network <network> --fee-pct <bps>` | Deploy a trade contract on a chain (fee in basis points) |
| `set-trade-contract --address <addr> --network <network>` | Register an existing trade contract address on a chain |
| `delete-trade-contract --network <network>` | Remove the trade contract association from a chain |
| `version` | Show server version information |
| `status` | Show current configuration and connection status |
| `admin-public-key` | Get the public key and address for the admin wallet (from `ADMIN_PRIVKEY`) |
| `balances` | Show balances for owner, signers, and contracts across all chains |

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

Install from crates.io:
```bash
cargo add aspens
```

Or add it manually to your `Cargo.toml`:
```toml
[dependencies]
aspens = "0.6"
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
```bash
cargo add aspens --no-default-features --features evm,solana
```
```toml
[dependencies]
aspens = { version = "0.6", default-features = false, features = ["evm", "solana"] }
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

### Post-only orders

Pass `--post-only` to `buy-limit` / `sell-limit` to guarantee your order
adds liquidity rather than taking it. If the price would cross the
opposing side of the book at submission, arborter returns
`FAILED_PRECONDITION` and **does not** lock funds on-chain — no gas is
spent and your gasless signature stays unused, so you can resubmit at a
different price.

```bash
# Post a maker-only bid at 100. If the best ask is ≤ 100, the order
# is rejected and you can retry at 99 (or below).
aspens-cli buy-limit USDC/USDT 1.5 100 --post-only

# Same on the sell side: rejected if there's a resting bid at ≥ 200.
aspens-cli sell-limit USDC/USDT 1.5 200 --post-only
```

In Rust:

```rust
use aspens::commands::trading::send_order;

let response = send_order::send_order_with_wallet(
    stack_url,
    market_id,
    1,                     // 1 = BUY
    "1.5".into(),          // quantity
    Some("100".into()),    // limit price (required for post-only)
    &wallet,
    config,
    true,                  // post_only
).await?;
```

Post-only is incompatible with market orders (the SDK pre-rejects
`post_only=true` with `price=None` before signing) and with the
`buy-marketable` / `sell-marketable` CLI variants (which are designed
to cross — the CLI hard-codes `post_only=false` for them).

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
- **Default** (everything): `aspens = "0.6"`
- **Lean EVM signing**: `aspens = { version = "0.6", default-features = false, features = ["evm"] }`
- **Lean Solana signing**: `aspens = { version = "0.6", default-features = false, features = ["solana"] }`
- **Both chains, no client runtime**: `aspens = { version = "0.6", default-features = false, features = ["evm", "solana"] }`

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

## Token Assumptions

**Important:** Aspens only supports tokens with **standard ERC-20 / SPL semantics**. Adding a non-compliant token to a market — via `aspens-admin set-token` or the admin-console — will produce incorrect balances, fee leakage, or stuck funds. The contracts do **not** detect non-compliant tokens; gating happens here, in market configuration.

A token is safe to add **only if all of the following hold**:

- **Standard transfer semantics.** A `transfer(to, amount)` reduces the sender's balance by exactly `amount`. No transfer hooks that re-enter or opportunistically revert.
- **No fee-on-transfer.** Tokens that charge a fee on transfer (reflection tokens, deflationary tokens) silently shift cost onto the user's existing `tradeBalance` during `_depositAndLock`, cause the Aspens vault to under-collect fees, and under-deliver `SETTLE_AND_WITHDRAW` payouts.
- **No rebasing.** Tokens whose balances change between two reads of `balanceOf` (AMPL-style, aTokens in rebase mode) break the `balanceBefore`/`balanceAfter` reconciliation used throughout the contract. Use the wrapped, non-rebasing variant (e.g. wstETH, not stETH).
- **No supply-pause that strands open orders.** Pausable tokens are tolerable as long as pauses are short-lived; pauses of a duration longer than the cancel-and-unlock window can strand `lockedTradeBalance` until the pause lifts.
- **Blocklist tokens (USDC-style) are accepted with caveats.** Funds remain correctly accounted for, but a blocklisted address cannot withdraw or settle out until removed from the list.

Quick checklist before running `aspens-admin set-token`:

1. Read the token's `transfer` implementation — confirm `_balances[from] -= amount` is the only debit.
2. Run a probe transfer (any amount) and check `balanceAfter == balanceBefore - amount` on both sides.
3. Confirm `balanceOf` is a pure function of stored state, not a function of total supply.

If any of these checks fails, **do not add the token**. Common safe examples: USDC, USDT (on chains where USDT does not enable fee-on-transfer), WBTC, WETH, DAI, most stablecoins.

### Solana-specific notes

The on-chain `midrib` program uses the **legacy SPL Token program**, not Token-2022. Mints owned by the Token-2022 program (`TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`) will fail deserialization at every entry point — by design, since Token-2022's transfer-fee, interest-bearing, and confidential-transfer extensions would all break the program's `deposited += amount` accounting.

For gasless `open_for` flows the user signs an `OpenForSignedPayload` with an `args.deadline` slot. Pick this tight — `current_slot + 600` (~4 minutes at 400ms slots) is a sensible default. The on-chain `UsedNonce` tombstone guarantees a signed payload is single-use regardless of deadline, but a tight deadline limits the window between user-signs and arborter-submits.



## Documentation

- [Decimal Conversion Guide](decimals.md) - Understanding decimal handling
- [CHANGELOG.md](CHANGELOG.md) - Release notes per version
- [CLAUDE.md](CLAUDE.md) - Architecture guide for development

## Versioning

The `aspens` crate follows [Semantic Versioning](https://semver.org/). The
workspace is pre-1.0, so the conventions in effect today are:

- **Patch releases (`0.4.x` → `0.4.y`)** — bug fixes, performance work,
  internal refactors. No source-breaking changes to public items in
  `aspens::{client, wallet, orders, evm, solana, decimals}` or to the
  re-exports at the crate root.
- **Minor releases (`0.4.x` → `0.5.0`)** — may include breaking changes
  to the public API surface (renames, signature changes, removals).
  Notable changes are recorded in [`CHANGELOG.md`](CHANGELOG.md).
- **Internal modules** — `aspens::grpc` (and any module marked
  `#[doc(hidden)]` or `pub(crate)`) are implementation details and may
  change in any release. Generated proto bindings under
  `aspens::proto::*` and `aspens::attestation::*` track the upstream
  `protos/` repo and follow its compatibility, not the SDK's.
- **CLI / REPL / Admin binaries** — version-bumped together with the
  library. Flag and command renames are called out in `CHANGELOG.md`.

When in doubt about whether a change is breaking, check the changelog
entry for the target version.

## License

This project is licensed under the Apache License 2.0. See the [LICENSE](LICENSE) file for details.
