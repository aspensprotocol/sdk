# Environment Management Guide

This guide explains how to set up and manage multiple environment configurations for the Aspens project.

## Overview

The Aspens project now supports multiple environment configurations, allowing you to easily switch between different setups (local development, testnet, mainnet, etc.) without manually editing files.

## Environment Files

Environment configurations are stored in `.env.{environment}.local` files in the project root:

- `.env.anvil.local` - Local Anvil development environment
- `.env.testnet.local` - Testnet environment (Sepolia, Mumbai, etc.)
- `.env.mainnet.local` - Mainnet environment (when ready)

## Quick Start

### 1. List Available Environments

```bash
# Using the script
./scripts/env-switch.sh --list

# Using just
just env-list
```

### 2. Switch Environments

```bash
# Using the script
./scripts/env-switch.sh anvil
./scripts/env-switch.sh testnet

# Using just
just env-switch anvil
just env-switch testnet
```

### 3. Run Commands with Specific Environment

```bash
# CLI with specific environment
cargo run --bin aspens-cli -- --env anvil
cargo run --bin aspens-cli -- --env testnet

# REPL with specific environment
cargo run --bin aspens-repl -- --env anvil
cargo run --bin aspens-repl -- --env testnet

# Using just shortcuts
just cli-anvil
just cli-testnet
just repl-anvil
just repl-testnet
```

### 4. Run Tests with Specific Environment

```bash
# AMMIT tests
./scripts/ammit.sh anvil
./scripts/ammit.sh testnet

# Using just shortcuts
just test-anvil
just test-testnet
```

## Environment Variables

You can also set the `ASPENS_ENV` environment variable to specify which environment to use:

```bash
export ASPENS_ENV=testnet
cargo run --bin aspens-cli
cargo run --bin aspens-repl
```

## Creating New Environments

### Using the Template Generator

```bash
# Create a new environment template
just create-env staging

# This creates .env.staging.local with placeholder values
```

### Manual Creation

1. Create a new file `.env.{environment}.local`
2. Copy the structure from an existing environment file
3. Update the values for your specific environment

Example structure:

```bash
# Environment Configuration
MARKET_ID_1=wbtc_usdt
MARKET_ID_2=usdc_usdt

# RPC URLs
ETHEREUM_RPC_URL=https://your-rpc-url
POLYGON_RPC_URL=https://your-rpc-url

# Contract addresses
ARBORTER_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890
USDC_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890
USDT_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890
WBTC_CONTRACT_ADDRESS=0x1234567890123456789012345678901234567890

# Private keys (use test accounts only!)
PRIVATE_KEY_1=0x1234567890123456789012345678901234567890123456789012345678901234
PRIVATE_KEY_2=0x1234567890123456789012345678901234567890123456789012345678901234

# Configuration
CHAIN_ID=1
GAS_LIMIT=3000000
GAS_PRICE=20000000000
```

## Environment File Priority

The system uses the following priority order for determining which environment to use:

1. `--env` command-line argument (highest priority)
2. `ASPENS_ENV` environment variable
3. Default value ("anvil")

## Available Commands

### Just Commands

- `just env-list` - List all available environments
- `just env-switch <env>` - Switch to a specific environment
- `just cli-<env>` - Run CLI with specific environment
- `just repl-<env>` - Run REPL with specific environment
- `just test-<env>` - Run AMMIT tests with specific environment
- `just create-env <name>` - Create a new environment template

### Script Commands

- `./scripts/env-switch.sh --list` - List environments
- `./scripts/env-switch.sh <env>` - Switch environment
- `./scripts/ammit.sh <env>` - Run AMMIT tests

## Security Notes

- Never commit environment files with real private keys to version control
- Use test accounts only for development and testing
- Keep your mainnet private keys secure and separate
- Consider using environment variable injection for production deployments

## Troubleshooting

### Environment File Not Found

If you get an error about a missing environment file:

```bash
# Check if the file exists
ls -la .env.*.local

# Create the environment if it doesn't exist
just create-env <environment_name>
```

### Wrong Environment Loaded

If the wrong environment is being loaded:

```bash
# Check current environment variable
echo $ASPENS_ENV

# Clear environment variable
unset ASPENS_ENV

# Use command-line argument instead
cargo run --bin aspens-cli -- --env <environment>
```

### Permission Issues

If you get permission errors with scripts:

```bash
# Make scripts executable
chmod +x scripts/*.sh
```

## Examples

### Development Workflow

```bash
# Start with local development
just cli-anvil

# Switch to testnet for testing
just cli-testnet

# Run tests on both environments
just test-anvil
just test-testnet
```

### CI/CD Integration

```bash
# Set environment for CI/CD
export ASPENS_ENV=testnet

# Run commands without explicit --env flag
cargo run --bin aspens-cli balance
```

### Team Development

```bash
# Each developer can have their own environment
just create-env jack-dev
just create-env alice-dev

# Use personal environments
just cli-jack-dev
just cli-alice-dev
``` 