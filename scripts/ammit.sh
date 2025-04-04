#!/bin/bash

# Automated Market Maker Internal Tester (AMMIT)
# This script automates testing of the Aspens CLI by setting up a local Anvil environment,
# deploying test tokens, and testing AMM operations including deposits, orders, and withdrawals.

# Exit on error
set -e pipefail

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Function to print section headers
print_section() {
    echo -e "\n${GREEN}=== $1 ===${NC}"
}

# Function to check if a command exists
check_command() {
    if ! command -v $1 &> /dev/null; then
        echo -e "${RED}Error: $1 is not installed${NC}"
        exit 1
    fi
}

# Check prerequisites
check_command "cargo"
check_command "just"

# Function to run CLI command and check result
run_cli_command() {
    local args="$@"
    echo -e "${GREEN}Running: cargo run --bin aspens-cli $args${NC}"
    cd app && cargo run --bin aspens-cli $args && cd ..
}

# Main test sequence
main() {
    source .env.anvil.local
    # print_section "Setting up Anvil environment"
    # just setup-anvil-full
    #
    print_section "Fetching config"
    run_cli_command "get-config"

    print_section "Checking balances"
    run_cli_command "get-balance"

    print_section "Depositing tokens"
    # Deposit USDC and WBTC from first account
    run_cli_command "deposit --market-id $MARKET_ID_2 --amount 100000000000 --token-index 0"
    run_cli_command "deposit --market-id $MARKET_ID_1 --amount 100000000 --token-index 0"
    
    # Deposit USDT from second account
    run_cli_command "deposit --market-id $MARKET_ID_1 --amount 100000000000 --token-index 1"
    run_cli_command "deposit --market-id $MARKET_ID_2 --amount 100000000000 --token-index 1"

    print_section "Checking balances after deposits"
    run_cli_command "balance"

    print_section "Sending orders"
    # Send buy orders for WBTC/USDT
    run_cli_command "send-order --market-id $MARKET_ID_1 --side buy --amount 100000000 --price 50000"
    run_cli_command "send-order --market-id $MARKET_ID_1 --side buy --amount 100000000 --price 49900"
    
    # Send sell orders for WBTC/USDT
    run_cli_command "send-order --market-id $MARKET_ID_1 --side sell --amount 100000000 --price 50100"
    run_cli_command "send-order --market-id $MARKET_ID_1 --side sell --amount 100000000 --price 50200"

    # Send orders for USDC/USDT
    run_cli_command "send-order --market-id $MARKET_ID_2 --side buy --amount 100000000000 --price 100"
    run_cli_command "send-order --market-id $MARKET_ID_2 --side sell --amount 100000000000 --price 101"

    print_section "Checking balances after orders"
    run_cli_command "balance"

    print_section "Withdrawing tokens"
    # Withdraw tokens from first account
    run_cli_command "withdraw --market-id $MARKET_ID_2 --amount 50000000000 --token-index 0"
    run_cli_command "withdraw --market-id $MARKET_ID_1 --amount 50000000 --token-index 0"
    
    # Withdraw tokens from second account
    run_cli_command "withdraw --market-id $MARKET_ID_1 --amount 50000000000 --token-index 1"
    run_cli_command "withdraw --market-id $MARKET_ID_2 --amount 50000000000 --token-index 1"

    print_section "Final balance check"
    run_cli_command "balance"

    print_section "Cleaning up"
    just stop-anvil

    echo -e "\n${GREEN}Test sequence completed successfully!${NC}"
}

# Run main function
main 
