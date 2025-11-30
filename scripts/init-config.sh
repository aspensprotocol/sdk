#!/bin/bash

# Initialize configuration for Aspens
# This script sets up:
# - Two chains: base-sepolia and flare-coston2
# - Tokens: USDC on base-sepolia, USDT0 and WC2FLR on flare-coston2
# - Markets: USDC/USDT0 and WC2FLR/USDC
#
# Usage: ./scripts/init-config.sh
#
# Prerequisites:
# - aspens-admin binary built (cargo build --package aspens-admin)
# - ADMIN_PRIVKEY set in your .env file for the manager wallet
# - ASPENS_MARKET_STACK_URL set in your .env file

set -e

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
print_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }
print_step() { echo -e "${BLUE}[STEP]${NC} $1"; }

# Default values
ASPENS_ADMIN="cargo run --package aspens-admin --"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --help|-h)
            echo "Usage: $0"
            echo ""
            echo "This script initializes the Aspens stack with:"
            echo "  - Chains: base-sepolia, flare-coston2"
            echo "  - Tokens: USDC (base-sepolia), USDT0/WC2FLR (flare-coston2)"
            echo "  - Markets: USDC/USDT0, WC2FLR/USDC"
            echo ""
            echo "Configuration is read from .env file (ASPENS_MARKET_STACK_URL, ADMIN_PRIVKEY)"
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# ============================================================================
# Configuration - Update these values for your deployment
# ============================================================================

# Base Sepolia configuration
BASE_SEPOLIA_CHAIN_ID=84532
BASE_SEPOLIA_RPC_URL="https://localhost:8545"
BASE_SEPOLIA_EXPLORER="https://sepolia.basescan.org"
BASE_SEPOLIA_SERVICE_ADDRESS="0x0000000000000000000000000000000000000000"  # Update with actual factory
BASE_SEPOLIA_PERMIT2_ADDRESS="0x000000000022D473030F116dDEE9F6B43aC78BA3"

# Flare Coston2 configuration
FLARE_COSTON2_CHAIN_ID=114
# FLARE_COSTON2_RPC_URL="https://coston2-api.flare.network/ext/C/rpc"
FLARE_COSTON2_RPC_URL="http://localhost:8546"
FLARE_COSTON2_EXPLORER="https://coston2-explorer.flare.network"
FLARE_COSTON2_SERVICE_ADDRESS="0x0000000000000000000000000000000000000000"  # Update with actual factory
FLARE_COSTON2_PERMIT2_ADDRESS="0x000000000022D473030F116dDEE9F6B43aC78BA3"

# Token addresses
USDC_ADDRESS_BASE="0x036CbD53842c5426634e7929541eC2318f3dCF7e"    # USDC on Base Sepolia
USDT0_ADDRESS_FLARE="0xC1A5B41512496B80903D1f32d6dEa3a73212E71F"  # USDT0 on Flare Coston2
WC2FLR_ADDRESS_FLARE="0xC67DCE33D7A8efA5FfEB961899C73fe01bCe9273"  # WC2FLR on Flare Coston2

# ============================================================================
# Script execution
# ============================================================================

print_info "Initializing Aspens Market Stack configuration"
echo ""

# Check if JWT is already set
if [[ -z "$ASPENS_JWT" ]]; then
    print_step "Step 1: Initializing manager and obtaining JWT..."
    print_warn "No ASPENS_JWT found. Running init-manager..."
    print_warn "If manager already exists, use 'aspens-admin login' instead"
    echo ""

    # Try to login first (in case manager already exists)
    if $ASPENS_ADMIN login 2>/dev/null; then
        print_info "Logged in successfully"
    else
        print_warn "Login failed, attempting to initialize new manager..."
        # Get address from private key - this requires the user to provide it
        print_error "Please run 'aspens-admin init-manager --address <your-address>' manually"
        print_error "Then export ASPENS_JWT and re-run this script"
        exit 1
    fi
else
    print_info "Using existing JWT from ASPENS_JWT environment variable"
fi

echo ""
print_step "Step 2: Adding chains..."

# Add Base Sepolia
print_info "Adding Base Sepolia chain..."
$ASPENS_ADMIN add-chain \
    --architecture "EVM" \
    --canonical-name "Base Sepolia" \
    --network "base-sepolia" \
    --chain-id "$BASE_SEPOLIA_CHAIN_ID" \
    --contract-owner-address "0x0000000000000000000000000000000000000000" \
    --rpc-url "$BASE_SEPOLIA_RPC_URL" \
    --service-address "$BASE_SEPOLIA_SERVICE_ADDRESS" \
    --permit2-address "$BASE_SEPOLIA_PERMIT2_ADDRESS" \
    --explorer-url "$BASE_SEPOLIA_EXPLORER" \
    || print_warn "Chain may already exist"

