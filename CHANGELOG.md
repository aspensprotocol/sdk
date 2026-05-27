# Changelog

All notable changes to the Aspens SDK workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html);
see the **Versioning** section in `README.md` for what counts as a breaking
change before 1.0.

## [Unreleased]

## [0.5.0] — 2026-05-27

A tech-debt sweep that retired legacy public API and tightened the
crate's surface. Bumped to a minor release because of the source-
breaking removals listed below — patch-compatible per the
**Versioning** section in `README.md` would require keeping those
items.

### Breaking — removed public items

- **Legacy `privkey: String` wrappers retired.** The `_with_wallet`
  (and `_with_wallets`) family is now the only public shape:
  - `commands::trading::send_order::send_order` → use
    `send_order_with_wallet` / `send_order_with_wallets`.
  - `commands::trading::cancel_order::call_cancel_order` and
    `call_cancel_order_from_config` → use the `_with_wallet`
    variants.
  - `commands::trading::deposit::call_deposit_from_config` → use
    `call_deposit_from_config_with_wallet`.
  - `commands::trading::withdraw::call_withdraw_from_config` → use
    `call_withdraw_from_config_with_wallet`.
  - `commands::trading::balance::balance_from_config` → use
    `balance_from_config_with_wallets` (multi-wallet, curve-aware).
  - `commands::auth::authenticate_with_signature` → use
    `authenticate_with_wallet`.
- **Legacy two-chain balance helpers deleted.**
  `commands::trading::balance::balance` and its private-key-derived
  primitives (`call_get_balance`, `call_get_locked_balance`,
  `call_get_erc20_balance`, `call_get_native_balance`,
  `balance_table`) were unused inside the workspace and have been
  removed. The address-based variants (`call_get_*_for_address`) and
  `format_balance` are kept — they're used by `aspens-admin`.
- **`chain_client::derive_associated_token_account` re-export
  removed.** The canonical location is
  `aspens::solana::derive_associated_token_account`.
- **`aspens::grpc` marked `#[doc(hidden)]`.** The gRPC channel
  helpers it exposes are internal — not part of the stable public
  API.

### Added

- New `aspens-cliutil` workspace crate (`publish = false`) holding the
  canonical `format_error(err, context, &BinaryContext)` and
  `resolve_token_amount` shared by `aspens-cli`, `aspens-repl`, and
  `aspens-admin`. Retires ~680 lines of triplicated CLI scaffolding;
  binaries now keep a thin local wrapper that picks up the right
  `BinaryContext` (binary name + privkey env var) and delegate.
- New `aspens::evm::rpc` submodule containing the `#[sol(rpc)]`-enabled
  `MidribV2` + `IERC20` bindings (gated on the `client` feature). The
  stateless `aspens::evm::MidribV2` struct-only bindings stay put for
  lean-signing consumers.
- **Supply-chain CI**: `deny.toml` at the workspace root and a new
  `.github/workflows/security-audit.yml` that runs
  `EmbarkStudios/cargo-deny-action` over `advisories ∪ bans ∪
  licenses ∪ sources` on every PR plus a daily cron.
- **New CI jobs**: `cargo build -p aspens --examples` (catches example
  bitrot) and a `build-lean-signing` job that exercises
  `--no-default-features --features evm,solana` so the lean-signing
  build path is gated on every PR.
- **Signing-primitive tests**: round-trip sign-with-`Wallet` →
  recover-with-`Signature` test in `aspens::evm` that mirrors the
  arborter's on-chain verifier (`arborter/app/onchain/src/verify.rs`)
  exactly; plus EIP-191 length-prefix coverage across byte counts.
- **Trading-command tests**: 10 new tests in `commands::trading::balance`
  covering `format_balance_with_decimals`, `format_balance`, and
  `select_wallet_for_chain` (multi-curve wallet routing).
- **Build-script tests**: `aspens/build.rs`'s attestation path-rewrite
  logic extracted to a shared `aspens/build_attestation_paths.rs`
  and covered by 6 unit tests including an on-disk regression guard.
