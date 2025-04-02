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

# Set up Anvil and deploy test tokens
setup-anvil-full:
    #!/usr/bin/env bash
    # Start two Anvil instances
    anvil --port 8545 --chain-id 84531 --mnemonic "test test test test test test test test test test test junk" > anvil1.log 2>&1 &
    anvil --port 8546 --chain-id 84532 --mnemonic "test test test test test test test test test test test junk" > anvil2.log 2>&1 &
    ANVIL_PRIVKEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
    ANVIL_ADDR1=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
    ANVIL_ADDR2=0x70997970C51812dc3A010C7d01b50e0d17dc79C8

    # Wait for Anvil to start
    sleep 2

    echo "Starting token deployment and distribution..."

    # Deploy USDC and WBTC on the first chain
    echo "Deploying tokens on Base Sepolia (port 8545)..."
    USDC_TOKEN_ADDRESS=$(./scripts/create-token.sh USDC 6 http://localhost:8545 | tail -n 1)
    WBTC_TOKEN_ADDRESS=$(./scripts/create-token.sh WBTC 8 http://localhost:8545 | tail -n 1)

    # Deploy USDT on the second chain
    echo "Deploying tokens on Optimism Sepolia (port 8546)..."
    USDT_TOKEN_ADDRESS=$(./scripts/create-token.sh USDT 6 http://localhost:8546 | tail -n 1)

    echo "Distributing tokens to test accounts..."

    # Mint and transfer tokens to test accounts on first chain
    echo "Base Sepolia (port 8545):"
    echo "  Transferring 1,000,000 USDC to $ANVIL_ADDR1"
    cast send --rpc-url http://localhost:8545 --private-key $ANVIL_PRIVKEY $USDC_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR1" "1000000000000"
    echo "  Transferring 1,000,000 USDC to $ANVIL_ADDR2"
    cast send --rpc-url http://localhost:8545 --private-key $ANVIL_PRIVKEY $USDC_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR2" "1000000000000"
    
    echo "  Transferring 1,000,000 WBTC to $ANVIL_ADDR1"
    cast send --rpc-url http://localhost:8545 --private-key $ANVIL_PRIVKEY $WBTC_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR1" "100000000000000"
    echo "  Transferring 1,000,000 WBTC to $ANVIL_ADDR2"
    cast send --rpc-url http://localhost:8545 --private-key $ANVIL_PRIVKEY $WBTC_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR2" "100000000000000"

    # Mint and transfer tokens to test accounts on second chain
    echo "Optimism Sepolia (port 8546):"
    echo "  Transferring 1,000,000 USDT to $ANVIL_ADDR1"
    cast send --rpc-url http://localhost:8546 --private-key $ANVIL_PRIVKEY $USDT_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR1" "1000000000000"
    echo "  Transferring 1,000,000 USDT to $ANVIL_ADDR2"
    cast send --rpc-url http://localhost:8546 --private-key $ANVIL_PRIVKEY $USDT_TOKEN_ADDRESS "transfer(address,uint256)" "$ANVIL_ADDR2" "1000000000000"

    echo -e "\nSetup complete! Token addresses:"
    echo "Base Sepolia (port 8545):"
    echo "  USDC: $USDC_TOKEN_ADDRESS"
    echo "  WBTC: $WBTC_TOKEN_ADDRESS"
    echo "Optimism Sepolia (port 8546):"
    echo "  USDT: $USDT_TOKEN_ADDRESS"
    echo -e "\nTest accounts:"
    echo "  Address 1: $ANVIL_ADDR1"
    echo "  Address 2: $ANVIL_ADDR2"
    echo -e "\nToken distribution summary:"
    echo "  Each address received:"
    echo "    - 1,000,000 USDC (6 decimals)"
    echo "    - 1,000,000 WBTC (8 decimals)"
    echo "    - 1,000,000 USDT (6 decimals)"

# Stop Anvil instances
stop-anvil:
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
