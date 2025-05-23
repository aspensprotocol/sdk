# Apsens CLI
 
a REPL / CLI to interact with an Aspens Markets Stack

## Prerequisites

1. Install Rust:
```bash
# Install Rust using rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. Set up environment variables:
```bash
# Copy the .env sample 
cp app/.env.sample app/.env

# After changing the values in your .env
source app/.env
```

## Usage

The CLI can be used two ways: interactive and scripted.

### Interactive Mode

Interactive mode provides a REPL (Read-Eval-Print Loop) interface where you can execute commands one at a time:

```bash
# Start the REPL
cargo run --bin aspens-cli repl

aspens> help
Usage: <COMMAND>

Commands:
  initialize       Initialize a new trading session by (optionally) defining the arborter URL
  get-config       Config: Fetch the current configuration from the arborter server
  download-config  Config: Download configuration to a file
  deposit          Deposit token(s) to make them available for trading
  withdraw         Withdraw token(s) to a local wallet
  buy              Send a BUY order
  sell             Send a SELL order
  get-orders       Get a list of all active orders
  cancel-order     Cancel an order
  balance          Fetch the balances
  get-orderbook    Fetch the latest top of book
  quit             Quit the REPL
  help             Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
aspens> quit
```

### Scripted Mode

Scripted mode allows you to execute commands directly from the command line, which is useful for automation and scripting:

```bash
❯ cargo run --bin aspens-cli
Aspens CLI for trading operations

Usage: aspens-cli [OPTIONS] <COMMAND>

Commands:
  initialize       Initialize a new trading session
  get-config       Config: Fetch the current configuration from the arborter server
  download-config  Download configuration to a file
  deposit          Deposit token(s) to make them available for trading
  withdraw         Withdraw token(s) to a local wallet
  buy              Send a BUY order
  sell             Send a SELL order
  get-orders       Get a list of all active orders
  cancel-order     Cancel an order
  balance          Fetch the balances
  get-orderbook    Fetch the latest top of book
  help             Print this message or the help of the given subcommand(s)
```
