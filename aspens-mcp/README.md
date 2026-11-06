# aspens-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server over the Aspens SDK. It lets
an autonomous AI agent trade on an Aspens venue, within per-market limits that you set in a config
file. The agent cannot change those limits.

It speaks MCP over stdio only. Your agent harness spawns it as a child process on the same
machine; it opens no network listener. Logs go to stderr; stdout carries nothing but JSON-RPC.

## Custody warning

**Read this before you give it a key.**

The limits bound an agent that misbehaves **through MCP calls**: a buggy policy, a prompt
injection, a confused model. They do **not** bound a harness that is itself compromised. The
harness spawns `aspens-mcp` as its own OS user, so the same user can read the key file and sign
directly, edit the config to loosen limits, or point `state_dir` somewhere fresh to reset daily
usage. No privilege separation is built (no separate uid, no launcher). **The key file is the
funds; whoever can read it has no limits.**

The price guard does not protect you on a thin book. It compares each order with the venue's own
book at the moment of the order, and on a thin book someone else can set the best bid or ask. The
guard then follows them. What holds is size: a buy can never commit more quote than
`max_order_quote` per order and `max_daily_quote` per day, and a sell never more base than
`max_order_qty` / `max_daily_qty`. A resting order can also be filled later at its own price after
the book has moved away; that is the ordinary risk of a limit order, not something the guard
covers. Set the caps to what you can afford to lose to a bad fill.

## Install

From a checkout of this repo:

```bash
cargo install --locked --path aspens-mcp
```

or straight from git:

```bash
cargo install --locked --git https://github.com/aspensprotocol/sdk aspens-mcp
```

## Configure

The config is [`aspens-mcp.example.toml`](aspens-mcp.example.toml). Copy it to
`~/.aspens-mcp/aspens-mcp.toml` (the default path) and fill it in:

```bash
mkdir -p ~/.aspens-mcp && chmod 700 ~/.aspens-mcp
cp aspens-mcp/aspens-mcp.example.toml ~/.aspens-mcp/aspens-mcp.toml
```

- `stack_url` is the venue's gRPC endpoint.
- `env_file` holds the trader keys, the same variables `aspens-cli` reads: `TRADER_PRIVKEY` (EVM
  and Tron share it), `TRADER_PRIVKEY_SOLANA`, and `ASPENS_RPC_URL_<NETWORK>` per chain. It must
  exist and be mode `0600`, or the server refuses to start. The config itself holds no secrets.
- `state_dir` holds the ledger: daily usage, sent orders, halts. Usage and halts survive restarts.
- Every `[markets."<market_id>"]` field is required. A market without a block is not tradable.
  Amounts are decimal strings in the market's units; the quote caps are in quote, the qty caps in
  base. Daily usage counts **submissions**, not fills: an order's qty and quote count against the
  UTC day once it is sent, even if it is later cancelled.

The config is read at startup only. To change a limit, edit the file and restart the process.

## Add it to Claude Code

```bash
claude mcp add aspens -- aspens-mcp --config ~/.aspens-mcp/aspens-mcp.toml
```

Any MCP client that can spawn a stdio server works the same way: run `aspens-mcp --config <path>`
(`serve` is the default subcommand).

## Tools

Fifteen tools by default; `withdraw` is the sixteenth, registered only with
`allow_withdraw = true`. Amounts are decimal strings, each with a `…Raw` integer beside it.

Read:

- `venue_config`: the venue's markets (legs, decimals, pair decimals, tradable here or not),
  chains and tokens.
- `top_of_book`: best bid and best ask of a market.
- `orderbook`: visible depth of a market, aggregated by price.
- `balances`: per token and chain, the wallet, available and locked amounts, plus native gas.
- `my_orders`: the orders this server sent and their state; filter `open`, `closed` or `all`.
- `get_order`: one of those orders by id, with its fills.
- `my_fills`: your fills on a market, maker and taker, newest last.
- `account_status`: your addresses per curve, which keys are loaded, venue reachability, halt
  state and reason, whether the venue is pinned.
- `limits`: the configured limits and today's usage and remaining budget per market.
- `venue_attestation`: the venue's attestation report for a fresh nonce.

Write (each checks every limit before anything is signed; a refusal names each rule that blocked
it):

- `place_limit_order`: a limit order; optional `post_only` and allowlisted settle addresses.
- `place_marketable_order`: takes liquidity with a limit at the best opposite price moved by
  `slippage_bps` (default 50); the limit price goes through the price guard.
