#!/bin/bash

# Environment switcher for Aspens
# Usage: ./scripts/env-switch.sh [environment_name]
# Examples: ./scripts/env-switch.sh anvil
#          ./scripts/env-switch.sh testnet

set -e

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

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
    echo "  anvil    - Local Anvil development environment"
    echo "  testnet  - Testnet environment (Sepolia, Mumbai, etc.)"
    echo ""
    echo "Examples:"
    echo "  $0 anvil"
    echo "  $0 testnet"
    echo ""
    echo "You can also set the ASPENS_ENV environment variable:"
    echo "  export ASPENS_ENV=testnet"
    echo "  cargo run --bin aspens-cli"
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

# Function to list available environments
list_environments() {
    print_info "Available environment files:"
    for file in .env.*.local; do
        if [[ -f "$file" ]]; then
            env_name=$(basename "$file" | sed 's/\.env\.\(.*\)\.local/\1/')
            echo "  - $env_name"
        fi
    done
}

# Main logic
main() {
    # If no arguments provided, show usage
    if [[ $# -eq 0 ]]; then
        show_usage
        exit 1
    fi
    
    # Check for help flag
    if [[ "$1" == "-h" || "$1" == "--help" ]]; then
        show_usage
        exit 0
    fi
    
    # Check for list flag
    if [[ "$1" == "-l" || "$1" == "--list" ]]; then
        list_environments
        exit 0
    fi
    
    local env_name=$1
    
    # Check if environment file exists
    if ! check_env_file "$env_name"; then
        exit 1
    fi
    
    # Set the environment variable
    export ASPENS_ENV="$env_name"
    
    print_info "Switched to environment: $env_name"
    print_info "Environment file: .env.${env_name}.local"
    print_info ""
    print_info "You can now run commands with:"
    print_info "  cargo run --bin aspens-cli -- --env $env_name"
    print_info "  cargo run --bin aspens-repl -- --env $env_name"
    print_info ""
    print_info "Or use the environment variable:"
    print_info "  export ASPENS_ENV=$env_name"
    print_info "  cargo run --bin aspens-cli"
}

# Run main function
main "$@" 