#!/bin/bash

# Automated Market Maker Internal Tester (AMMIT)
# This script automates testing of the Aspens CLI by setting up a local Anvil environment,
# deploying test tokens, and testing AMM operations including deposits, orders, and withdrawals.

# Exit on error
set -e pipefail

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default environment
DEFAULT_ENV="anvil"

# Function to print section headers
print_section() {
    echo -e "\n${GREEN}=== $1 ===${NC}"
}

# Function to print colored output
print_info() {
    echo -e "${GREEN}$1${NC}"
}

print_warning() {
    echo -e "${YELLOW}$1${NC}"
}

print_error() {
    echo -e "${RED}$1${NC}"
}

# Function to show usage
show_usage() {
    echo "Usage: $0 [environment_name]"
    echo ""
    echo "Available environments:"
    echo "  anvil    - Local Anvil development environment (default)"
    echo "  testnet  - Testnet environment"
    echo ""
    echo "Examples:"
    echo "  $0 anvil"
    echo "  $0 testnet"
    echo ""
    echo "You can also set the ASPENS_ENV environment variable:"
    echo "  export ASPENS_ENV=testnet"
    echo "  $0"
}

# Function to check if a command exists
check_command() {
    if ! command -v $1 &> /dev/null; then
        print_error "Error: $1 is not installed"
        exit 1
    fi
}

# Function to check if environment file exists
check_env_file() {
    local env_name=$1
    local env_file=".env.${env_name}.local"
    
    if [[ ! -f "$env_file" ]]; then
        print_error "Environment file $env_file not found!"
        print_warning "Please create $env_file with your configuration values."
        return 1
    fi
    
    return 0
}

# Function to run CLI command and check result
run_cli_command() {
    local args="$@"
    echo -e "${GREEN}Running: cargo run --bin aspens-cli -- --env $ENV_NAME $args${NC}"
    cd wrappers && cargo run --bin aspens-cli -- --env $ENV_NAME $args && cd ..
}

# Main test sequence
main() {
    # Parse command line arguments
    if [[ "$1" == "-h" || "$1" == "--help" ]]; then
        show_usage
        exit 0
    fi
    
    # Determine environment to use
    ENV_NAME="${1:-$DEFAULT_ENV}"
    
    # Check if environment file exists
    if ! check_env_file "$ENV_NAME"; then
        exit 1
    fi
    
    print_info "Using environment: $ENV_NAME"
    print_info "Loading environment from: .env.${ENV_NAME}.local"
    
    # Load environment variables
    source .env.${ENV_NAME}.local
    
    # Check prerequisites
    check_command "cargo"
    check_command "just"

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