- New top-level docs: `CHANGELOG.md` (Keep-a-Changelog) and a
  **Versioning** section in `README.md`.

### Changed

- Binaries (`aspens-cli`, `aspens-repl`, `aspens-admin`) now declare
  their `aspens` features explicitly with
  `default-features = false, features = ["client", "trader", "evm",
  "solana", "formatting"]` (+ `"admin"` for the admin binary). Changes
  to the library's default features no longer silently affect
  binaries. All three binaries also gained `publish = false` —
  they're not on crates.io.
- `commands/trading/gasless.rs` (810 LOC) split into
  `gasless/{mod.rs, evm.rs, solana.rs}`; EVM and Solana
  `build_gasless_authorization` branches now sit next door to the
  dispatcher. `GaslessBuildArgs` retires 4 of the 8
  `#[allow(clippy::too_many_arguments)]` attributes.
- `commands/trading/send_order.rs` (855 LOC) split into
  `send_order/{mod.rs, display.rs}` so the proto `Display` impls and
  CLI-formatting helpers live separately from signing / RPC dispatch.
- Production-path `.unwrap()` → `.expect("...")` with descriptive
  messages: `BlockingExecutor::new` (tokio runtime build) and the
  four well-known Solana sysvar / program IDs
  (`sysvar_rent_id`, `sysvar_instructions_id`, `ata_program_id`,
  `ed25519_program_id`). The remaining ~140 unwraps in the lib were
  audited and confirmed test-only.
- `AspensClient` `RwLock.unwrap()` (8 sites) →
  `.expect("AspensClient ... lock poisoned")` so a poisoned lock
  surfaces with a clear message instead of a generic panic.
- `chain_curve`, `load_trader_wallet_for_chain`, and
  `load_trader_wallet_for_network` are now gated behind the
  `client` feature. They take proto types that were already
  `client`-only — gating them makes the documented lean-signing build
  (`--no-default-features --features evm,solana`) actually work.
- Examples moved from `examples/` to `aspens/examples/` so Cargo
  picks them up natively. Declared with `required-features = ["client"]`
  so lean-signing builds skip them automatically. Run with
  `cargo run -p aspens --example <name>`.
- CI workflow reads the Rust toolchain from `rust-toolchain.toml`
  via `actions-rust-lang/setup-rust-toolchain@v1` instead of pinning
  `stable` per job. Local and CI builds now use the same compiler.
- `aspens-repl` and the `quickstart` example now use the curve-aware
  `_with_wallet` API directly. The REPL constructs an EVM `Wallet`
  from `TRADER_PRIVKEY` via a new `load_trader_wallet_or_complain`
  helper; the example re-builds a `Wallet` inside each `async move`
  block since `Wallet` is intentionally not `Clone` (Solana keypairs).
- `aspens/Cargo.toml`: corrected the misleading comment claiming
  `trader` / `admin` features don't gate deps — both gate `commands::*`
  modules in `commands/mod.rs`.

### Fixed

- `decimals.md`: audited end-to-end and corrected four real
  discrepancies — diagram referenced a nonexistent
  `normalize_decimals` (real function is `gasless::normalize`); the
  BID/ASK paragraph misattributed normalization to `send_order.rs`
  as the "arborter-side mirror" (it's `gasless::resolve_order`,
  SDK-side); Example 3 (`buy-market`) claimed market orders execute
  but they're rejected at `gasless::resolve_order`; reference section
  now marks `convert_to_pair_decimals` as private and adds
  `gasless::normalize`.
- `justfile`: `test-anvil` and `test-testnet` recipes now invoke
  `python3 scripts/ammit.py`. The previous `./scripts/ammit.sh`
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

[Unreleased]: https://github.com/aspensprotocol/sdk/compare/0.5.0...HEAD
[0.5.0]: https://github.com/aspensprotocol/sdk/compare/0.4.3...0.5.0
[0.4.3]: https://github.com/aspensprotocol/sdk/releases/tag/0.4.3
[0.4.2]: https://github.com/aspensprotocol/sdk/releases/tag/0.4.2
