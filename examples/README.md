# Aspens SDK Examples

## Quickstart

The core trading workflow in five steps: **connect, check balances, deposit, trade, withdraw**.

### Using the CLI

```bash
# 1. Connect & check status
aspens-cli status

# 2. View balances
aspens-cli balance

# 3. Deposit tokens (network, token symbol, amount in smallest unit)
aspens-cli deposit anvil-1 USDC 1000000

# 4. Trade
aspens-cli buy-limit <MARKET_ID> 1.5 100.50    # limit buy: quantity, price
aspens-cli sell-market <MARKET_ID> 0.5          # market sell: quantity

# 5. Withdraw tokens back to your wallet
aspens-cli withdraw anvil-1 USDC 500000
```

### Using the Rust SDK

See [`quickstart.rs`](quickstart.rs) for a complete example showing how to use
the `aspens` library crate directly.

### Using the REPL

```bash
aspens-repl
aspens> config
aspens> balance
aspens> deposit anvil-1 USDC 1000000
aspens> buy-limit <MARKET_ID> 1.5 100.50
aspens> withdraw anvil-1 USDC 500000
aspens> quit
```

## Setup

1. Copy `.env.sample` to `.env` and fill in:
   ```
   ASPENS_MARKET_STACK_URL=http://localhost:50051
   TRADER_PRIVKEY=<64-char-hex-private-key>
   ```

2. Verify connectivity:
   ```bash
   aspens-cli status
   aspens-cli config
   ```

## Other Examples

- [`quickstart.rs`](quickstart.rs) — Full SDK workflow (connect, deposit, trade, withdraw)
- [`transaction_hash_example.rs`](transaction_hash_example.rs) — Working with transaction hashes from order responses

## Key Concepts

- **Deposit/withdraw amounts** are in the token's smallest unit (e.g., `1000000` = 1 USDC with 6 decimals)
- **Order quantities and prices** are human-readable strings (e.g., `"1.5"`, `"100.50"`) — the SDK converts to pair decimals automatically
- **Market IDs** follow the format: `chain_network::token_address::chain_network::token_address`
- Run `aspens-cli config` to see available chains, tokens, and markets