- `cancel_order`: cancels by id; an order that is not live is reported as `already_gone`.
  Allowed while halted.
- `deposit`: moves a token from your wallet into the venue.
- `withdraw`: moves a token from the venue to your own address only. Absent unless
  `allow_withdraw = true`.
- `halt`: stops new orders, deposits and withdrawals, with an optional reason.

There are no hidden orders, no named-id (dealroom) fills and no raw market orders: those
parameters are absent from the schemas.

An order in state `unknown` got no answer from the venue. The server never re-sends it; check it
later with `my_orders` or `get_order`.

## Halt and resume

`halt` is a persistent kill switch for the agent. It is written to the ledger, so it survives
restarts, and the agent cannot lift it. Cancels stay allowed while halted, since a cancel only
reduces exposure. To lift it, stop the server and run:

```bash
aspens-mcp resume --config ~/.aspens-mcp/aspens-mcp.toml
```

`resume` refuses while a server holds the same `state_dir`.

## Behaviour to know about

- **Setting `[venue] attestation_mrtd` makes every order, deposit and withdraw refuse today.** The
  SDK's attestation verifier needs DCAP collateral and the manifest and image digests, so a pinned
  MRTD alone cannot verify a report. This is deliberate fail-closed behaviour. Pin with
  `expected_contracts` alone until the verifier can be fed the rest.
- **With `[venue]` set, a network missing from `expected_contracts` is refused.** Every network an
  order, deposit or withdraw touches (both legs of a market) needs an entry.
- **A venue restart that changes a market's pair decimals refuses orders on that market until
  aspens-mcp restarts.** The limits were resolved in the old units; restarting re-resolves them.
- **If the process exits mid-send, usage is still counted, but the order may not appear in
  `my_orders`.** Usage is fsynced before the order is signed; the outcome record comes after the
  venue answers. Check the venue directly (for example `aspens-cli`) for an order sent just before
  a crash.
- **A second aspens-mcp on the same `state_dir` refuses to start.** The ledger has a single
  writer.
- **Amounts with more decimal places than the market or token allows are rejected, not rounded.**

## Testing

```bash
cargo test -p aspens-mcp --features test-fake
```

drives the whole MCP surface in-process against a fake venue.

`tests/rig_e2e.rs` is the end-to-end suite: it spawns the built binary over stdio against the
local rig and runs eight scenarios (reads, deposit, rest and cancel, a crossing fill and a
marketable fill, the refusals, halt and resume across restarts, usage across a restart, and
withdraw). Every test is `#[ignore]` and needs `RIG=1`. To run it:

1. Bring up the evm-evm-local rig from `infra/`, as `otc/e2e/README.md` describes:

   ```bash
   TIGERBEETLE=1 just local-up
   ANVIL_1_FORK_URL=https://rpc.ankr.com/flare_coston2 \
   ANVIL_2_FORK_URL=https://rpc.ankr.com/flare_coston2 \
     just test-up scenarios/evm-evm-local.toml
   ```

2. Fund two anvil dev accounts, the trader and the counterparty, the same way: fXRP by
   impersonating `0x…dEaD` on :8545 and transferring; USDT0 with
   `just fund flare-coston2-quote USDT0 <addr> 1000 http://localhost:8546`. Deposit 20+ fXRP and
   500 USDT0 for each with `aspens-cli deposit`, then `cast rpc anvil_mine 0x5` on both forks.
   The trader also needs USDT0 left in its wallet for the deposit scenario.

3. Run the suite, in order, one test at a time:

   ```bash
   RIG=1 \
   ASPENS_MCP_RIG_MARKET='<market id>' \
   ASPENS_MCP_RIG_TRADER_ENV=$PWD/.env.local.1 \
   ASPENS_MCP_RIG_COUNTER_ENV=$PWD/.env.local.2 \
     cargo test -p aspens-mcp --test rig_e2e -- --ignored --test-threads=1
   ```

   The env files are the trader's and the counterparty's (`TRADER_PRIVKEY` plus
   `ASPENS_RPC_URL_<NETWORK>`); the suite copies the trader's into a 0600 temp file per run and
   never writes to either. The optional variables (`ASPENS_MCP_RIG_STACK`, `_QTY`, `_PRICE`,
   `_TRANSFER`) are documented in the file's header.
