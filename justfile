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

# Run AMMIT tests with specific environment
test-anvil:
    ./scripts/ammit.sh anvil

test-testnet:
    ./scripts/ammit.sh testnet
