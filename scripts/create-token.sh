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
RPC_URL=${3:-http://localhost:8545}

# Convert symbol to lowercase for consistency
symbol=$(echo "$TOKEN_SYMBOL" | tr '[:upper:]' '[:lower:]')
name="${symbol} Token"

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
cp contracts/Token.sol.template contracts/$symbol.sol
sed -i '' "s/TOKEN_NAME/$name/" contracts/$symbol.sol
sed -i '' "s/TOKEN_SYMBOL/$symbol/" contracts/$symbol.sol
sed -i '' "s/TOKEN_DECIMALS/$TOKEN_DECIMALS/" contracts/$symbol.sol

# Compile the contract
forge build

# Deploy the contract to Anvil
ANVIL_PRIVKEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
echo "Deploying $symbol token..."
ADDRESS=$(cast send --private-key $ANVIL_PRIVKEY --rpc-url $RPC_URL --value 0 --create $(cat artifacts/$symbol.sol/Token.json | jq -r .bytecode.object) | grep "contractAddress" | awk '{print $2}')

# Clean up temporary contract
rm contracts/$symbol.sol

echo "Token deployed successfully!"
echo "Name: $name"
echo "Symbol: $symbol"
echo "Decimals: $TOKEN_DECIMALS"
echo "Address: $ADDRESS"
echo "Initial supply: 1,000,000 $symbol"

# Return the address for use in other scripts
echo "$ADDRESS" 
