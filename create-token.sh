#!/bin/bash

# Check if symbol is provided
if [ -z "$1" ]; then
    echo "Error: Symbol is required"
    echo "Usage: ./create-token.sh SYMBOL [DECIMALS]"
    echo "  SYMBOL: Token symbol (e.g., BTC, ETH)"
    echo "  DECIMALS: Number of decimal places (default: 18)"
    exit 1
fi

# Get arguments
TOKEN_SYMBOL=$1
TOKEN_DECIMALS=${2:-18}

# Convert symbol to lowercase for consistency
symbol=$(echo "$TOKEN_SYMBOL" | tr '[:upper:]' '[:lower:]')
name="${symbol^} Token"

# Check if forge is installed
if ! command -v forge &> /dev/null; then
    echo "Error: forge is not installed. Please install foundry first:"
    echo "curl -L https://foundry.paradigm.xyz | bash"
    echo "foundryup"
    exit 1
fi

# Install OpenZeppelin contracts if not already installed
if [ ! -d "lib/openzeppelin-contracts" ]; then
    forge install OpenZeppelin/openzeppelin-contracts
fi

# Create a copy of the template and update it with the token details
cp contracts/TempToken.sol contracts/TempToken.sol.tmp
sed -i '' "s/TOKEN_NAME/$name/" contracts/TempToken.sol.tmp
sed -i '' "s/TOKEN_SYMBOL/$symbol/" contracts/TempToken.sol.tmp
sed -i '' "s/TOKEN_DECIMALS/$TOKEN_DECIMALS/" contracts/TempToken.sol.tmp

# Compile the contract
forge build

# Deploy the contract to Anvil
echo "Deploying $symbol token..."
ADDRESS=$(cast send --private-key $EVM_TESTNET_PRIVKEY_ACCOUNT_1 --create $(cat artifacts/TempToken.sol/TempToken.json | jq -r .bytecode.object) --value 0 --rpc-url http://localhost:8545 | grep "contractAddress" | awk '{print $2}')

# Clean up temporary contract
rm contracts/TempToken.sol.tmp

echo "Token deployed successfully!"
echo "Name: $name"
echo "Symbol: $symbol"
echo "Decimals: $TOKEN_DECIMALS"
echo "Address: $ADDRESS"
echo "Initial supply: 1,000,000 $symbol" 