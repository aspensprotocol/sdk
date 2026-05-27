# Changelog

All notable changes to the Aspens SDK workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html);
see the **Versioning** section in `README.md` for what counts as a breaking
change before 1.0.

## [Unreleased]

### Changed
- Split `commands/trading/gasless.rs` (810 LOC) into
  `gasless/{mod.rs, evm.rs, solana.rs}` — the EVM and Solana
  `build_*` branches each live next door to the dispatcher now
  instead of all sharing one file.
- Split `commands/trading/send_order.rs` (855 LOC) — the proto
  `Display` impls and CLI-formatting helpers moved to a sibling
  `send_order/display.rs`; signing / RPC dispatch stays in `mod.rs`.
- Introduced a `GaslessBuildArgs` struct used by both gasless
  branches; retires four `#[allow(clippy::too_many_arguments)]`
  attributes (was 8, now 4). The 4 remaining allows are on the
  public top-level `send_order` API and `derive_order_id` —
  documented with comments explaining why those argument lists
  stay flat (consensus recipe / public API surface).
- Production-path `.unwrap()` calls now `.expect("...")` with a
  descriptive message:
  - `executor::BlockingExecutor::new` (tokio runtime build)
  - `solana::{sysvar_rent_id, sysvar_instructions_id,
    ata_program_id, ed25519_program_id}` (well-known program /
    sysvar pubkeys — parse failure would mean a catastrophic
    solana-sdk regression, but the panic message now says so)
- The other ~140 `.unwrap()` calls in the lib were audited and
  found to be inside `#[cfg(test)]` modules or doc-test examples,
  where the test framework already prints clear failures. No
  changes to test code.

### Removed (breaking, pre-1.0)
- Retired the legacy `privkey: String` wrappers around the
  curve-agnostic trading API. The `_with_wallet` (and `_with_wallets`)
  family is the only public shape now. Specifically deleted:
  - `commands::trading::send_order::send_order`
  - `commands::trading::cancel_order::call_cancel_order`
  - `commands::trading::cancel_order::call_cancel_order_from_config`
  - `commands::trading::deposit::call_deposit_from_config`
  - `commands::trading::withdraw::call_withdraw_from_config`
  - `commands::trading::balance::balance_from_config`
  - `commands::auth::authenticate_with_signature`
- Deleted the legacy two-chain pretty-printer
  `commands::trading::balance::balance` plus its private-key-derived
  helpers (`call_get_balance`, `call_get_locked_balance`,
  `call_get_erc20_balance`, `call_get_native_balance`) and the
  `balance_table` helper. The address-based variants
  (`call_get_*_for_address`) and `format_balance` are kept — they're
  used by `aspens-admin`.
- Removed the `chain_client::derive_associated_token_account`
  re-export. The function still lives at
  `aspens::solana::derive_associated_token_account`.

### Changed
- `aspens-repl` and the `quickstart` example now use the curve-aware
  `_with_wallet` API directly. The REPL constructs an EVM `Wallet`
  from `TRADER_PRIVKEY` via a new `load_trader_wallet_or_complain`
  helper; the example re-builds a `Wallet` inside each `async move`
  block since `Wallet` is intentionally not `Clone` (Solana keypairs).
- Binaries (`aspens-cli`, `aspens-repl`, `aspens-admin`) now declare the
  `aspens` library features they depend on explicitly
  (`client`, `evm`, `solana`, `formatting`, plus `admin` for the admin
  binary) and turn off default-feature inheritance. Changes to the
  `aspens` crate's default features no longer silently affect binaries.
- Binaries (`aspens-cli`, `aspens-repl`, `aspens-admin`) now declare the
  `aspens` library features they depend on explicitly
  (`client`, `evm`, `solana`, `formatting`, plus `admin` for the admin
  binary) and turn off default-feature inheritance. Changes to the
  `aspens` crate's default features no longer silently affect binaries.
- Internal helpers in `AspensClient` that take `RwLock` guards now
  `expect("AspensClient ... lock poisoned")` instead of `unwrap()`, so a
  poisoned lock surfaces with a clear message instead of a generic panic.
