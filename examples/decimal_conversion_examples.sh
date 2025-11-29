#!/bin/bash

# Aspens Decimal Conversion Examples
# This script demonstrates how to use the Aspens CLI with proper decimal conversions
# for different token pairs.

set -e

echo "=== Aspens Decimal Conversion Examples ==="
echo "This script demonstrates proper decimal conversion for different trading pairs."
echo ""

# Configuration
ASPENS_MARKET_STACK_URL="http://localhost:50051"
MARKET_ID_ETH_USDC="1::0x1234567890123456789012345678901234567890::1::0x0987654321098765432109876543210987654321"
MARKET_ID_BTC_USDT="2::0x1234567890123456789012345678901234567890::2::0x0987654321098765432109876543210987654321"
MARKET_ID_USDC_DAI="3::0x1234567890123456789012345678901234567890::3::0x0987654321098765432109876543210987654321"

# Function to calculate pair decimals
calculate_pair_decimals() {
    local human_amount=$1
    local pair_decimals=$2
    echo "scale=0; $human_amount * 10^$pair_decimals / 1" | bc
}

# Function to display conversion info
show_conversion_info() {
    local human_amount=$1
    local pair_decimals=$2
    local pair_amount=$3
    local token_name=$4
    
    echo "  Human amount: $human_amount $token_name"
    echo "  Pair decimals: $pair_decimals"
    echo "  Pair amount: $pair_amount"
    echo "  Formula: $human_amount * 10^$pair_decimals = $pair_amount"
    echo ""
}

echo "=== Example 1: ETH/USDC Trading ==="
echo "Configuration:"
echo "  Base token (ETH): 18 decimals"
echo "  Quote token (USDC): 6 decimals"
echo "  Pair decimals: 18"
echo ""

# Calculate ETH/USDC examples
ETH_HUMAN_AMOUNT=1.5
ETH_PAIR_DECIMALS=18
ETH_PAIR_AMOUNT=$(calculate_pair_decimals $ETH_HUMAN_AMOUNT $ETH_PAIR_DECIMALS)

USDC_HUMAN_PRICE=2500.0
USDC_PAIR_PRICE=$(calculate_pair_decimals $USDC_HUMAN_PRICE $ETH_PAIR_DECIMALS)

show_conversion_info $ETH_HUMAN_AMOUNT $ETH_PAIR_DECIMALS $ETH_PAIR_AMOUNT "ETH"
show_conversion_info $USDC_HUMAN_PRICE $ETH_PAIR_DECIMALS $USDC_PAIR_PRICE "USDC price"

echo "CLI Commands:"
echo "  # Buy 1.5 ETH at $2,500 USDC per ETH"
echo "  aspens-cli buy $ETH_PAIR_AMOUNT --limit-price $USDC_PAIR_PRICE"
echo ""
echo "  # Sell 0.5 ETH at $2,400 USDC per ETH"
ETH_SELL_AMOUNT=$(calculate_pair_decimals 0.5 $ETH_PAIR_DECIMALS)
USDC_SELL_PRICE=$(calculate_pair_decimals 2400.0 $ETH_PAIR_DECIMALS)
echo "  aspens-cli sell $ETH_SELL_AMOUNT --limit-price $USDC_SELL_PRICE"
echo ""

echo "=== Example 2: BTC/USDT Trading ==="
echo "Configuration:"
echo "  Base token (BTC): 8 decimals"
echo "  Quote token (USDT): 6 decimals"
echo "  Pair decimals: 8"
echo ""

# Calculate BTC/USDT examples
BTC_HUMAN_AMOUNT=0.5
BTC_PAIR_DECIMALS=8
BTC_PAIR_AMOUNT=$(calculate_pair_decimals $BTC_HUMAN_AMOUNT $BTC_PAIR_DECIMALS)

USDT_HUMAN_PRICE=45000.0
USDT_PAIR_PRICE=$(calculate_pair_decimals $USDT_HUMAN_PRICE $BTC_PAIR_DECIMALS)

