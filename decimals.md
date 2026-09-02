# Decimal handling in the Aspens SDK

Aspens trades cross-chain tokens that don't agree on decimal precision
(USDC has 6, WFLR has 18, BTC has 8). This doc is the reference for
what number you type, what the CLI / REPL does with it, and what the
arborter / on-chain contracts ultimately see.

The short version: **`aspens-cli` and `aspens-repl` accept human-readable
decimal strings for every amount and price.** You don't pre-scale.
You type `1.5`, not `1500000000000000000`.

## Layers of precision

Aspens has three coexisting precisions for any market:

| Layer | Precision | Where it appears |
|---|---|---|
| Human input | Decimal string (e.g. `"10.5"`) | What you type into `aspens-cli` / `aspens-repl` |
| Pair decimals | Integer in `pair_decimals` units | gRPC payloads (`SendOrder.quantity`, `SendOrder.price`) |
| Token decimals | Integer in each token's native decimals | ERC-20 calls, SPL token amounts, on-chain receipts |

**Pair decimals** is configured per market and may differ from both
base- and quote-token decimals. It's the orderbook's internal
arithmetic precision: every limit price, market quantity, and trade
volume is stored in `pair_decimals` units and converted in/out at the
edges.

**Token decimals** is the per-token `decimals` field from the chain
config (`config.chains[].tokens[].decimals`). It governs ERC-20 calls
on EVM chains and SPL `Mint.decimals` on Solana.

## What the CLI / REPL accept

After the human-readable refactor, every amount-bearing command takes a
decimal string and the binary scales it for you using whichever
precision that command operates in:

| Command | Argument(s) | Scaled by |
|---|---|---|
| `deposit <network> <token> <amount>` | `amount` | `token.decimals` (from config) |
| `withdraw <network> <token> <amount>` | `amount` | `token.decimals` |
| `buy-market <market> <amount>` | `amount` | `market.pair_decimals` |
| `buy-limit <market> <amount> <price>` | `amount`, `price` | `market.pair_decimals` |
| `sell-market <market> <amount>` | `amount` | `market.pair_decimals` |
| `sell-limit <market> <amount> <price>` | `amount`, `price` | `market.pair_decimals` |

Strings accepted: integers (`"10"`), decimals (`"10.5"`), bare-fraction
(`".5"`), trailing-dot (`"10."`), with surrounding whitespace tolerated.
Rejected: empty input, `+`/`-` prefixes, scientific notation, thousands
separators, hex/octal prefixes, alphabetic input, or multiple decimal
points. Excess fractional digits are **truncated, not rounded** —
`"0.9999999"` with 6 decimals becomes `999_999`, not `1_000_000`. See
`aspens::decimals::parse_decimal_amount` for the definitive rules and
the test suite that pins them.

## What changes when

```
"10.5"                          ← what you type
   │ aspens::decimals::parse_decimal_amount(amount, token.decimals)
   ▼
10_500_000  (u128 → u64)        ← what the lib hands to ERC-20 / SPL
                                  for deposit / withdraw

"10.5"                          ← what you type for an order
   │ send_order::convert_to_pair_decimals(amount, market.pair_decimals)
   ▼
10_500_000  (gRPC integer)      ← what arborter receives in SendOrder
   │ gasless::resolve_order
   │   └─ gasless::normalize(amount, pair_decimals[*2], token.decimals)
   ▼
on-chain integer                ← what the user's signature commits to
                                  for the on-chain lock / settle
```

Both legs of every order are normalised, not just one. For a Bid
(side = 1, giving quote):

- `amount_in` = `quantity × price` (in `pair_decimals × 2`) normalised to
  the **input/quote** token's native decimals.
- `amount_out` = `quantity` (in `pair_decimals`) normalised to the
  **output/base** token's native decimals.

For an Ask (side = 2, giving base) the roles flip:

- `amount_in` = `quantity` normalised to the **input/base** token's decimals.
- `amount_out` = `quantity × price` (in `pair_decimals × 2`) normalised to
  the **output/quote** token's decimals.

`amount_in` is the order's **budget**: how much of the asset it gives it
commits. One rule covers all four cells — and a **market bid** is the one
that cannot derive it, because it gives quote and has no price to size that
with. It states the budget instead, as `Order.quote_budget`, already in the
**quote token's own base units** (not pair decimals, so it is passed through
unscaled). A market ask needs nothing extra: it gives base, and its budget IS
its `quantity`. `quote_budget` is rejected on any other order, where the
budget is derived and a stated one could only disagree.

These integers feed `derive_order_id`. What the arborter actually
authenticates is the **outer envelope signature** over the prost-encoded
`Order` (`sign_send_order_envelope`), and `quote_budget` rides inside `Order`
precisely so that signature covers it. There is no on-chain lock and no
per-order on-chain verifier under the optimistic ledger — the arborter
re-derives the collateral requirement from the signed order and reserves it in
the shadow ledger. See `commands/trading/gasless.rs::resolve_order`.

## Real-world examples

Every example below shows the human-typed command. The numbers in
parentheses are what the SDK / arborter compute internally — you do
not type those.

### Example 1: 1.5 ETH at 2,500 USDC on a `pair_decimals = 18` market

```sh
aspens-cli buy-limit "$MARKET" 1.5 2500
```

Internally:
- `quantity = 1.5 × 10^18 = 1_500_000_000_000_000_000`
- `price    = 2500 × 10^18` (in pair decimals)
- BID → quote-leg lock normalised from 18 → 6 decimals (USDC) →
  `2500 × 10^6 × 1.5 = 3_750_000_000` USDC base units.

### Example 2: 0.5 BTC at 45,000 USDT on a `pair_decimals = 8` market

