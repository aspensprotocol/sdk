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
cp .env.sample .env

# After changing the values in your .env
source .env
```

## Usage

```bash
$ EVM_TESTNET_PUBKEY=$EVM_TESTNET_PUBKEY_ACCOUNT_1 \
  EVM_TESTNET_PRIVKEY=$EVM_TESTNET_PRIVKEY_ACCOUNT_1 \
  cargo run

# by default, connects to arborter running on localhost:50051. instead, 
# connect to a remote aspens stack at <arborter-url>
aspens> initialize https://<arborter-url>

# to see all commands
aspens> help 

# to end the session
aspens> quit
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
cargo run

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
# Stop Anvil instances
just stop-anvil

# Clean up build artifacts
just clean
```