show_conversion_info $BTC_HUMAN_AMOUNT $BTC_PAIR_DECIMALS $BTC_PAIR_AMOUNT "BTC"
show_conversion_info $USDT_HUMAN_PRICE $BTC_PAIR_DECIMALS $USDT_PAIR_PRICE "USDT price"

echo "CLI Commands:"
echo "  # Sell 0.5 BTC at $45,000 USDT per BTC"
echo "  aspens-cli sell $BTC_PAIR_AMOUNT --limit-price $USDT_PAIR_PRICE"
echo ""
echo "  # Buy 0.1 BTC at $44,500 USDT per BTC"
BTC_BUY_AMOUNT=$(calculate_pair_decimals 0.1 $BTC_PAIR_DECIMALS)
USDT_BUY_PRICE=$(calculate_pair_decimals 44500.0 $BTC_PAIR_DECIMALS)
echo "  aspens-cli buy $BTC_BUY_AMOUNT --limit-price $USDT_BUY_PRICE"
echo ""

echo "=== Example 3: USDC/DAI Trading ==="
echo "Configuration:"
echo "  Base token (USDC): 6 decimals"
echo "  Quote token (DAI): 18 decimals"
echo "  Pair decimals: 12"
echo ""

# Calculate USDC/DAI examples
USDC_HUMAN_AMOUNT=1000.0
USDC_PAIR_DECIMALS=12
USDC_PAIR_AMOUNT=$(calculate_pair_decimals $USDC_HUMAN_AMOUNT $USDC_PAIR_DECIMALS)

DAI_HUMAN_PRICE=1.001
DAI_PAIR_PRICE=$(calculate_pair_decimals $DAI_HUMAN_PRICE $USDC_PAIR_DECIMALS)

show_conversion_info $USDC_HUMAN_AMOUNT $USDC_PAIR_DECIMALS $USDC_PAIR_AMOUNT "USDC"
show_conversion_info $DAI_HUMAN_PRICE $USDC_PAIR_DECIMALS $DAI_PAIR_PRICE "DAI price"

echo "CLI Commands:"
echo "  # Buy 1,000 USDC at 1.001 DAI per USDC"
echo "  aspens-cli buy $USDC_PAIR_AMOUNT --limit-price $DAI_PAIR_PRICE"
echo ""
echo "  # Sell 500 USDC at 0.999 DAI per USDC"
USDC_SELL_AMOUNT=$(calculate_pair_decimals 500.0 $USDC_PAIR_DECIMALS)
DAI_SELL_PRICE=$(calculate_pair_decimals 0.999 $USDC_PAIR_DECIMALS)
echo "  aspens-cli sell $USDC_SELL_AMOUNT --limit-price $DAI_SELL_PRICE"
echo ""

echo "=== Example 4: Market Orders ==="
echo "Configuration:"
echo "  Base token (WBTC): 8 decimals"
echo "  Quote token (USDT): 6 decimals"
echo "  Pair decimals: 10"
echo ""

# Calculate WBTC market order example
WBTC_HUMAN_AMOUNT=0.75
WBTC_PAIR_DECIMALS=10
WBTC_PAIR_AMOUNT=$(calculate_pair_decimals $WBTC_HUMAN_AMOUNT $WBTC_PAIR_DECIMALS)

show_conversion_info $WBTC_HUMAN_AMOUNT $WBTC_PAIR_DECIMALS $WBTC_PAIR_AMOUNT "WBTC"

echo "CLI Commands:"
echo "  # Buy 0.75 WBTC at market price"
echo "  aspens-cli buy $WBTC_PAIR_AMOUNT"
echo ""
echo "  # Sell 0.25 WBTC at market price"
WBTC_SELL_AMOUNT=$(calculate_pair_decimals 0.25 $WBTC_PAIR_DECIMALS)
echo "  aspens-cli sell $WBTC_SELL_AMOUNT"
echo ""

