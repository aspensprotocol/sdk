#!/bin/bash

# Automated Market Maker Internal Tester (AMMIT)
#
# This script automates testing of the Aspens CLI by connecting to a trading server
# and executing a full test sequence including:
# - Server initialization and connection
# - Token deposits on multiple chains
# - Placing buy and sell orders
# - Balance verification
# - Token withdrawals
#
# The script uses the new CLI structure with chain+token parameters:
#   deposit CHAIN TOKEN AMOUNT
#   withdraw CHAIN TOKEN AMOUNT
#   buy AMOUNT --limit-price PRICE [--market MARKET_ID]
#   sell AMOUNT --limit-price PRICE [--market MARKET_ID]
#
# Multi-Token Support:
# To test with multiple tokens (USDC, WETH, WBTC, etc.), ensure your .env file contains:
#   BASE_CHAIN_USDC_TOKEN_ADDRESS=0x...
#   BASE_CHAIN_WETH_TOKEN_ADDRESS=0x...
#   QUOTE_CHAIN_USDC_TOKEN_ADDRESS=0x...
#   QUOTE_CHAIN_WBTC_TOKEN_ADDRESS=0x...
# Then update the deposit/withdraw commands in this script to use different tokens.

# Exit on error
set -e
set -o pipefail

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default environment
DEFAULT_ENV="anvil"
DEFAULT_SERVER_URL="http://localhost:50051"

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

print_command() {
    echo -e "${BLUE}→ $1${NC}"
}

# Function to show usage
show_usage() {
    cat << EOF
Aspens AMMIT - Automated Market Maker Internal Tester

Usage: $0 [environment] [server_url]

Arguments:
  environment    Environment name (default: anvil)
                 Options: anvil, testnet

  server_url     gRPC server URL (default: http://localhost:50051)

Examples:
  $0                                    # Use anvil with localhost
  $0 anvil                              # Use anvil with localhost
  $0 testnet http://remote.server:8080  # Use testnet with remote server

The script will:
  1. Initialize connection to trading server
  2. Check configuration and balances
  3. Deposit USDC on base and quote chains
  4. Place buy and sell orders
  5. Check balances after trading
  6. Withdraw tokens from both chains
  7. Verify final balances

Requirements:
  - Cargo (Rust toolchain)
  - Environment file: .env.<environment>.local
  - Running Aspens trading server at specified URL

For more information, see the README.md
EOF
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
    print_command "aspens-cli --env $ENV_NAME $args"
    cargo run -p aspens-cli -- --env $ENV_NAME $args
}

# Function to run admin CLI command (requires admin feature)
run_admin_command() {
    local args="$@"
    print_command "aspens-cli --env $ENV_NAME $args (admin)"
    cargo run -p aspens-cli --features admin -- --env $ENV_NAME $args
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
    SERVER_URL="${2:-$DEFAULT_SERVER_URL}"

    # Check if environment file exists
    if ! check_env_file "$ENV_NAME"; then
        exit 1
    fi

    print_info "🚀 Aspens AMMIT Test Suite"
    print_info "Environment: $ENV_NAME"
    print_info "Server URL: $SERVER_URL"
    print_info "Config file: .env.${ENV_NAME}.local"

    # Load environment variables (for reference, CLI loads them automatically)
    source .env.${ENV_NAME}.local

    # Check prerequisites
    check_command "cargo"

    print_section "Initializing connection to trading server"
    run_cli_command "initialize $SERVER_URL"

    print_section "Checking configuration status"
    run_cli_command "status"

    # Optional: Fetch server config if admin feature is available
    # Uncomment the following line if you built with --features admin
    # print_section "Fetching server configuration (admin)"
    # run_admin_command "get-config"

    print_section "Checking initial balances"
    run_cli_command "balance"

    print_section "Depositing tokens on base chain"
    print_info "Depositing USDC to base chain..."
    run_cli_command "deposit base usdc 1000000"

    print_section "Depositing tokens on quote chain"
    print_info "Depositing USDC to quote chain..."
    run_cli_command "deposit quote usdc 1000000"

    print_section "Checking balances after deposits"
    run_cli_command "balance"

    print_section "Placing buy orders"
    print_info "Buy order #1: 100 @ limit price 99"
    run_cli_command "buy 100 --limit-price 99"

    print_info "Buy order #2: 150 @ limit price 98"
    run_cli_command "buy 150 --limit-price 98"

    print_info "Buy order #3: 200 @ limit price 97"
    run_cli_command "buy 200 --limit-price 97"

    print_section "Placing sell orders"
    print_info "Sell order #1: 100 @ limit price 101"
    run_cli_command "sell 100 --limit-price 101"

    print_info "Sell order #2: 150 @ limit price 102"
    run_cli_command "sell 150 --limit-price 102"

    print_info "Sell order #3: 200 @ limit price 103"
    run_cli_command "sell 200 --limit-price 103"

    print_section "Checking balances after trading"
    run_cli_command "balance"

    print_section "Withdrawing tokens from base chain"
    print_info "Withdrawing 50% of deposited USDC from base chain..."
    run_cli_command "withdraw base usdc 500000"

    print_section "Withdrawing tokens from quote chain"
    print_info "Withdrawing 50% of deposited USDC from quote chain..."
    run_cli_command "withdraw quote usdc 500000"

    print_section "Final balance check"
    run_cli_command "balance"

    print_section "Final status verification"
    run_cli_command "status"

    print_section "Test Summary"
    echo -e "${GREEN}✅ Test sequence completed successfully!${NC}"
    echo ""
    echo -e "${GREEN}Tests performed:${NC}"
    echo "  ✓ Server connection initialized"
    echo "  ✓ Configuration verified"
    echo "  ✓ Initial balance checked"
    echo "  ✓ Deposited USDC on base chain (1,000,000)"
    echo "  ✓ Deposited USDC on quote chain (1,000,000)"
    echo "  ✓ Placed 3 buy orders at various prices"
    echo "  ✓ Placed 3 sell orders at various prices"
    echo "  ✓ Verified balances after trading"
    echo "  ✓ Withdrew 50% from base chain (500,000)"
    echo "  ✓ Withdrew 50% from quote chain (500,000)"
    echo "  ✓ Final balance verification"
    echo ""
    echo -e "${GREEN}All operations completed without errors${NC}"
    echo -e "${BLUE}You can now review the transaction hashes and balances above${NC}"
}

# Run main function
main "$@" 