- `chain_curve`, `load_trader_wallet_for_chain`, and
  `load_trader_wallet_for_network` are now gated behind the `client`
  feature. They depend on the proto-generated `Chain` /
  `GetConfigResponse` types in `commands::config`, which are themselves
  `client`-only — gating these helpers makes the lean-signing build
  (`--no-default-features --features evm,solana`) work.
- `aspens::grpc` is marked `#[doc(hidden)]`. The gRPC channel helpers it
  exposes are internal — not part of the stable public API.
- Examples moved from `examples/` to `aspens/examples/` so Cargo picks
  them up automatically (`cargo run -p aspens --example <name>`).
- CI workflow now reads the Rust toolchain from `rust-toolchain.toml`
  instead of pinning `stable` per job. Local and CI builds now use the
  same compiler version.
- CI now builds examples (`cargo build -p aspens --examples`) and runs
  a lean-signing job (`aspens --no-default-features --features
  evm,solana`) so example bitrot and accidental `client`-feature
  coupling are caught on every PR.

### Fixed
- `justfile`: `test-anvil` and `test-testnet` recipes now call
  `python3 scripts/ammit.py` directly. The previous `./scripts/ammit.sh`
  invocations referred to a script that no longer exists.
- `aspens/examples/transaction_hash_example.rs`: updated
  `SendOrderResponse` initializer to include the `current_orderbook`
  and `order_id` fields added in the 0.4.x proto sync.

### Removed
- `scripts/ammit-multi-token.sh` deleted. `scripts/ammit.py` is the
  single supported AMMIT entry point.

## [0.4.3] — 2026-05-08

### Added
- `feat(aspens-cli)`: `buy-marketable` / `sell-marketable` helpers
  (`7600288`).
- `feat(aspens-admin)`: `--instance-signer-address` flag on `set-chain`
  (`65b077d`).

### Changed
- `chore(deps)`: bumped workspace dependencies to latest (`282df55`).

### Fixed
- `fix(orders)`: try base58-32-bytes before hex for unprefixed
  destination-token inputs (`e527050`).
- `fix(gasless)`: sign amounts in `token_decimals` and forward them
  verbatim to arborter; reject market orders on the gasless path
  (`6487c48`).
- `fix(cli,repl)`: accept human-readable amounts for `deposit` and
  `withdraw` (`bec1ab2`).
- `fix(send_order)`: pick wallet per chain by curve for cross-chain
  orders (`1a403c6`).
- `fix(send_order)`: send full curve-native signature (C1) (`df2abbd`).
- `fix(cross-chain)`: widen `OrderData.outputToken` to `bytes32`
  (`aa31e5f`).

## [0.4.2] — 2026-04-30

### Added
- `docs(aspens)`: filled missing rustdoc on the public surface
  (`92f50f3`).

### Changed
- `refactor(cli)`: extracted `dispatch_send_order` helper (`ed1d66b`).
- `refactor(lib)`: exposed `chain_curve`,
  `load_trader_wallet_for_chain/network`, `origin_network_for_side`,
  `parse_side` (`7e44b57`).
- `refactor(config)`: collapsed `call_get_config` into `get_config`
  (`ca2f772`).

### Fixed
- `fix(cli)`: dispatch wallet curve for all trading commands
  (`2a83b07`).
- `fix(cli)`: dispatch deposit wallet curve on chain architecture
  (`af93663`).
- `fix(gasless)`: match chain architecture case-insensitively
  (`e2d9ec0`).

## [0.4.1] and earlier

Pre-0.4.1 history is recorded in git only. The 0.4.x line introduced
Solana support, the Wallet enum, and feature gates (`evm`, `solana`,
`client`) for lean-signing consumers.

[Unreleased]: https://github.com/aspensprotocol/sdk/compare/0.4.3...HEAD
[0.4.3]: https://github.com/aspensprotocol/sdk/releases/tag/0.4.3
[0.4.2]: https://github.com/aspensprotocol/sdk/releases/tag/0.4.2
