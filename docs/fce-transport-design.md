# FCE transport — wire-format design (feature `fce`)

Status: design. Pins the FCE direct-action wire format so the SDK can drive
actions through the Flare Confidential Extension proxy instead of dialing
arborter gRPC directly. All byte formats below are pinned against the Go
sources they must interoperate with; the exact citations are in
[§7 Conformance](#7-conformance).

## 0. Scope & the feature flag

Two transports, selected at build time by a Cargo feature so a non-FCE build
pulls in no HTTP-proxy code, no extra deps, and no risk of a client accidentally
talking to a proxy:

- **default (no feature):** the existing `tonic` gRPC transport to arborter.
- **`fce` feature:** adds the `fce` module (HTTP `POST /direct` + result poll)
  and an `Transport::Fce { proxy_url, api_key }` variant.

```toml
# aspens/Cargo.toml
[features]
fce = ["dep:reqwest"]        # HTTP client for the proxy; keep gRPC default-on
```

```rust
pub enum Transport {
    Grpc(GrpcChannel),                       // always available
    #[cfg(feature = "fce")]
    Fce { proxy_url: Url, api_key: String }, // /direct + poll
}
```

The TypeScript `@aspens/terminal-sdk` mirrors this with a build/runtime flag
(`ASPENS_FCE` / a `transport: "fce"` client option) — the frontend is the other
place that produces the signed envelope the adapter forwards.

**Unchanged by this doc:** order signing (`aspens::orders::derive_order_id`, the
EIP-712 `signatureHash`), account addresses, and the withdraw signature scheme.
arborter authenticates identically regardless of transport — the FCE path only
*wraps and transports* the same envelope. FCE trading carries **no JWT** (auth is
the per-request signature).

## 1. `ToHash` / `to_bytes32` — NOT a hash

The adapter's `teeutils.ToHash(s)` and the `send-direct` client's `toBytes32(s)`
are `bytes32(s)` in the Solidity sense: copy the UTF-8 bytes of `s` into a
32-byte array, truncating past 32 bytes and zero-padding the tail. No keccak, no
sha.

```rust
/// bytes32(s): UTF-8 bytes right-padded (zero tail) into 32 bytes; truncate >32.
pub fn to_bytes32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    let n = b.len().min(32);
    out[..n].copy_from_slice(&b[..n]);
    out
}
```

### Golden vectors (assert exactly)

| string           | `to_bytes32` (hex)                                                    |
|------------------|----------------------------------------------------------------------|
| `ASPENS`         | `0x415350454e530000000000000000000000000000000000000000000000000000` |
| `DEPOSIT`        | `0x4445504f53495400000000000000000000000000000000000000000000000000` |
| `WITHDRAW`       | `0x5749544844524157000000000000000000000000000000000000000000000000` |
| `PLACE_ORDER`    | `0x504c4143455f4f52444552000000000000000000000000000000000000000000` |
| `CANCEL_ORDER`   | `0x43414e43454c5f4f524445520000000000000000000000000000000000000000` |
| `GET_MY_STATE`   | `0x4745545f4d595f53544154450000000000000000000000000000000000000000` |
| `GET_BOOK_STATE` | `0x4745545f424f4f4b5f5354415445000000000000000000000000000000000000` |
| `EXPORT_HISTORY` | `0x4558504f52545f484953544f5259000000000000000000000000000000000000` |

`ASPENS` is the only OPType. The six direct OPCommands are WITHDRAW, PLACE_ORDER,
CANCEL_ORDER, GET_MY_STATE, GET_BOOK_STATE, EXPORT_HISTORY. DEPOSIT is the
on-chain instruction channel (§6), not a direct action.

## 2. `DirectInstruction` — the on-wire object

The adapter parses `action.Data.Message` as a JSON `DirectInstruction`
(`tee-node/pkg/types` `direct.go`):

```go
type DirectInstruction struct {
    OPType    common.Hash   `json:"opType"`     // 0x + 64 hex (32 bytes)
    OPCommand common.Hash   `json:"opCommand"`  // 0x + 64 hex
    Message   hexutil.Bytes `json:"message"`    // 0x + hex(bytes)
}
```

So the SDK emits **JSON**:

```json
{
  "opType":    "0x415350454e53...00",        // to_bytes32("ASPENS")
  "opCommand": "0x504c4143455f4f52444552...", // to_bytes32("PLACE_ORDER")
  "message":   "0x7b226d61726b65744964..."   // hex( utf8( payload-JSON ) )
}
```

- `opType` / `opCommand`: go-ethereum `common.Hash` → **`0x` + exactly 64 lowercase
  hex chars** (the 32 bytes from §1).
- `message`: go-ethereum `hexutil.Bytes` → **`0x` + hex of the raw bytes**. The
  raw bytes are the **UTF-8 of the payload JSON string** (§3). Example: payload
  `{"marketId":"m1"}` → `message = 0x7b226d61726b65744964223a226d31227d`.

`common.Hash` / `hexutil.Bytes` are always `0x`-prefixed and lowercase; the SDK
encoder must match (empty bytes → `"0x"`).

## 3. Payload JSON (`message` decoded) — the six commands

`message` carries the JSON of the request struct the adapter `json.Unmarshal`s
(`extension/pkg/types/types.go`). Field names are **camelCase**; all amounts are
**u128 decimal strings** (never numbers, never u64). The SDK already computes
`signatureHash` + `orderId`; here it serializes them to this schema.

```jsonc
// PLACE_ORDER
{
  "side": "BID" | "ASK",
  "quantity": "5",                 // decimal string
  "price": "1000",                 // omit/null => MARKET order
  "marketId": "flare-coston2::0x..::flare-coston2-quote::0x..",
  "baseAccountAddress": "0x..",
  "quoteAccountAddress": "0x..",
  "executionType": "DIRECT",       // optional: DIRECT | DISCRETIONARY
  "postOnly": false,               // optional
  "signatureHash": "0x<eip712-sig>",
  "orderId": "0x<sdk-derived>",
  "amountIn": "5"
}

// CANCEL_ORDER   — { orderId, marketId, signatureHash, ... } (mirror CancelOrderRequest)
// WITHDRAW       — { "network":"flare-coston2", "token":"0x..", "account":"0x..",
//                    "amount":"1000", "signature":"0x<sig over network|token|account|amount>" }
//                    → result data is a MidribV3 WithdrawVoucher
// GET_MY_STATE   — { "marketId":"<mkt>", "trader":"0x<addr>" }
// GET_BOOK_STATE — { "marketId":"<mkt>", "depth":10 }
// EXPORT_HISTORY — { "marketId":"<mkt>", ... }
```

> The SDK must serialize the payload struct to JSON, then hex-encode those UTF-8
> bytes into `message` — a **double layer**: JSON-in-hex-in-JSON. Keep the two
> JSON serializers (payload vs DirectInstruction) distinct.

## 4. `POST /direct` — submit

Pinned against Aspens' `send-direct` client (`extension/tools/cmd/send-direct`):

```
POST {proxy}/direct
Content-Type: application/json
X-API-Key: {DIRECT_API_KEY}

<DirectInstruction JSON from §2>
```

- `{proxy}` is the ext-proxy **external** endpoint: host port `:6674` (container
  `:6664`) — for us, the public tunnel `EXT_PROXY_URL`.
- Response `200`: `{ "data": { "id": "0x<32-byte hex>" } }` — the action id.
- Non-200: body is the error; surface it.

## 5. Poll `/action/result/{id}` — result

```
GET {proxy}/action/result/{id}?submissionTag=submit
```

Poll until `200` (client polls 15× every 2s). Body is `ActionResponse` wrapping
an `ActionResult` (`tee-node/pkg/types`, built by the adapter's `buildResult`):

```jsonc
{
  "result": {
    "id": "0x..", "submissionTag": "submit", "version": "0.1.0",
    "opType": "0x..", "opCommand": "0x..",
    "status": 1,                    // uint8: 1 = ok, 0 = error
    "log": "ok",                    // "ok" | "error: <msg>"
    "data": <arborter response>     // present on status=1
  }
}
```

- `status == 0` → action failed; `log` has the reason. `status == 1` → ok.
- `data` is `json.Marshal(<arborter client response>)` from the adapter handler
  (PLACE_ORDER→PlaceOrder resp, GET_BOOK_STATE→drained Orderbook **snapshot**,
  GET_MY_STATE→Orderbook filtered by trader, EXPORT_HISTORY→Trades snapshot,
  WITHDRAW→WithdrawVoucher). The SDK decodes `data` into the matching native type.
- **Reads are one-shot snapshots, not streams.** The adapter drains the arborter
  stream into a point-in-time result, so live `Orderbook`/`Trades` subscriptions
  are NOT available over FCE — expose snapshot variants; keep live streaming on
  the gRPC transport only.

> Pin the exact JSON representation of `ActionResult.Data` (Go `[]byte` marshals
> as a base64 string; `json.RawMessage` stays inline) as a conformance item — the
> SDK's result decoder branches on it. Confirm against a live `send-direct` on the
> harness (§7).

## 6. Deposit (out of scope for the direct transport)

DEPOSIT is the **on-chain instruction** channel, not a direct action: a cosigned
`AspensInstructionSender.deposit(token, amount)` on Flare; arborter credits from
its own chain listener (the adapter only ACKs). So the SDK's FCE-custody deposit
is a *contract call*, and non-Flare chains (HyperEVM, Solana) keep their existing
on-chain deposit/withdraw paths — the FCE instruction relay is Flare-C-chain-only.
The credit loop is a Phase-2 seam (no `CreditDeposit` RPC yet).

## 7. Conformance

Pinned against (vendor a copy of these commits' bytes into the test fixtures):

- `tee-node` `v0.0.22`:
  - `pkg/utils/utils.go` → `ToHash` (§1).
  - `pkg/types/direct.go` → `DirectInstruction` (§2).
  - `pkg/types/actions.go` → `Action`, `ActionResult`, `ActionResponse` (§5).
- `go-flare-common` `v1.2.2-0.20260623111601-c573c79c0924`:
  - `pkg/tee/instruction` → `DataFixed` (instruction channel).
- Aspens `extension/tools/cmd/send-direct/main.go` — the reference POST+poll
  client (§4/§5).
- Aspens `extension/pkg/types/types.go` — the payload structs (§3).

Tests the SDK must ship under `#[cfg(feature = "fce")]`:

1. **`to_bytes32` golden vectors** — assert every row in §1.
2. **`DirectInstruction` encoder** — a fixed `(command, payload)` → exact JSON
   bytes, compared to a golden captured from `send-direct` (which JSON-marshals
   the same struct). Include `message` = `0x` + hex(payload-JSON).
3. **Round-trip on the local harness** — with the harness proxy + a stub arborter,
   `PLACE_ORDER` returns `status=1` and the decoded `data`; an unknown command
   returns `status=0 log="unsupported direct command…"`; a wrong OPType returns
   the proxy's `501`. (These are the exact negative checks the harness doc lists.)
4. **Result `data` decoding** — decode a captured `ActionResult.data` into the
   native response type; pin the base64-vs-raw representation.

## 8. Module layout (Rust)

```
aspens/src/fce/
  mod.rs         // Transport::Fce plumbing + client entry (cfg(feature="fce"))
  wire.rs        // to_bytes32, OPType/OPCommand consts, DirectInstruction (serde)
  payloads.rs    // the six request structs (serde, camelCase, decimal-string amounts)
  proxy.rs       // POST /direct + poll /action/result (reqwest)
  result.rs      // ActionResult/ActionResponse decode + Data → native types
```

The `ExchangeClient` gains a transport selector; `place_order` / `cancel_order` /
`withdraw` / `my_state` / `book_state` / `export_history` route to gRPC or, under
`fce` + `Transport::Fce`, to `fce::proxy`. `get_config` stays on gRPC (no
`GET_CONFIG` OPCommand) — the FCE client is hybrid.