echo "=== Example 5: Small Amount Trading ==="
echo "Configuration:"
echo "  Base token (SHIB): 18 decimals"
echo "  Quote token (USDT): 6 decimals"
echo "  Pair decimals: 6"
echo ""

# Calculate SHIB/USDT examples
SHIB_HUMAN_AMOUNT=1000000.0
SHIB_PAIR_DECIMALS=6
SHIB_PAIR_AMOUNT=$(calculate_pair_decimals $SHIB_HUMAN_AMOUNT $SHIB_PAIR_DECIMALS)

SHIB_HUMAN_PRICE=0.00001
SHIB_PAIR_PRICE=$(calculate_pair_decimals $SHIB_HUMAN_PRICE $SHIB_PAIR_DECIMALS)

show_conversion_info $SHIB_HUMAN_AMOUNT $SHIB_PAIR_DECIMALS $SHIB_PAIR_AMOUNT "SHIB"
show_conversion_info $SHIB_HUMAN_PRICE $SHIB_PAIR_DECIMALS $SHIB_PAIR_PRICE "USDT price"

echo "CLI Commands:"
echo "  # Buy 1,000,000 SHIB at $0.00001 USDT per SHIB"
echo "  aspens-cli buy $SHIB_PAIR_AMOUNT --limit-price $SHIB_PAIR_PRICE"
echo ""

echo "=== Complete Trading Session Example ==="
echo ""
echo "# 1. Initialize session"
echo "aspens-cli initialize"
echo ""
echo "# 2. Get configuration"
echo "aspens-cli --admin get-config"
echo ""
echo "# 3. Deposit funds (amounts in base units)"
echo "aspens-cli deposit base-goerli USDC 1000000  # 1 USDC"
echo "aspens-cli deposit base-sepolia ETH 1000000000000000000  # 1 ETH"
echo ""
echo "# 4. Place orders using pair decimal amounts"
echo "aspens-cli buy $ETH_PAIR_AMOUNT --limit-price $USDC_PAIR_PRICE"
echo "aspens-cli sell $BTC_PAIR_AMOUNT --limit-price $USDT_PAIR_PRICE"
echo ""
echo "# 5. Check balance"
echo "aspens-cli balance"
echo ""
echo "# 6. Get orderbook"
echo "aspens-cli get-orderbook $MARKET_ID_ETH_USDC"
echo ""

echo "=== REPL Session Example ==="
echo ""
echo "# Start REPL"
echo "aspens-repl"
echo ""
echo "# In REPL:"
echo "aspens> initialize --url $ASPENS_MARKET_STACK_URL"
echo "aspens> get-config"
echo "aspens> deposit base-goerli USDC 1000000"
echo "aspens> buy $ETH_PAIR_AMOUNT --limit-price $USDC_PAIR_PRICE"
echo "aspens> sell $BTC_PAIR_AMOUNT"
echo "At what price? $USDT_PAIR_PRICE"
echo "aspens> balance"
echo "aspens> quit"
echo ""

echo "=== Important Notes ==="
echo ""
echo "1. All amounts in CLI commands must be in PAIR DECIMAL format"
echo "2. Use the formula: human_amount * 10^pair_decimals"
echo "3. The system automatically converts to token decimals for on-chain operations"
echo "4. Precision may be lost when scaling down from higher to lower decimals"
echo "5. Always test with small amounts first"
echo "6. Check the orderbook to verify your orders are formatted correctly"
echo ""
echo "=== Testing Your Setup ==="
echo ""
echo "# Test if Arborter is running"
echo "curl -s $ASPENS_MARKET_STACK_URL/health || echo 'Arborter not running'"
echo ""
echo "# Test market configuration"
echo "just get-config"
echo ""
echo "# Stream orderbook to see decimal formatting"
echo "just stream-orderbook $MARKET_ID_ETH_USDC"
echo ""

echo "Script completed successfully!" 
