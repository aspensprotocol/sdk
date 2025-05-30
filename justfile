#!/usr/bin/env just --justfile

# List all available commands
default:
    @just --list

# Set up the development environment
setup:
    #!/usr/bin/env bash
    cp sdk/.env.sample app/.env
    echo "Please edit sdk/.env with your configuration values"
    echo "Then run: source sdk/.env"

# Build the project
build:
    cd sdk && cargo build

release:
    cd sdk && cargo build --release

# Run tests
test:
    cd sdk && cargo test

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    cd sdk && cargo clean
    rm -rf sdk/target
    rm -rf sdk/out
    rm -rf sdk/anvil*.log
    rm -rf artifacts/

# Format code
fmt:
    cd sdk && cargo fmt

# Check code style
check:
    cd sdk && cargo check

# Run linter
lint:
    cd sdk && cargo clippy

# Run the CLI in interactive mode
cli:
    cd sdk && cargo run

# Run the CLI in scripted mode with arguments
run *args:
    cd sdk && cargo run {{args}}