```sh
aspens-cli sell-limit "$MARKET" 0.5 45000
```

Internally:
- `quantity = 0.5 × 10^8 = 50_000_000`
- `price    = 45000 × 10^8`
- ASK → base-leg lock normalised from 8 → 8 (no change) →
  `50_000_000` BTC base units.

### Example 3: A market BUY states its budget in quote

```sh
aspens-cli buy-market "$MARKET" 0.75 --quote-budget 2000
```

A market buy gives quote and has no price to convert `0.75` with, so its
`quantity` bounds nothing — `--quote-budget` does, and it is what the arborter
reserves. Internally, on a market whose quote token is USDC (6 decimals):

- `quote_budget = 2000 × 10^6 = 2_000_000_000` — scaled by the **quote
  token's** decimals, NOT `pair_decimals`, and sent as `Order.quote_budget`.
- `quantity` is still scaled to pair decimals, still signed, and must still be
  greater than zero.

Omit `--quote-budget` and the order is refused before it leaves the process:

> a market BID (side BID, no price) must set Order.quote_budget: it gives
> quote and has no price to size that with, so nothing else bounds what it
> may spend.

A market SELL needs no budget — it gives base, so `amount` IS its budget:

```sh
aspens-cli sell-market "$MARKET" 0.75
```

Passing `--quote-budget` to anything but a market buy is an error, not a
no-op: the budget there is derived from what is already signed, so a stated
one could only disagree with it.

If you want market-like behaviour with an explicit price ceiling instead, use
a limit at a slippage-capped price (`buy-marketable`, or `buy-limit` at
`best_ask × 1.005`).

> **Over the FCE transport**, a market BUY is refused: the direct-action wire
> has no `quote_budget` field, so the adapter would rebuild a different order
> and the signature would not verify. Use a limit buy there.

### Example 4: Deposit 10 USDC (token has 6 decimals)

```sh
aspens-cli deposit base-sepolia USDC 10
```

Internally `amount = 10 × 10^6 = 10_000_000` USDC base units, sent to
the trade contract's `deposit(token, amount)`.

### Example 5: Withdraw 0.25 USDT0 back to wallet

```sh
aspens-cli withdraw flare-coston2-quote USDT0 0.25
```

Internally `amount = 0.25 × 10^6 = 250_000` USDT0 base units.

## Programmatic use (library callers)

If you're calling the library directly (not via the CLI), keep in
mind:

- `aspens::commands::trading::deposit::call_deposit_from_config_with_wallet(... amount: u128 ...)`
  takes **base units**, not human-readable strings. Pre-scale yourself
  using `aspens::decimals::parse_decimal_amount(s, decimals)` (or
  multiply if you already have an integer).
- `aspens::commands::trading::withdraw::call_withdraw_from_config_with_wallet`
  has the same convention.
- On Solana chains, `deposit`/`withdraw` narrow that `u128` to `u64`
  internally (the on-chain instruction takes a native `u64`) via a checked
  `u64::try_from(amount)?`, returning a clear error rather than truncating
  if the amount doesn't fit.
- Order helpers (`send_order_with_wallets`) take `quantity: String` /
  `price: Option<String>` and call `convert_to_pair_decimals` on them
  internally, so the human-readable form works there too.

The CLI / REPL is the only layer that converts strings — the library
surface is integer-typed by design so other clients (UIs, tests) can
work in whichever representation suits them.

## Pitfalls

### Precision loss when normalising across decimals

Markets where the orderbook's `pair_decimals` exceeds the on-chain
token decimals lose precision when settling — e.g., a market with
`pair_decimals = 18` settling against USDC (6 decimals) silently drops
the bottom 12 digits of the lock amount. The arborter performs that
normalisation; the CLI shows the pair-decimal value back to you.

### u64 overflow on deposit / withdraw

The library's deposit / withdraw API takes `u64`, which caps the
maximum depositable amount at `2^64 - 1` base units. For 18-decimal
tokens that's roughly `18.45 × 10^0` whole tokens — i.e. you can't
deposit more than ~18 WFLR in one call. The CLI surfaces this as a
clear "exceeds u64::MAX" error rather than silently truncating.
Workaround: split into multiple deposits, or update the lib API to
`u128` if you genuinely need more.

### Truncation never rounds

The decimal parser truncates fractional digits beyond the configured
precision. `"0.9999999"` with 6 decimals becomes `999_999`, *not*
`1_000_000`. If you need rounding, do it before passing the string in.

### Sub-precision prices become zero

A price of `0.00001` on a market with `pair_decimals = 4` truncates to
zero. The matching engine then accepts the order at price 0 and almost
certainly gives you a worse fill than you intended. Always check that
your typed price scales to a non-zero pair-decimal integer.

## Reference

- `aspens::decimals::parse_decimal_amount(amount: &str, decimals: u32) -> Result<u128>`
  — single source of truth for human-readable → base-units conversion.
  `deposit`/`withdraw` on Solana take `u64`, so narrow the result with
  `u64::try_from(..)?` before calling them there.
- `aspens/src/decimals.rs` test module — pins parsing, truncation,
  overflow, and rejection behaviour. If you change parsing rules,
  update those tests first.
- `aspens/src/commands/trading/send_order.rs::convert_to_pair_decimals`
  *(private)* — the order path's thin wrapper that returns the gRPC
  `String` form; not callable from outside the crate.
- `aspens/src/commands/trading/gasless.rs::normalize`
  *(private)* — the per-leg `pair_decimals → token_decimals` rescale
  that produces the integers the user's signature commits to. Unit
  tests in the same file cover identity / downscale-truncation /
  upscale-overflow.