# Add Flare Coston2
print_info "Adding Flare Coston2 chain..."
$ASPENS_ADMIN add-chain \
    --architecture "EVM" \
    --canonical-name "Flare Coston2" \
    --network "flare-coston2" \
    --chain-id "$FLARE_COSTON2_CHAIN_ID" \
    --contract-owner-address "0x0000000000000000000000000000000000000000" \
    --rpc-url "$FLARE_COSTON2_RPC_URL" \
    --service-address "$FLARE_COSTON2_SERVICE_ADDRESS" \
    --permit2-address "$FLARE_COSTON2_PERMIT2_ADDRESS" \
    --explorer-url "$FLARE_COSTON2_EXPLORER" \
    || print_warn "Chain may already exist"

echo ""
print_step "Step 3: Adding tokens..."

# Add USDC to Base Sepolia
print_info "Adding USDC to Base Sepolia..."
$ASPENS_ADMIN add-token \
    --network "base-sepolia" \
    --name "USD Coin" \
    --symbol "USDC" \
    --address "$USDC_ADDRESS_BASE" \
    --decimals 6 \
    --trade-precision 6 \
    || print_warn "Token may already exist"

# Add USDT0 to Flare Coston2
print_info "Adding USDT0 to Flare Coston2..."
$ASPENS_ADMIN add-token \
    --network "flare-coston2" \
    --name "Tether USD" \
    --symbol "USDT0" \
    --address "$USDT0_ADDRESS_FLARE" \
    --decimals 6 \
    --trade-precision 6 \
    || print_warn "Token may already exist"

# Add WC2FLR to Flare Coston2
print_info "Adding WC2FLR to Flare Coston2..."
$ASPENS_ADMIN add-token \
    --network "flare-coston2" \
    --name "Wrapped Coston2 Flare" \
    --symbol "WC2FLR" \
    --address "$WC2FLR_ADDRESS_FLARE" \
    --decimals 18 \
    --trade-precision 6 \
    || print_warn "Token may already exist"

echo ""
print_step "Step 4: Adding markets..."

# Add USDC/USDT0 market (Base Sepolia USDC <-> Flare Coston2 USDT0)
print_info "Adding USDC/USDT0 market..."
$ASPENS_ADMIN add-market \
    --base-network "base-sepolia" \
    --quote-network "flare-coston2" \
    --base-symbol "USDC" \
    --quote-symbol "USDT0" \
    --base-address "$USDC_ADDRESS_BASE" \
    --quote-address "$USDT0_ADDRESS_FLARE" \
    --base-decimals 6 \
    --quote-decimals 6 \
    --pair-decimals 6 \
    || print_warn "Market may already exist"

# Add WC2FLR/USDC market (Flare Coston2 WC2FLR <-> Base Sepolia USDC)
print_info "Adding WC2FLR/USDC market..."
$ASPENS_ADMIN add-market \
    --base-network "flare-coston2" \
    --quote-network "base-sepolia" \
    --base-symbol "WC2FLR" \
    --quote-symbol "USDC" \
    --base-address "$WC2FLR_ADDRESS_FLARE" \
    --quote-address "$USDC_ADDRESS_BASE" \
    --base-decimals 18 \
    --quote-decimals 6 \
    --pair-decimals 6 \
    || print_warn "Market may already exist"

echo ""
print_info "============================================"
print_info "Testnet configuration complete!"
print_info "============================================"
echo ""
print_info "Chains added:"
print_info "  - base-sepolia (Chain ID: $BASE_SEPOLIA_CHAIN_ID)"
print_info "  - flare-coston2 (Chain ID: $FLARE_COSTON2_CHAIN_ID)"
echo ""
print_info "Tokens added:"
print_info "  - USDC on base-sepolia"
print_info "  - USDT0 on flare-coston2"
print_info "  - WC2FLR on flare-coston2"
echo ""
print_info "Markets added:"
print_info "  - USDC/USDT0 (base-sepolia <-> flare-coston2)"
print_info "  - WC2FLR/USDC (flare-coston2 <-> base-sepolia)"
echo ""
print_warn "Note: Update service addresses with actual factory contract addresses"
