# Decimal Conversions in Aspens: A Comprehensive Guide

## Overview

Aspens is a cross-chain trading platform that handles tokens with different decimal places across multiple blockchains. This document explains how decimal conversions work in the Aspens ecosystem, how to properly format orders using the CLI and REPL, and provides real-world examples.

## Key Concepts

### 1. Token Decimals
Each token has a specific number of decimal places:
- **ETH**: 18 decimals (1 ETH = 1,000,000,000,000,000,000 wei)
- **USDC**: 6 decimals (1 USDC = 1,000,000 base units)
- **BTC**: 8 decimals (1 BTC = 100,000,000 satoshis)
- **USDT**: 6 decimals (1 USDT = 1,000,000 base units)
- **DAI**: 18 decimals (1 DAI = 1,000,000,000,000,000,000 base units)

### 2. Pair Decimals
The `pair_decimals` is a configuration parameter that determines the precision used internally by the orderbook. It can be:
- Equal to the base token decimals
- Equal to the quote token decimals  
- A value between the two
- A value larger than both

### 3. Order Format
Orders are sent to the gRPC API with quantities and prices in **pair decimal format**, not in the original token decimal format.

## How Aspens CLI and REPL Work

### CLI Structure
The Aspens CLI (`aspens-cli`) provides a command-line interface for trading operations:

```bash
# Basic structure
aspens-cli [OPTIONS] <COMMAND>

# Available commands
aspens-cli --help
```

### REPL Structure
The Aspens REPL (`aspens-repl`) provides an interactive trading environment:

```bash
# Start the REPL
aspens-repl

# Available commands in REPL
aspens> help
```

### Order Commands
Both CLI and REPL support the same order commands:

```bash
# Buy order
buy <amount> [--limit-price <price>]

# Sell order  
sell <amount> [--limit-price <price>]
```

## Decimal Conversion Process

### Step 1: Human Input → Pair Decimals
When you place an order via CLI or REPL, you need to convert your human-readable amounts to pair decimal format:

```rust
// For quantity (base token)
pair_quantity = human_quantity * 10^pair_decimals

// For price (quote token per base token)
pair_price = human_price * 10^pair_decimals
```

**Important:** Both quantity and price use the same conversion formula. The matching engine operates entirely in pair decimal format and multiplies quantity × price to calculate the total quote value.

### Step 2: Pair Decimals → On-chain Decimals
When the order is processed, the system converts from pair decimals to the actual token decimals for on-chain operations:

```rust
// For BID orders (buying base token with quote token)
onchain_quantity = normalize_decimals(pair_quantity, pair_decimals, quote_token_decimals)

// For ASK orders (selling base token for quote token)  
onchain_quantity = normalize_decimals(pair_quantity, pair_decimals, base_token_decimals)
```

## Real-World Examples

### Example 1: ETH/USDC Trading via CLI
**Configuration:**
- Base token (ETH): 18 decimals
- Quote token (USDC): 6 decimals  
- Pair decimals: 18

**Scenario:** Buy 1.5 ETH at $2,500 USDC per ETH

**CLI Command:**
```bash
# Calculate pair decimal values
# Quantity: 1.5 * 10^18 = 1,500,000,000,000,000,000
# Price: 2500.0 * 10^18 = 2,500,000,000,000,000,000,000,000,000

aspens-cli buy-limit <market_id> 1500000000000000000 2500000000000000000000000000000000000
```

**REPL Command:**
```bash
aspens> buy-limit <market_id> 1500000000000000000 2500000000000000000000000000000000000
```

**What happens:**
1. Order is placed in pair decimal format (18 decimals)
2. For BID order, quantity is converted from 18 to 6 decimals for USDC on-chain operations
3. On-chain: `normalize_decimals(1500000000000000000, 18, 6) = 1500000` (1.5 USDC worth)

### Example 2: BTC/USDT Trading via REPL
**Configuration:**
- Base token (BTC): 8 decimals
- Quote token (USDT): 6 decimals
- Pair decimals: 8

**Scenario:** Sell 0.5 BTC at $45,000 USDT per BTC

**REPL Command:**
```bash
# Calculate pair decimal values
# Quantity: 0.5 * 10^8 = 50,000,000
# Price: 45000.0 * 10^8 = 4,500,000,000,000

aspens> sell-limit <market_id> 50000000 4500000000000
```

**What happens:**
1. Order is placed in pair decimal format (8 decimals)
2. For ASK order, quantity is converted from 8 to 8 decimals for BTC on-chain operations (no change)
3. On-chain: `normalize_decimals(50000000, 8, 8) = 50000000` (0.5 BTC)

### Example 3: USDC/DAI Trading via CLI
**Configuration:**
- Base token (USDC): 6 decimals
- Quote token (DAI): 18 decimals
- Pair decimals: 12

**Scenario:** Buy 1,000 USDC at 1.001 DAI per USDC

**CLI Command:**
```bash
# Calculate pair decimal values
# Quantity: 1000.0 * 10^12 = 1,000,000,000,000,000
# Price: 1.001 * 10^12 = 1,001,000,000,000

aspens-cli buy-limit <market_id> 1000000000000000 1001000000000
```

