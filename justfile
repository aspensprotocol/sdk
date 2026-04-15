#!/usr/bin/env just --justfile

# List all available commands
default:
    @just --list

# Set up the development environment
init:
    #!/usr/bin/env bash
    cp .env.sample .env
    echo "Please edit the created .env with your account values"

# Build the entire workspace
build:
    cargo build

# Build workspace in release mode
release:
    cargo build --release

# Build only the core library
build-lib:
    cargo build -p aspens

# Build only the CLI
build-cli:
    cargo build -p aspens-cli

# Build only the REPL
build-repl:
    cargo build -p aspens-repl

# Build only the Admin 
build-admin:
    cargo build -p aspens-admin

# Run tests for the entire workspace
test:
    cargo test

# Run tests for core library only
test-lib:
    cargo test -p aspens

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    cargo clean
    rm -rf target
    rm -rf anvil*.log
    rm -rf artifacts/

# Format code for the entire workspace
fmt:
    cargo fmt --all

# Check code style for the entire workspace
check:
    cargo check --workspace

# Run linter on the entire workspace
lint:
    cargo clippy --workspace

# Run `cargo fmt` in check mode (matches CI)
fmt-check:
    cargo fmt --all -- --check

# In preparation for CI passing for a Pull Request, run lint, fmt-check, and test
prep-for-pr: lint fmt-check test

# Run the live SDK↔arborter gasless-order round-trip test.
# Requires: running arborter at $ASPENS_MARKET_STACK_URL, admin-configured
# chains + markets, deployed Midrib program / MidribV2 per chain,
# mock-signer running, and the trader wallet already having deposited
# balance on the origin chain. See aspens/tests/send_order_live.rs header.
#
# Env driving the call:
#   SDK_LIVE_TEST_MARKET_ID   — required, shorthand or full market id
#   SDK_LIVE_TEST_SIDE        — BID|ASK (default ASK)
#   SDK_LIVE_TEST_QUANTITY    — default "0.001"
#   SDK_LIVE_TEST_PRICE       — omit for market order
test-live-send-order:
    cargo test -p aspens --test send_order_live --all-features -- --ignored --nocapture

# Run AMMIT tests with specific environment
test-anvil:
    ./scripts/ammit.sh anvil

test-testnet:
    ./scripts/ammit.sh testnet
