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

# Environment management commands
env-list:
    #!/usr/bin/env bash
    ./scripts/env-switch.sh --list

env-switch env:
    #!/usr/bin/env bash
    ./scripts/env-switch.sh {{env}}

# Run CLI with specific environment
cli-anvil *args:
    cargo run -p aspens-cli -- --env anvil {{args}}

cli-testnet *args:
    cargo run -p aspens-cli -- --env testnet {{args}}

# Run REPL with specific environment
repl-anvil:
    cargo run -p aspens-repl -- --env anvil

repl-testnet:
    cargo run -p aspens-repl -- --env testnet

# Run AMMIT tests with specific environment
test-anvil:
    ./scripts/ammit.sh anvil

test-testnet:
    ./scripts/ammit.sh testnet

# Create environment template
create-env name:
    #!/usr/bin/env bash
    if [[ -f ".env.{{name}}.local" ]]; then
        echo "Environment file .env.{{name}}.local already exists!"
        exit 1
    fi

    echo "# {{name}} Environment Configuration" > .env.{{name}}.local
    echo "# Copy this file and customize the values for your {{name}} setup" >> .env.{{name}}.local
    echo "" >> .env.{{name}}.local
    echo "# Market IDs" >> .env.{{name}}.local
    echo "MARKET_ID_1={{name}}_wbtc_usdt" >> .env.{{name}}.local
    echo "MARKET_ID_2={{name}}_usdc_usdt" >> .env.{{name}}.local
    echo "" >> .env.{{name}}.local
    echo "# RPC URLs" >> .env.{{name}}.local
    echo "ETHEREUM_RPC_URL=https://your-rpc-url" >> .env.{{name}}.local
    echo "POLYGON_RPC_URL=https://your-rpc-url" >> .env.{{name}}.local
    echo "" >> .env.{{name}}.local
    echo "# Contract addresses" >> .env.{{name}}.local
    echo "ARBORTER_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890" >> .env.{{name}}.local
    echo "USDC_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890" >> .env.{{name}}.local
    echo "USDT_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890" >> .env.{{name}}.local
    echo "WBTC_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890" >> .env.{{name}}.local
    echo "" >> .env.{{name}}.local
    echo "# Private keys (use test accounts only!)" >> .env.{{name}}.local
    echo "PRIVATE_KEY_1=0x1234567890123456789012345678901234567890123456789012345678901234" >> .env.{{name}}.local
    echo "PRIVATE_KEY_2=0x1234567890123456789012345678901234567890123456789012345678901234" >> .env.{{name}}.local
    echo "" >> .env.{{name}}.local
    echo "# Configuration" >> .env.{{name}}.local
    echo "CHAIN_ID=1" >> .env.{{name}}.local
    echo "GAS_LIMIT=3000000" >> .env.{{name}}.local
    echo "GAS_PRICE=20000000000" >> .env.{{name}}.local

    echo "Created .env.{{name}}.local template"
    echo "Please edit the file with your actual configuration values"
