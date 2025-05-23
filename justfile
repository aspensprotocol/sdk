#!/usr/bin/env just --justfile

# List all available commands
default:
    @just --list

# Set up the development environment
setup:
    #!/usr/bin/env bash
    cp app/.env.sample app/.env
    echo "Please edit app/.env with your configuration values"
    echo "Then run: source app/.env"

# Build the project
build:
    cd app && cargo build

release:
    cd app && cargo build --release

# Run tests
test:
    cd app && cargo test

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    cd app && cargo clean
    rm -rf app/target
    rm -rf app/out
    rm -rf app/anvil*.log
    rm -rf artifacts/

# Format code
fmt:
    cd app && cargo fmt

# Check code style
check:
    cd app && cargo check

# Run linter
lint:
    cd app && cargo clippy

# Run the CLI in interactive mode
cli:
    cd app && cargo run

# Run the CLI in scripted mode with arguments
run *args:
    cd app && cargo run {{args}}
