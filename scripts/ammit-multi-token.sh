#!/bin/bash

# AMMIT Multi-Token Test Script
#
# This is an advanced example showing how to test with multiple tokens.
# This script demonstrates depositing, trading, and withdrawing different tokens
# across multiple chains.
#
# Prerequisites:
# - Configure token addresses in your .env file:
#   BASE_CHAIN_USDC_TOKEN_ADDRESS=0x...
#   BASE_CHAIN_WETH_TOKEN_ADDRESS=0x...
#   BASE_CHAIN_WBTC_TOKEN_ADDRESS=0x...
#   QUOTE_CHAIN_USDC_TOKEN_ADDRESS=0x...
#   QUOTE_CHAIN_WETH_TOKEN_ADDRESS=0x...

set -e
set -o pipefail

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

ENV_NAME="${1:-anvil}"
SERVER_URL="${2:-http://localhost:50051}"

echo -e "${GREEN}🚀 AMMIT Multi-Token Test Suite${NC}"
echo -e "${BLUE}Environment: $ENV_NAME${NC}"
echo -e "${BLUE}Server: $SERVER_URL${NC}"

run_cmd() {
    echo -e "${BLUE}→ $@${NC}"
    cargo run -p aspens-cli -- --env $ENV_NAME "$@"
}

# Initialize
echo -e "\n${GREEN}=== Initializing ===${NC}"
run_cmd initialize $SERVER_URL
run_cmd status

# Initial balances
echo -e "\n${GREEN}=== Initial Balances ===${NC}"
run_cmd balance

# Deposit multiple tokens on base chain
echo -e "\n${GREEN}=== Depositing Multiple Tokens (Base Chain) ===${NC}"
run_cmd deposit base usdc 1000000
run_cmd deposit base weth 100
run_cmd deposit base wbtc 50

# Deposit multiple tokens on quote chain
echo -e "\n${GREEN}=== Depositing Multiple Tokens (Quote Chain) ===${NC}"
run_cmd deposit quote usdc 1000000
run_cmd deposit quote weth 100

# Check balances after deposits
echo -e "\n${GREEN}=== Balances After Deposits ===${NC}"
run_cmd balance

# Place orders for different token pairs
echo -e "\n${GREEN}=== Trading Multiple Markets ===${NC}"
echo "Testing USDC market..."
run_cmd buy 100 --limit-price 1.00
run_cmd sell 100 --limit-price 1.01

echo "Testing WETH market (if configured)..."
# Uncomment if you have WETH market configured
# run_cmd buy 10 --limit-price 2000 --market $WETH_MARKET_ID
# run_cmd sell 10 --limit-price 2100 --market $WETH_MARKET_ID

# Check balances after trading
echo -e "\n${GREEN}=== Balances After Trading ===${NC}"
run_cmd balance

# Withdraw different tokens
echo -e "\n${GREEN}=== Withdrawing Multiple Tokens ===${NC}"
run_cmd withdraw base usdc 500000
run_cmd withdraw base weth 50
run_cmd withdraw quote usdc 500000

# Final balances
echo -e "\n${GREEN}=== Final Balances ===${NC}"
run_cmd balance

echo -e "\n${GREEN}✅ Multi-token test completed!${NC}"
echo -e "${GREEN}Tested with: USDC, WETH, WBTC${NC}"
