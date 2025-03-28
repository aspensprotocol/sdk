#!/usr/bin/env just --justfile

# Default recipe to run when just is called without arguments
default:
    @just --list

# Set up the development environment
setup:
    #!/usr/bin/env bash
    cp .env.sample .env
    echo "Please edit .env with your configuration values"
    echo "Then run: source .env"

# Build the project
build:
    cargo build

# Run the CLI
run:
    #!/usr/bin/env bash
    if [ -z "$EVM_TESTNET_PUBKEY" ] || [ -z "$EVM_TESTNET_PRIVKEY" ]; then
        echo "Error: EVM_TESTNET_PUBKEY and EVM_TESTNET_PRIVKEY must be set"
        echo "Please source your .env file first"
        exit 1
    fi
    cargo run

# Run tests
test:
    cargo test

# Clean build artifacts
clean:
    cargo clean
    rm -rf target/
    rm -rf artifacts/

# Format code
fmt:
    cargo fmt

# Check code style
check:
    cargo check

# Run linter
lint:
    cargo clippy

# Set up Anvil environment
setup-anvil:
    #!/usr/bin/env bash
    if ! command -v anvil &> /dev/null; then
        echo "Error: anvil is not installed. Please install foundry first:"
        echo "curl -L https://foundry.paradigm.xyz | bash"
        echo "foundryup"
        exit 1
    fi
    cp .env.anvil.local .env
    echo "Anvil environment set up. Run 'just start-anvil' to start the local chain"

# Start Anvil in the background
start-anvil:
    #!/usr/bin/env bash
    if pgrep -f "anvil.*8545" > /dev/null; then
        echo "Anvil instance on port 8545 is already running"
    else
        anvil --port 8545 &
        echo "Anvil started on http://localhost:8545"
    fi

    if pgrep -f "anvil.*8546" > /dev/null; then
        echo "Anvil instance on port 8546 is already running"
    else
        anvil --port 8546 &
        echo "Anvil started on http://localhost:8546"
    fi

    echo "Waiting for chains to be ready..."
    sleep 2

# Stop Anvil
stop-anvil:
    #!/usr/bin/env bash
    pkill -f "anvil.*8545"
    pkill -f "anvil.*8546"
    echo "Both Anvil instances stopped"

# Create and setup ERC20 tokens
setup-tokens:
    #!/usr/bin/env bash
    if ! command -v cast &> /dev/null; then
        echo "Error: cast is not installed. Please install foundry first:"
        echo "curl -L https://foundry.paradigm.xyz | bash"
        echo "foundryup"
        exit 1
    fi

    # Deploy USDC token on first Anvil instance (port 8545)
    echo "Deploying USDC token on first chain..."
    USDC_ADDRESS=$(cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_1 --create "0x36372b07" --value 0 --rpc-url http://localhost:8545 | grep "contractAddress" | awk '{print $2}')
    echo "USDC deployed at: $USDC_ADDRESS"

    # Deploy WBTC token on first Anvil instance (port 8545)
    echo "Deploying WBTC token on first chain..."
    WBTC_ADDRESS=$(cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_1 --create "0x36372b07" --value 0 --rpc-url http://localhost:8545 | grep "contractAddress" | awk '{print $2}')
    echo "WBTC deployed at: $WBTC_ADDRESS"

    # Deploy USDT token on second Anvil instance (port 8546)
    echo "Deploying USDT token on second chain..."
    USDT_ADDRESS=$(cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_2 --create "0x36372b07" --value 0 --rpc-url http://localhost:8546 | grep "contractAddress" | awk '{print $2}')
    echo "USDT deployed at: $USDT_ADDRESS"

    # Mint USDC to first account on first chain
    echo "Minting USDC to first account on first chain..."
    cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_1 --rpc-url http://localhost:8545 $USDC_ADDRESS "mint(address,uint256)" $EVM_TESTNET_PUBKEY_ACCOUNT_1 1000000000000

    # Mint WBTC to first account on first chain (100 WBTC with 8 decimals)
    echo "Minting WBTC to first account on first chain..."
    cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_1 --rpc-url http://localhost:8545 $WBTC_ADDRESS "mint(address,uint256)" $EVM_TESTNET_PUBKEY_ACCOUNT_1 10000000000

    # Mint USDT to second account on second chain
    echo "Minting USDT to second account on second chain..."
    cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_2 --rpc-url http://localhost:8546 $USDT_ADDRESS "mint(address,uint256)" $EVM_TESTNET_PUBKEY_ACCOUNT_2 1000000000000

    # Update market IDs in .env.anvil.local with actual token addresses
    echo "Updating market IDs in .env.anvil.local..."
    sed -i '' "s/<WBTC_ADDRESS>/$WBTC_ADDRESS/" .env.anvil.local
    sed -i '' "s/<USDC_ADDRESS>/$USDC_ADDRESS/" .env.anvil.local
    sed -i '' "s/<USDT_ADDRESS>/$USDT_ADDRESS/" .env.anvil.local

    echo "Tokens deployed and minted successfully!"
    echo "USDC on chain 1 (port 8545): $USDC_ADDRESS"
    echo "WBTC on chain 1 (port 8545): $WBTC_ADDRESS"
    echo "USDT on chain 2 (port 8546): $USDT_ADDRESS"
    echo "First account has:"
    echo "  - 1,000,000 USDC on chain 1"
    echo "  - 100 WBTC on chain 1"
    echo "Second account has:"
    echo "  - 1,000,000 USDT on chain 2"
    echo "Market IDs have been updated in .env.anvil.local"

# Full Anvil setup with tokens
setup-anvil-full:
    just setup-anvil
    just start-anvil
    just setup-tokens

# Create and deploy a new ERC20 token
create-token symbol decimals:
    ./create-token.sh {{symbol}} {{decimals}}