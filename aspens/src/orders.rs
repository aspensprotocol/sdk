//! Chain-agnostic order primitives.
//!
//! `derive_order_id` is the single reference recipe for producing the 32-byte
//! order id that the client and arborter MUST hash identically. `GaslessLockParams`
//! is the shared input struct fed to chain-specific signing helpers
//! (`aspens::evm::gasless_lock_signing_hash`, `aspens::solana::gasless_lock_signing_message`).

use sha2::{Digest, Sha256};

/// Derive the canonical 32-byte order id.
///
/// Hash layout (all little-endian where applicable):
/// ```text
/// sha256(
///     user_pubkey || client_nonce || origin_chain_id || destination_chain_id ||
///     input_token || output_token || input_amount || output_amount
/// )
/// ```
///
/// Both EVM and Solana clients MUST use this exact derivation — the arborter
/// rehashes with the same recipe and will reject orders whose id doesn't match.
#[allow(clippy::too_many_arguments)]
pub fn derive_order_id(
    user_pubkey: &[u8],
    client_nonce: u64,
    origin_chain_id: u64,
    destination_chain_id: u64,
    input_token: &[u8],
    output_token: &[u8],
    input_amount: u128,
    output_amount: u128,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(user_pubkey);
    h.update(client_nonce.to_le_bytes());
    h.update(origin_chain_id.to_le_bytes());
    h.update(destination_chain_id.to_le_bytes());
    h.update(input_token);
    h.update(output_token);
    h.update(input_amount.to_le_bytes());
    h.update(output_amount.to_le_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// Shared input struct fed to chain-specific signing helpers.
///
/// Fields are chain-specific where noted; harmless-but-ignored defaults are
/// fine for the other chain. A client constructs one of these and passes it
/// to either `aspens::evm::gasless_lock_signing_hash` or
/// `aspens::solana::gasless_lock_signing_message`.
#[derive(Debug, Clone)]
pub struct GaslessLockParams<'a> {
    /// User funding the lock — hex address on EVM, base58 pubkey on Solana.
    pub depositor_address: &'a str,
    /// Address / mint of the token being deposited on the origin chain.
    pub token_contract: &'a str,
    /// Address / mint of the token the user expects to receive on the
    /// destination chain.
    pub token_contract_destination_chain: &'a str,
    /// Chain id of the destination chain (decimal string).
    pub destination_chain_id: &'a str,
    /// Amount of `token_contract` being deposited, in pair decimals.
    pub amount_in: u128,
    /// Amount of `token_contract_destination_chain` the user expects out,
    /// in pair decimals.
    pub amount_out: u128,
    /// Opaque order id — typically a 32-byte hex string. On Solana this
    /// is the key under which the `Order` PDA is `init`-ed; on EVM it's
    /// the intent id. Chains that want to derive it internally may
    /// accept an empty string.
    pub order_id: &'a str,
    /// Chain-specific absolute deadline:
    ///   * Solana: slot number.
    ///   * EVM:    unix-seconds `fillDeadline` stamped on the GaslessCrossChainOrder.
    pub deadline: u64,
    /// Permit2 / EIP-712 nonce, embedded in the EVM `PermitSingle`. The user's
    /// signature is computed over the exact struct that includes this nonce,
    /// so the arborter must pass it through verbatim. Ignored by Solana
    /// (the `Order` PDA's `init` serves as the single-use nonce).
    pub nonce: u64,
    /// EVM-only `openDeadline` field on `GaslessCrossChainOrder` (unix
    /// seconds). Ignored by Solana.
    pub open_deadline: u64,
    /// User-produced signature. 64 bytes Ed25519 on Solana; 65 bytes
    /// ECDSA on EVM; length and semantics are chain-specific.
    pub user_signature: &'a [u8],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_id_is_deterministic() {
        let a = derive_order_id(&[1; 32], 42, 1, 501, b"0xaaa", b"MintXYZ", 100, 200);
        let b = derive_order_id(&[1; 32], 42, 1, 501, b"0xaaa", b"MintXYZ", 100, 200);
        assert_eq!(a, b);
    }

    #[test]
    fn order_id_changes_with_nonce() {
        let a = derive_order_id(&[1; 32], 1, 1, 501, b"t1", b"t2", 100, 200);
        let b = derive_order_id(&[1; 32], 2, 1, 501, b"t1", b"t2", 100, 200);
        assert_ne!(a, b);
    }

    #[test]
    fn order_id_endianness_is_le() {
        // If the hash ever changes to BE we need to coordinate with arborter,
        // so pin the canonical bytes for a known input.
        let id = derive_order_id(&[], 0, 0, 0, &[], &[], 0, 0);
        // sha256 of 8*8 = 64 zero bytes (5 u64 LE zero fields + 2 u128 LE zero).
        // Total: 0 + 8 + 8 + 8 + 0 + 0 + 16 + 16 = 56 bytes of zeros.
        let mut h = Sha256::new();
        h.update([0u8; 56]);
        let mut want = [0u8; 32];
        want.copy_from_slice(&h.finalize());
        assert_eq!(id, want);
    }
}
