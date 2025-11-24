# Aspens Decimal Conversion Examples

This directory contains practical examples demonstrating how to use the Aspens CLI and REPL with proper decimal conversions for crosschain trading.

## Files

- `decimal_conversion_examples.sh` - Comprehensive script showing decimal conversions for various token pairs
- `README.md` - This file

## Quick Start

### Prerequisites

1. **Arborter Server Running**
   ```bash
   cd arborter
   just run
   ```

2. **Environment Configuration**
   Create a `.env.anvil.local` file in the aspens directory:
   ```bash
   # Arborter server URL
   ARBORTER_URL=http://localhost:50051
   
   # Market configuration
   MARKET_ID_1=1::0x1234567890123456789012345678901234567890::1::0x0987654321098765432109876543210987654321
   
   # Wallet configuration
   EVM_TESTNET_PUBKEY=0x9bdbb2d6fb90a54f90e8bfee32157a081b0a907f
   EVM_TESTNET_PRIVKEY=0x1234567890123456789012345678901234567890123456789012345678901234
   
   # Chain configuration
   BASE_CHAIN_RPC_URL=http://localhost:8545
   QUOTE_CHAIN_RPC_URL=http://localhost:8546
   BASE_CHAIN_USDC_TOKEN_ADDRESS=0x1234567890123456789012345678901234567890
   QUOTE_CHAIN_USDC_TOKEN_ADDRESS=0x0987654321098765432109876543210987654321
   BASE_CHAIN_CONTRACT_ADDRESS=0x1111111111111111111111111111111111111111
   QUOTE_CHAIN_CONTRACT_ADDRESS=0x2222222222222222222222222222222222222222
   ```

3. **Build Aspens CLI and REPL**
   ```bash
   cd aspens/wrappers
   cargo build --release
   ```

### Running the Examples

1. **View the Examples**
   ```bash
   cd aspens/examples
   chmod +x decimal_conversion_examples.sh
   ./decimal_conversion_examples.sh
   ```

2. **Run Individual Examples**
   ```bash
   # ETH/USDC trading
   aspens-cli buy 1500000000000000000 --limit-price 2500000000000000000000000000000000000
   
   # BTC/USDT trading
   aspens-cli sell 50000000 --limit-price 4500000000000
   
   # USDC/DAI trading
   aspens-cli buy 1000000000000000 --limit-price 1001000000000
   ```

## Key Concepts Explained

### Decimal Conversion Formula

The fundamental formula for converting human-readable amounts to pair decimal format:

```
pair_amount = human_amount * 10^pair_decimals
```

### Examples by Token Pair

#### 1. ETH/USDC (18/6 decimals, pair_decimals=18)
- **Human**: 1.5 ETH at $2,500 USDC
- **Pair**: 1,500,000,000,000,000,000 at 2,500,000,000,000,000,000,000,000,000,000,000
- **CLI**: `aspens-cli buy 1500000000000000000 --limit-price 2500000000000000000000000000000000000`

#### 2. BTC/USDT (8/6 decimals, pair_decimals=8)
- **Human**: 0.5 BTC at $45,000 USDT
- **Pair**: 50,000,000 at 4,500,000,000,000
- **CLI**: `aspens-cli sell 50000000 --limit-price 4500000000000`

#### 3. USDC/DAI (6/18 decimals, pair_decimals=12)
- **Human**: 1,000 USDC at 1.001 DAI
- **Pair**: 1,000,000,000,000,000 at 1,001,000,000,000
- **CLI**: `aspens-cli buy 1000000000000000 --limit-price 1001000000000`

## Common Patterns

### Market Orders (No Price)
```bash
# Buy 0.75 WBTC at market price
aspens-cli buy 7500000000

# Sell 0.25 WBTC at market price
aspens-cli sell 2500000000
```

### Small Amount Trading
```bash
# Buy 1,000,000 SHIB at $0.00001 USDT
aspens-cli buy 1000000000000 --limit-price 10
```

### REPL Interactive Trading
```bash
aspens-repl
aspens> initialize --url http://localhost:50051
aspens> buy 1500000000000000000 --limit-price 2500000000000000000000000000000000000
aspens> sell 50000000
At what price? 4500000000000
aspens> balance
aspens> quit
```

## Testing Your Setup

### 1. Check Arborter Status
```bash
# From arborter directory
just get-config
```

### 2. Stream Orderbook
```bash
# View real-time orderbook with decimal formatting
just stream-orderbook "your-market-id"
```

### 3. Test Small Orders
```bash
# Always test with small amounts first
aspens-cli buy 100000000000000000 --limit-price 100000000000000000000000000000000000
```

## Troubleshooting

### Common Issues

1. **"Invalid amount" errors**
   - Ensure you're using pair decimal format, not human amounts
   - Check that your amounts don't exceed u64 limits

2. **"Precision loss" warnings**
   - This is expected when scaling down from higher to lower decimals
   - The system handles this automatically

3. **"Order not found" errors**
   - Verify your market ID is correct
   - Check that Arborter is running and configured

### Debug Commands

```bash
# Check Arborter logs
cd arborter
just run

# View orderbook in real-time
just stream-orderbook "your-market-id"

# Test gRPC connection
grpcurl -plaintext localhost:50051 xyz.aspens.arborter_config.v1.ConfigService.GetConfig
```

## Advanced Usage

### Custom Decimal Calculations

For custom token pairs, calculate pair decimals manually:

```bash
# Example: Token with 27 decimals vs USDC with 6 decimals
# Pair decimals: 18
HUMAN_AMOUNT=0.000000000000000000000001
PAIR_DECIMALS=18
PAIR_AMOUNT=$(echo "scale=0; $HUMAN_AMOUNT * 10^$PAIR_DECIMALS / 1" | bc)
echo "Pair amount: $PAIR_AMOUNT"
```

### Batch Trading

Create scripts for automated trading:

```bash
#!/bin/bash
# batch_trading.sh

# Calculate amounts
ETH_AMOUNT=$(echo "scale=0; 1.5 * 10^18 / 1" | bc)
ETH_PRICE=$(echo "scale=0; 2500.0 * 10^18 / 1" | bc)

# Place orders
aspens-cli buy $ETH_AMOUNT --limit-price $ETH_PRICE
aspens-cli sell $ETH_AMOUNT --limit-price $ETH_PRICE
```

## References

- [Main Decimal Documentation](../decimals.md) - Comprehensive guide to decimal conversions
- [Arborter Decimal Guide](../../arborter/decimals.md) - Technical details of the conversion system
- [CLI Documentation](../wrappers/src/bin/cli.rs) - CLI command reference
- [REPL Documentation](../wrappers/src/bin/repl.rs) - REPL command reference

## Support

For issues or questions:
1. Check the troubleshooting section above
2. Review the main decimal documentation
3. Test with the provided examples
4. Check Arborter logs for detailed error messages 
