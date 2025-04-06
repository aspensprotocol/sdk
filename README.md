# Apsens CLI
 
a REPL style CLI to interact with an Aspens Markets Stack

## Prerequisites

1. Install Rust:
```bash
# Install Rust using rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. Install Foundry (for local development):
```bash
# Install Foundry
curl -L https://foundry.paradigm.xyz | bash
foundryup
```

3. Set up environment variables:
```bash
# Copy the .env sample 
cp app/.env.sample app/.env

# After changing the values in your .env
source app/.env
```

## Usage

The CLI can be used in two modes: interactive and scripted.

### Interactive Mode

Interactive mode provides a REPL (Read-Eval-Print Loop) interface where you can execute commands one at a time:

```bash
$ just cli

aspens> help
Available commands:
  balance - Check your token balances
  deposit - Deposit tokens for trading
  send-order - Send an order to the market
  withdraw - Withdraw tokens to your wallet

aspens> send-order buy 100 50
Response received: SendOrderReply {
  order_in_book: true,
  order: Order {
    side: 1,
    quantity: 100,
    price: 50,
    ...
  },
  trades: []
}

aspens> balance
Token Balances:
  USDC:
    Base Sepolia: 1000 (Available: 900, Locked: 100)
    Optimism Sepolia: 2000 (Available: 1800, Locked: 200)

aspens> quit
```

### Scripted Mode

Scripted mode allows you to execute commands directly from the command line, which is useful for automation and scripting:

```bash
# Send a buy order
$ just run send-order buy 100 50

# Check balances
$ just run balance

# Deposit tokens
$ just run deposit base-sepolia USDC 1000

# Withdraw tokens
$ just run withdraw optimism-sepolia USDC 500
```

### Available Commands

### Configuration Commands
- `initialize` - Initialize a new trading session by (optionally) defining the arborter URL
- `get-config` - Fetch the current configuration from the arborter server
- `add-market` - Add a new market to the arborter service (requires valid signature)
- `add-token` - Add a new token to the arborter service (requires valid signature)
- `deploy-contract` - Deploy the trade contract onto the given chain

### Trading Commands
- `deposit` - Deposit token(s) to make them available for trading
- `withdraw` - Withdraw token(s) to a local wallet
- `buy` - Send a BUY order
- `sell` - Send a SELL order
- `get-orders` - Get a list of all active orders
- `cancel-order` - Cancel an order
- `balance` - Fetch the balances
- `get-orderbook` - Fetch the latest top of book

### REPL Commands
The REPL interface provides the same commands as above, but with a more interactive experience:
- `initialize` - Initialize a new trading session
- `get-config` - Fetch the current configuration
- `add-market` - Add a new market
- `add-token` - Add a new token
- `deploy-contract` - Deploy the trade contract
- `deposit` - Deposit tokens
- `withdraw` - Withdraw tokens
- `buy` - Send a BUY order
- `sell` - Send a SELL order
- `get-orders` - Get active orders
- `cancel-order` - Cancel an order
- `balance` - Fetch balances
- `get-orderbook` - Fetch orderbook
- `quit` - Close the session and quit

For more details about a specific command, use:
```bash
aspens> help <command>
```

## Local Development with Anvil

For local development and testing, you can use Anvil (part of Foundry) to create a local blockchain environment.

### Setting up the Environment

```bash
# Set up Anvil and deploy test tokens
just setup-anvil-full

# This will:
# - Start two Anvil instances (ports 8545 and 8546)
# - Deploy USDC and WBTC on the first chain
# - Deploy USDT on the second chain
# - Mint initial tokens to test accounts
```

### Creating Custom Tokens

You can create custom ERC20 tokens for testing using the `create-token` command:

```bash
# Create a token with default 18 decimals
just create-token BTC

# Create a token with custom decimals (e.g., 8 decimals like BTC)
just create-token BTC 8
```

The command will:
- Deploy a new ERC20 token to your local Anvil instance
- Set the token name and symbol
- Configure the number of decimal places
- Mint 1,000,000 tokens to the deployer
- Display the token details (name, symbol, address, decimals)

### Testing with Local Tokens

After creating tokens, you can use them with the CLI:

```bash
# Start the CLI
just cli

# Initialize with local Anvil
aspens> initialize http://localhost:50051

# Check balances
aspens> balance

# Place orders using the token addresses from the deployment output
aspens> send-order --market-id <chain_id>::<token1>::<chain_id>::<token2> --side buy --amount 100000000 --price 100
```

### Cleanup

When you're done testing:
```bash
aspens> quit

# Stop Anvil instances
just stop-anvil

# Clean up build artifacts
just clean
```
