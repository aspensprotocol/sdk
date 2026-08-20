# CLAUDE.md

Guidance for working in this repo. This file is deliberately limited to things
you **can't** infer from the code, `README.md`, or config — invariants,
deliberate prohibitions, and cross-repo contracts. For everything else:

- **Usage, examples, client API** → `README.md`
- **Build / test / run / lint commands** → `justfile` (`just build`, `just test`, `just check`, `just lint`, `just cli`, `just repl`, `just admin`)
- **Decimal conversions** → `decimals.md`
- **Env vars** → `.env.sample`
- **Module surfaces / public API** → read the module; do not trust an enumeration here (it rots when symbols are renamed)

## What this is

Aspens SDK — a Rust Cargo workspace for cross-chain trading. Crates:
`aspens` (core lib), `aspens-cli`, `aspens-repl`, `aspens-admin`.

## Architectural constraints (decisions, not facts — don't quietly undo them)

- **The core `aspens` lib has NO CLI dependencies** (no `clap`/`clap-repl`).
  Binaries are thin wrappers that call library functions directly; env/`.env`
  loading and config live in the library (`AspensClient`), not the binaries.
- **Protocol buffers are internal.** The `proto/` types are an implementation
  detail; the public API exposes clean Rust types. Don't leak `prost` types.
- **`*_with_wallet` (and `*_with_wallets`) is the only public trading/auth
  shape.** The earlier `privkey: String` wrappers are gone — callers go
  through `Wallet` (`aspens/src/wallet.rs`, the EVM/Solana curve
  enum). Don't reintroduce string-key entry points.

## Feature gating (non-obvious; preserve the dependency separation)

The `aspens` crate has three orthogonal, default-on feature groups:

- **`evm`** / **`solana`** — *stateless* signing helpers only (`aspens::evm`,
  `aspens::solana`, `aspens::orders`). No runtime deps beyond the curve crates.
- **`client`** — the full gRPC + RPC runtime (`AspensClient`, commands,
  `chain_client`, executor, auth, RPC submission). Pulls `tonic`/`prost`/
  `tokio`/`solana-client`/`alloy-contract`. The RPC-enabled `sol!` bindings
  (`aspens::evm::rpc`) and `aspens::solana::client` are gated here.

The point of the split: a browser / embedded / lean-signing consumer builds and
signs orders with `--no-default-features --features evm,solana` and pulls **none**
of tonic/tokio/solana-client/gRPC codegen. Keep stateless helpers free of
`client`-only deps, or this guarantee breaks. Binaries inherit defaults (all on).

## Cross-repo parity with arborter (the highest-value invariant here)

The SDK's client-side signing/hashing is a port of the single reference
implementations in `arborter/app/{chain-traits,chain-evm,chain-solana}`. Any
change to a hashing recipe, EIP-712 domain, or borsh layout on **either** side
must be mirrored on the other, plus the snapshot tests in
`aspens/tests/client_parity.rs`. A one-bit drift fails order submission
*silently* (the arborter recovers/validates a different value). Specifics:

- **`aspens::orders::derive_order_id`** is the one shared order-id recipe used
  by EVM, Solana, and the arborter. Drift → silent id-validation failure.
- **EVM EIP-712 domain** is name `"Midrib"`, version `"3"` (MidribV3). Lives in
  `aspens::evm::{MIDRIB_EIP712_NAME, MIDRIB_EIP712_VERSION}` and must equal the
  contract's `_domainNameAndVersion` + the arborter's constants.
- **Solana**: PDA seeds, account orderings, and Anchor discriminators
  (`sha256("global:<method>")[..8]`) in `aspens/src/solana/mod.rs` must stay in
  lock-step with the on-chain `midrib` program.

## Optimistic-ledger model (post-burn)

Trading is off-chain in the TEE; the chain only sees deposits, net settlement
(`settleBatch`), and TEE-voucher withdrawals. Consequences for the SDK:

- **Order entry is authenticated by the outer envelope signature only**
  (`aspens::evm::sign_send_order_envelope`, EIP-191 over the encoded order — the
  counterpart to arborter's `is_signature_valid`). `SendOrderRequest` is now
  just the order and that signature: `OrderAuthorization` is gone. There is
  **no per-order on-chain lock signature** — the legacy gasless `open`/`open_for`
  lock signing (EVM `GaslessCrossChainOrder`, Solana `OpenForSignedPayload`)
  was burned.
- **Anything the arborter acts on must be INSIDE `Order`**, where the envelope
  signature covers it. Both fields `OrderAuthorization` carried failed that
  test and were deleted with it: `amount_in` set an encumbrance from outside
  the signature, and `order_id` was taken verbatim from the caller. The
  arborter derives both from the signed order now, and the id's last unsigned
  input moved in as **`Order.nonce`** (field 12). A caller still derives its
  own copy of the id (`aspens::orders::derive_order_id`) but only for tracking
  — see the parity note above, and note the failure mode: hash anything the
  arborter doesn't, and the order is accepted while the two sides track
  different ids, with nothing on the wire to say so.
- **An order commits a budget, denominated in the asset it gives.** Three
  cells derive their budget from the signed `(quantity, price)`; a **market
  bid** cannot, and states it as `Order.quote_budget` — in the QUOTE TOKEN's
  own base units, not pair decimals, required there and rejected on every
  other cell. `quantity` is ignored for sizing a market bid but is still
  signed and must be non-zero.
- **The FCE direct-action transport signs `nonce = 0`, always**
  (`fce_actions::FCE_ORDER_NONCE`). Its wire has no nonce field and the
  adapter rebuilds `Order` without one, so anything else fails signature
  verification there. Consequence: over FCE, two identical orders from one
  wallet derive the same id and the second is refused as a replay.
- **Markets no longer carry token decimals.** `SetMarketRequest` lost
  `base_/quote_chain_token_decimals`; register the token first and the
  arborter reads them from the `tokens` table. It also matches the market's
  token addresses against that table **byte-for-byte, case included** — and
  nothing on the SDK's admin path normalises case, deliberately.
- **The order id's token strings are cut out of `Order.market_id`**, and never
  read from `Token.address`. The id hashes each token as its address STRING,
  so `0xAbC…` and `0xabc…` are two ids for one order; `market_id` is the only
  spelling inside the signed message, so it is the only one the arborter can
  hash. `SetMarket` asserts the two agree at registration, but nothing
  re-checks afterwards — re-register the token under another spelling and the
  market keeps the old one. `aspens/tests/client_parity.rs` pins the whole
  composition against a vector shared with the arborter and terminal-ui;
  changing it means changing all three.
- The live EVM contract is **MidribV3** (`artifacts/MidribV3.json`); MidribV3
  has no on-chain locked balance, so EVM "locked" reads as 0.
- Settlement (`settleBatch` / Solana `settle_batch`) is arborter-signed and
  lives in the arborter, not the SDK.

## Decimals (correctness gotcha)

The matching engine + gRPC API work in **pair decimals**, not native token
decimals; an amount in the wrong scale is accepted and mis-priced rather than
rejected. Convert at the boundary — see `decimals.md`.