**What happens:**
1. Order is placed in pair decimal format (12 decimals)
2. For BID order, quantity is converted from 12 to 18 decimals for DAI on-chain operations
3. On-chain: `normalize_decimals(1000000000000000, 12, 18) = 1000000000000000000000` (1,000 DAI worth)

### Example 4: Market Orders (No Price)
**Configuration:**
- Base token (WBTC): 8 decimals
- Quote token (USDT): 6 decimals
- Pair decimals: 10

**Scenario:** Buy 0.75 WBTC at market price

**CLI Command:**
```bash
# Calculate pair decimal values
# Quantity: 0.75 * 10^10 = 7,500,000,000
# No price specified (market order)

aspens-cli buy-market <market_id> 7500000000
```

**REPL Command:**
```bash
aspens> buy-market <market_id> 7500000000
```

## Setting Up Trading Environment

### 1. Environment Configuration
Create a `.env.anvil.local` file with your configuration:

```bash
# Aspens Market Stack URL
ASPENS_MARKET_STACK_URL=http://localhost:50051

# Wallet configuration (only private key needed - public key is derived automatically)
EVM_TESTNET_PRIVKEY=0x1234567890123456789012345678901234567890123456789012345678901234
```

**Note:** All chain, token, contract, and market configuration is fetched automatically from the Aspens Market Stack.

### 2. Using the CLI
```bash
# Initialize session
aspens-cli initialize

# Get configuration
aspens-cli --admin get-config

# Deposit funds
aspens-cli deposit base-goerli USDC 1000000

# Place orders (use actual market_id from config)
aspens-cli buy-limit <market_id> 1500000000000000000 2500000000000000000000000000000000000
aspens-cli sell-limit <market_id> 50000000 4500000000000

# Check balance
aspens-cli balance
```

### 3. Using the REPL
```bash
# Start REPL
aspens-repl

# Initialize session
aspens> initialize --url https://ams-instance-url:50051

# Get configuration
aspens> config

# Deposit funds
aspens> deposit base-goerli USDC 1000000

# Place orders (use actual market_id from config)
aspens> buy-limit <market_id> 1500000000000000000 2500000000000000000000000000000000000
aspens> sell-limit <market_id> 50000000 4500000000000

# Check balance
aspens> balance

# Quit
aspens> quit
```

## Decimal Conversion Helper Functions

The Aspens system uses several helper functions for decimal conversion:

### `normalize_decimals(amount, from_decimals, to_decimals)`
Converts amounts between different decimal precisions:

```rust
// ETH (18 decimals) to USDC (6 decimals)
let eth_amount = U256::from(1000000000000000000u128); // 1 ETH
let usdc_amount = normalize_decimals(eth_amount, 18, 6);
// Result: 1000000 (1 USDC worth)
```

### `base_units_to_human(amount, decimals)`
Converts base units to human-readable values:

```rust
// Convert 1 ETH base units to human value
let eth_base_units = U256::from(1000000000000000000u128);
let human_eth = base_units_to_human(eth_base_units, 18);
// Result: 1
```

### `human_to_base_units(value, decimals)`
Converts human-readable values to base units:

```rust
// Convert 1 ETH human value to base units
let eth_base_units = human_to_base_units(1, 18)?;
// Result: 1000000000000000000
```

## Common Pitfalls and Best Practices

### 1. Precision Loss
When converting from higher decimals to lower decimals, precision can be lost:
- WBTC (8 decimals) → USDT (6 decimals): May lose precision
- ETH (18 decimals) → USDC (6 decimals): May lose precision

### 2. Price Truncation
Very small prices may become 0 due to u64 limitations:
- DAI price of 1.001 with pair_decimals=12 becomes 0
- SHIB price of 0.00001 with pair_decimals=6 becomes 0

### 3. Overflow
Very large amounts may overflow u64:
- Always check your calculations before sending orders

### 4. Best Practices
1. **Always convert to pair decimals** before sending orders
2. **Test with small amounts** first to verify decimal conversions
3. **Use the normalize_decimals function** for verification
4. **Check for precision loss** when scaling down
5. **Verify on-chain amounts** match your expectations

## Testing Your Orders

### Using gRPC Commands
```bash
# Get market configuration
just get-config

# Stream orderbook to see decimal formatting
just stream-orderbook "your-market-id"

# Add test markets with specific decimal configurations
just add-market base-chain quote-chain base-token quote-token base-addr quote-addr base-decimals quote-decimals pair-decimals
```

## Summary

The key points for using Aspens CLI and REPL with decimal conversions:

1. **Orders are sent in pair decimal format** - convert your human amounts using `human_amount * 10^pair_decimals`
2. **The system converts to token decimals for on-chain operations** automatically
3. **Precision can be lost when scaling down** - be aware of this when trading tokens with very different decimal places
4. **Always test your decimal conversions** before production use
5. **Use the CLI for scripted trading** and **REPL for interactive trading**

This system allows Aspens to handle diverse token combinations while maintaining precision where possible and gracefully handling precision loss where necessary. 
