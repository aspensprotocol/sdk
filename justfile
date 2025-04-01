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

# Set up Anvil and deploy test tokens
setup-anvil-full:
    #!/usr/bin/env bash
    # Start two Anvil instances
    anvil --port 8545 --chain-id 84531 --mnemonic "test test test test test test test test test test test junk" > anvil1.log 2>&1 &
    anvil --port 8546 --chain-id 84532 --mnemonic "test test test test test test test test test test test junk" > anvil2.log 2>&1 &
    # Wait for Anvil to start
    sleep 2
    # Deploy USDC and WBTC on the first chain
    forge create --rpc-url http://localhost:8545 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 contracts/USDC.sol:USDC
    forge create --rpc-url http://localhost:8545 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 contracts/WBTC.sol:WBTC
    # Deploy USDT on the second chain
    forge create --rpc-url http://localhost:8546 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 contracts/USDT.sol:USDT
    # Mint initial tokens to test accounts
    cast send --rpc-url http://localhost:8545 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 0x1234567890123456789012345678901234567890 "mint(address,uint256)" 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 1000000000000000000000000
    cast send --rpc-url http://localhost:8545 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 0x1234567890123456789012345678901234567890 "mint(address,uint256)" 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 1000000000000000000000000
    cast send --rpc-url http://localhost:8546 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 0x1234567890123456789012345678901234567890 "mint(address,uint256)" 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 1000000000000000000000000

# Stop Anvil instances
stop-anvil:
    #!/usr/bin/env bash
    pkill -f anvil

# Create a new ERC20 token
create-token symbol decimals:
    ./create-token.sh {{symbol}} {{decimals}}

# Run the CLI in interactive mode
cli:
    cd app && cargo run

# Run the CLI in scripted mode with arguments
run *args:
    cd app && cargo run {{args}}