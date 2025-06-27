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

# Environment management commands
env-list:
    #!/usr/bin/env bash
    ./scripts/env-switch.sh --list

env-switch env:
    #!/usr/bin/env bash
    ./scripts/env-switch.sh {{env}}

# Run CLI with specific environment
cli-anvil *args:
    cd wrappers && cargo run --bin aspens-cli -- --env anvil {{args}}

cli-testnet *args:
    cd wrappers && cargo run --bin aspens-cli -- --env testnet {{args}}

# Run REPL with specific environment
repl-anvil:
    cd wrappers && cargo run --bin aspens-repl -- --env anvil

repl-testnet:
    cd wrappers && cargo run --bin aspens-repl -- --env testnet

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
