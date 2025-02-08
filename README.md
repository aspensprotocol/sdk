# Apsens CLI
 
a REPL style CLI to interact with an Aspens Markets Stack

## Getting Started

Prerequisite: Ensure [rust](https://rustup.rs/) is installed on your system.

```bash
# copy the .env sample 
cp .env.sample .env

# after changing the values in your .env
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
