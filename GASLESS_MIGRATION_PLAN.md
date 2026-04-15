# SDK → gasless send_order migration plan

## Current state

`aspens/src/commands/trading/send_order.rs` builds a `SendOrderRequest` with
`gasless: None` (line 153). This routes arborter into the legacy
`lock_for_order` path on every order.

Arborter's state (current `main`):
- **chain-evm** `lock_for_order` is now a typed-error stub directing callers
  at `lock_for_order_gasless` (PR #122, tech-debt P1).
- **chain-solana** `lock_for_order` has returned an error since the gasless
  pipeline landed (PR #115).

**⇒ SDK-originated orders currently fail at the arborter's trait boundary
on both chains.** This migration is the fix.

The signing helpers already live in the SDK (`aspens::orders`,
`aspens::evm`, `aspens::solana`, per CLAUDE.md). What's missing is the glue
that ties them together inside `send_order` and attaches the resulting
`GaslessAuthorization` to the proto request.

## Arborter-side contract (what we must produce)

From `arborter/app/protos/proto/arborter.proto`:

```proto
message GaslessAuthorization {
  bytes  user_signature = 1;  // 64 Ed25519 or 65 ECDSA
  uint64 deadline       = 2;  // Solana slot / EVM fillDeadline (unix s)
  string order_id       = 3;  // 0x-prefixed 32-byte hex
  uint64 nonce          = 4;  // EVM Permit2 nonce; Solana ignores
  uint64 open_deadline  = 5;  // EVM-only openDeadline (unix s)
}
```

And from `arborter/app/chain-traits/src/market.rs::GaslessLockParams`:
```
depositor_address, token_contract, token_contract_destination_chain,
destination_chain_id, amount_in, amount_out, order_id,
deadline, nonce, open_deadline, user_signature
```

The arborter's handler (`server/src/handlers/send_order.rs`) branches on
`gasless.is_some()` and forwards the bundle to `lock_for_order_gasless` —
we just have to populate it correctly.

## SDK → arborter data dependency map

Every field we need lives in `GetConfigResponse` or is computable from the
wallet + order intent:

| Gasless field            | Source in SDK                                                                |
|--------------------------|------------------------------------------------------------------------------|
| depositor_address        | `wallet.address()`                                                           |
| token_contract           | `Market.tokens[origin_symbol].contract_address`                              |
| token_dest_chain         | `Market.tokens[dest_symbol].contract_address` on the opposite chain          |
| destination_chain_id     | `Chain.chain_id` (stringified) of the opposite chain                         |
| amount_in / amount_out   | `quantity_raw` + `price_raw` (pair-decimals; already computed)               |
| order_id                 | `aspens::orders::derive_order_id(...)` keyed on user+nonce+market            |
| deadline                 | EVM: now + 24h (u32 unix). Solana: current slot + buffer (queried via rpc)   |
| open_deadline            | EVM: now + 1h. Solana: 0 (ignored).                                          |
| nonce                    | EVM: unix seconds (matches arborter's legacy unique_nonce scheme). Solana: 0 |
| user_signature           | Chain-specific — see below                                                   |

### Per-chain signing

- **EVM**: build the `GaslessCrossChainOrder` via
  `aspens::evm::build_gasless_cross_chain_order(params, arborter_addr,
  origin_settler, origin_chain_id)`; compute the EIP-712 digest via
  `aspens::evm::gasless_lock_signing_hash(...)`; sign with
  `wallet.sign_message(hash.as_slice())` — `Wallet::sign_message` on EVM
  already applies the EIP-191 wrap the contract expects (arborter's PR #122
  docstring flags that `sign_hash_sync` would fail with `INVALID_SIGNER`).
- **Solana**: build `aspens::solana::OpenOrderArgs`; compute borsh bytes
  via `aspens::solana::gasless_lock_signing_message(instance, user,
  deadline, &order)`; sign with `wallet.sign_message(&message)` — Ed25519
  64-byte signature, no wrap.

Arborter-side cross-checks:
- `chain_evm::market::build_gasless_cross_chain_order` (same function,
  same layout — parity snapshot in `aspens/tests/client_parity.rs` must
  stay current)
- `chain_solana::instructions::gasless_lock_signing_message` (200-byte
  borsh layout, snapshot-tested)
- `chain_traits::market::derive_order_id` (same sha2 inputs in the same
  order — also parity-snapshotted)

### Which chain signs?

The "origin" of the lock is whichever chain the user is *locking on* for
this order. Handler convention (arborter `server/src/handlers/send_order.rs`):

- **Bid (buying base)** → user locks on the **quote** chain.
- **Ask (selling base)** → user locks on the **base** chain.

Pick origin_chain via `side`. The opposite chain supplies `destination_chain_id`
and `token_contract_destination_chain`.

## Code changes

### 1. `aspens/src/commands/trading/send_order.rs`

Add a helper `fn build_gasless_authorization(config, market, side, wallet,
quantity_raw, price_raw) -> Result<GaslessAuthorization>` that:

1. Resolves origin + destination chains + tokens from `market_id` and `side`.
2. Reads arborter address from `origin_chain.instance_signer_address`,
   instance from `origin_chain.trade_contract.address`, chain_id from
   `origin_chain.chain_id`.
3. Generates a client nonce (unix seconds — matches arborter's legacy
   nonce scheme so if any on-chain code still looks for collision, it
   behaves).
4. Computes `order_id = aspens::orders::derive_order_id(...)` with
   `(user_pubkey, nonce, origin_chain_id, dest_chain_id, input_token,
   output_token, amount_in, amount_out)`.
5. Computes `deadline` + `open_deadline`:
   - EVM: now + 24h / now + 1h.
   - Solana: fetch current slot via `solana-client` (feature `client`),
     deadline = slot + 100. `open_deadline = 0`.
6. Builds `GaslessLockParams`.
7. Chain-dispatches on `origin_chain.architecture`:
   - `"EVM"`: `aspens::evm::gasless_lock_signing_hash(&params, ...)` →
     `wallet.sign_message(hash.as_slice())`.
   - `"Solana"`: build `OpenOrderArgs`, then
     `aspens::solana::gasless_lock_signing_message(...)` →
     `wallet.sign_message(&message)`.
8. Returns `GaslessAuthorization { user_signature, deadline, order_id,
   nonce, open_deadline }`.

`call_send_order` threads the `Option<GaslessAuthorization>` through and
sets it on the `SendOrderRequest`.

`send_order_with_wallet` calls `build_gasless_authorization` and
unconditionally populates the field. We drop the `None` path.

### 2. Decimals / addresses — no new parsing

All the needed fields (token contract addresses, chain ids, pair decimals)
are in `GetConfigResponse`; no new RPC calls beyond the Solana
`getSlot` for the deadline. Existing helpers (`lookup_market`,
`convert_to_pair_decimals`) stay untouched.

### 3. Parity snapshot

Update / add tests in `aspens/tests/client_parity.rs`:
- Solana: regenerate the borsh-payload layout snapshot with the same
  inputs arborter's `instructions::gasless_lock_signing_message_layout_is_stable`
  checks (200-byte layout, instance / user / deadline / order order_id).
- EVM: ensure `build_gasless_cross_chain_order` + domain/hash match
  arborter's `gasless_order_signature_round_trips` expectations.

The parity tests should fail loudly if either side drifts.

### 4. Integration test: SDK → arborter round-trip

Stand up arborter + both chains the way the cross-chain e2e test does
(see `arborter/app/chain-solana/tests/cross_chain.rs`), then:
1. Use `aspens::AspensClient` to call `send_order_with_wallet`.
2. Assert the `SendOrderResponse.transaction_hashes` includes a
   `send_order_tx` — that confirms the arborter accepted the gasless
   auth and dispatched `lock_for_order_gasless` successfully.

Two variants: EVM-origin + Solana-origin orders. Both `#[ignore]`d like
the existing cross-chain tests.

## Cross-repo crosscheck (arborter-side)

I verified these against `../arborter/main`:

- ✅ `SendOrderRequest.gasless` is `optional GaslessAuthorization` —
  populated request is accepted (`app/server/src/handlers/send_order.rs`
  lines 327–340 + 507–520).
- ✅ `GaslessLockParams` field layout matches the proto — every field we
  plan to populate is consumed.
- ✅ EIP-712 domain / struct layout on both sides is the same function
  (`build_gasless_cross_chain_order`) — parity by construction.
- ✅ Solana borsh payload layout identical (single 200-byte schema,
  snapshot-tested on both sides).
- ✅ `derive_order_id` is the same sha2 recipe with identical field
  order — already checked via `client_parity.rs`.

One subtle risk to flag:
- **EVM `nonce` collision**: arborter's legacy path used
  `unix_seconds` as nonce. If the SDK uses the same scheme, two SDK
  clients issuing orders in the same second produce colliding nonces →
  Permit2 replay rejection. Safer recipe: `nonce = unix_millis` (u64 is
  huge enough), or a random u64. Recommend **unix_millis** — still
  time-ordered, 1000× collision headroom.

## Rollout order

1. ✅ **Stateless helper** — `build_gasless_authorization`, merged via PR #58.
2. ✅ **Wire into `call_send_order`** — attaches `GaslessAuthorization`
   to every `SendOrderRequest`; PR #59.
3. ✅ **Parity test refresh** — arborter-fixture cross-check tests added
   (`solana_message_matches_arborter_fixture`,
   `evm_hash_matches_arborter_fixture`); PR #60.
4. ✅ **Live integration test** — `aspens/tests/send_order_live.rs` +
   `just test-live-send-order` recipe. `#[ignore]`d; driven off env
   vars documented in the test header. External prerequisites match
   arborter's `test-cross-chain-integration` recipe.
5. **Legacy-ready comment + `gasless: None` fallback deletion** — the
   `call_send_order` signature still accepts `Option<GaslessAuthorization>`
   for flexibility; every caller passes `Some(..)`. Can be tightened to
   `T` once the integration test has been exercised against a live stack
   in CI / infra.

## Out of scope

- Decimals helpers migrating out of arborter handler → SDK (tech-debt P9).
  Separate effort.
- Admin commands (`deposit`, `withdraw`, `cancel_order`) — those stay
  user-signed on Solana and arborter-signed on EVM; no gasless variant
  today.
- Batching / multi-order requests — the proto is single-order.

## Success signals

- SDK's `just test-lib` stays green after the refactor.
- `aspens/tests/client_parity.rs` passes.
- New SDK ↔ arborter integration test (`send_order_roundtrip`) passes
  against a live stack.
- The corresponding `gasless: None` path is deletable, not just unused.
