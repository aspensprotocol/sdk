//! FCE action-envelope wire format (design: `sdk/docs/fce-transport-design.md`).
//!
//! Pinned against Flare's `tee-node@v0.0.22`:
//! - `pkg/utils/utils.go` — `ToHash` is `bytes32(s)`, NOT a hash.
//! - `pkg/types/direct.go` — `DirectInstruction` is JSON `{opType, opCommand, message}`.
//!
//! All three fields are go-ethereum types serialized as `0x`-prefixed lowercase
//! hex: `common.Hash` → `0x` + 64 hex; `hexutil.Bytes` → `0x` + hex (empty = `0x`).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The single OPType this extension answers to.
pub const OP_TYPE_ASPENS: &str = "ASPENS";

/// Direct-action OPCommands (off-chain). `DEPOSIT` is the on-chain instruction
/// channel, not a direct action — see the design doc §6.
pub const OP_WITHDRAW: &str = "WITHDRAW";
pub const OP_PLACE_ORDER: &str = "PLACE_ORDER";
pub const OP_CANCEL_ORDER: &str = "CANCEL_ORDER";
pub const OP_GET_MY_STATE: &str = "GET_MY_STATE";
pub const OP_GET_BOOK_STATE: &str = "GET_BOOK_STATE";
pub const OP_EXPORT_HISTORY: &str = "EXPORT_HISTORY";

/// `bytes32(s)` — copy the UTF-8 bytes of `s` into a 32-byte array, truncating
/// past 32 bytes and zero-padding the tail. Mirrors `teeutils.ToHash` /
/// `send-direct`'s `toBytes32` (Solidity `bytes32("ASPENS")`). NOT a hash.
pub fn to_bytes32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    let n = b.len().min(32);
    out[..n].copy_from_slice(&b[..n]);
    out
}

/// The on-wire object the ext-proxy `/direct` endpoint accepts (the proxy wraps
/// it into a tee-node `Action`; see `send-direct`). `message` carries the
/// UTF-8 bytes of the payload JSON (design §3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectInstruction {
    #[serde(rename = "opType", with = "hex32")]
    pub op_type: [u8; 32],
    #[serde(rename = "opCommand", with = "hex32")]
    pub op_command: [u8; 32],
    #[serde(rename = "message", with = "hexbytes")]
    pub message: Vec<u8>,
}

impl DirectInstruction {
    /// Build a direct instruction for `command` (an `OP_*` const) carrying the
    /// already-serialized payload JSON bytes as `message`.
    pub fn new(command: &str, payload_json: Vec<u8>) -> Self {
        Self {
            op_type: to_bytes32(OP_TYPE_ASPENS),
            op_command: to_bytes32(command),
            message: payload_json,
        }
    }
}

/// `0x`-prefixed hex for a fixed 32-byte value (go-ethereum `common.Hash`).
pub(crate) mod hex32 {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(v)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes =
            hex::decode(s.strip_prefix("0x").unwrap_or(&s)).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

/// `0x`-prefixed hex for a variable-length byte slice (go-ethereum
/// `hexutil.Bytes`). Empty bytes serialize as `"0x"`; `"0x"` deserializes to
/// empty (matches go-ethereum).
pub(crate) mod hexbytes {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(v)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        let body = s.strip_prefix("0x").unwrap_or(&s);
        if body.is_empty() {
            return Ok(Vec::new());
        }
        hex::decode(body).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32_str(s: &str) -> String {
        format!("0x{}", hex::encode(to_bytes32(s)))
    }

    // Golden vectors — MUST match `teeutils.ToHash` byte-for-byte (design §1).
    #[test]
    fn to_bytes32_golden_vectors() {
        assert_eq!(
            hex32_str("ASPENS"),
            "0x415350454e530000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("DEPOSIT"),
            "0x4445504f53495400000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("WITHDRAW"),
            "0x5749544844524157000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("PLACE_ORDER"),
            "0x504c4143455f4f52444552000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("CANCEL_ORDER"),
            "0x43414e43454c5f4f524445520000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("GET_MY_STATE"),
            "0x4745545f4d595f53544154450000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("GET_BOOK_STATE"),
            "0x4745545f424f4f4b5f5354415445000000000000000000000000000000000000"
        );
        assert_eq!(
            hex32_str("EXPORT_HISTORY"),
            "0x4558504f52545f484953544f5259000000000000000000000000000000000000"
        );
    }

    #[test]
    fn to_bytes32_truncates_past_32() {
        let s = "X".repeat(40);
        assert_eq!(to_bytes32(&s), [b'X'; 32]);
    }

    // The DirectInstruction JSON must be exactly {opType, opCommand, message}
    // with 0x-hex values; `message` = 0x + hex(payload JSON) (design §2).
    #[test]
    fn direct_instruction_json_shape() {
        let payload = br#"{"marketId":"m1"}"#.to_vec();
        let di = DirectInstruction::new(OP_GET_BOOK_STATE, payload);
        let v: serde_json::Value = serde_json::to_value(&di).unwrap();
        assert_eq!(
            v["opType"],
            "0x415350454e530000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            v["opCommand"],
            "0x4745545f424f4f4b5f5354415445000000000000000000000000000000000000"
        );
        // message = 0x + hex('{"marketId":"m1"}')
        assert_eq!(v["message"], "0x7b226d61726b65744964223a226d31227d");

        // round-trips
        let back: DirectInstruction = serde_json::from_value(v).unwrap();
        assert_eq!(back, di);
        assert_eq!(back.message, br#"{"marketId":"m1"}"#);
    }

    #[test]
    fn empty_hexbytes_is_0x() {
        let di = DirectInstruction::new(OP_PLACE_ORDER, Vec::new());
        let v: serde_json::Value = serde_json::to_value(&di).unwrap();
        assert_eq!(v["message"], "0x");
        let back: DirectInstruction = serde_json::from_value(v).unwrap();
        assert!(back.message.is_empty());
    }
}
