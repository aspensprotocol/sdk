//! Solana on-chain program (Midrib) client-side helpers.
//!
//! Mirrors `arborter/app/chain-solana` — keep PDA seeds, account orderings,
//! and Anchor discriminators in sync with the on-chain `midrib` program.
//! Anchor instruction data layout: `sha256("global:<method>")[..8] || borsh(args)`.

use borsh::BorshSerialize;
use eyre::{eyre, Result};
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use std::str::FromStr;

#[cfg(feature = "client")]
pub mod client;

/// System program ID — "11111111111111111111111111111111" (all-zero pubkey).
pub const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::new_from_array([0u8; 32]);
/// SPL Token program ID — "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
]);

/// PDA seeds — must match the on-chain `midrib` program.
pub mod seeds {
    pub const FACTORY_SEED: &[u8] = b"factory";
    pub const INSTANCE_SEED: &[u8] = b"instance";
    pub const BALANCE_SEED: &[u8] = b"balance";
    pub const ORDER_SEED: &[u8] = b"order";
    pub const INSTANCE_VAULT_SEED: &[u8] = b"instance_vault";
}

/// Sysvar Rent — `"SysvarRent111111111111111111111111111111111"`.
pub fn sysvar_rent_id() -> Pubkey {
    Pubkey::from_str("SysvarRent111111111111111111111111111111111").unwrap()
}

/// Sysvar Instructions — `"Sysvar1nstructions1111111111111111111111111"`.
/// Required as an account for any Midrib instruction that reads the
/// transaction's instruction list (e.g. `openFor`, which verifies that an
/// Ed25519Program instruction precedes it).
pub fn sysvar_instructions_id() -> Pubkey {
    Pubkey::from_str("Sysvar1nstructions1111111111111111111111111").unwrap()
}

/// SPL Associated Token Account program ID —
/// `"ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"`.
pub fn ata_program_id() -> Pubkey {
    Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap()
}

/// Ed25519 signature-verification precompile program id —
/// `"Ed25519SigVerify111111111111111111111111111"`.
pub fn ed25519_program_id() -> Pubkey {
    Pubkey::from_str("Ed25519SigVerify111111111111111111111111111").unwrap()
}

/// Compute Anchor's 8-byte instruction discriminator for `<method>`.
fn anchor_ix_discriminator(method: &str) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(format!("global:{method}").as_bytes());
    let digest = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

fn encode_ix<A: BorshSerialize>(method: &str, args: &A) -> Result<Vec<u8>> {
    let disc = anchor_ix_discriminator(method);
    let body = borsh::to_vec(args).map_err(|e| eyre!("borsh encode {}: {}", method, e))?;
    let mut data = Vec::with_capacity(8 + body.len());
    data.extend_from_slice(&disc);
    data.extend_from_slice(&body);
    Ok(data)
}

/// Derive the factory PDA — singleton per program.
pub fn derive_factory_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[seeds::FACTORY_SEED], program_id)
}

/// Derive the trading-instance PDA for `(factory, instance_id)`.
pub fn derive_instance_pda(
    factory: &Pubkey,
    instance_id: u64,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            seeds::INSTANCE_SEED,
            factory.as_ref(),
            &instance_id.to_le_bytes(),
        ],
        program_id,
    )
}

/// Derive the `Order` PDA for `(instance, order_id)`.
pub fn derive_order_pda(
    instance: &Pubkey,
    order_id: &[u8; 32],
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[seeds::ORDER_SEED, instance.as_ref(), order_id.as_ref()],
        program_id,
    )
}

/// Derive the user-balance PDA for `(instance, user, mint)`.
pub fn derive_user_balance_pda(
    instance: &Pubkey,
    user: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            seeds::BALANCE_SEED,
            instance.as_ref(),
            user.as_ref(),
            mint.as_ref(),
        ],
        program_id,
    )
}

/// Derive the per-(instance, mint) SPL vault PDA.
pub fn derive_instance_vault(
    instance: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[seeds::INSTANCE_VAULT_SEED, instance.as_ref(), mint.as_ref()],
        program_id,
    )
}

/// Derive the vault authority PDA for an instance.
pub fn derive_vault_authority(instance: &Pubkey, program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[seeds::INSTANCE_VAULT_SEED, instance.as_ref()], program_id)
}

/// Derive the SPL Associated Token Account address for `(owner, mint)`.
pub fn derive_associated_token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let seeds = &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()];
    let (ata, _bump) = Pubkey::find_program_address(seeds, &ata_program_id());
    ata
}

#[derive(BorshSerialize)]
struct AmountArgs {
    amount: u64,
}

/// Build the `deposit` instruction. User-signed — the user's Ed25519 key must
/// sign the resulting transaction. Initializes UserBalance / instance_vault
/// PDAs on first call (init_if_needed on-chain).
pub fn deposit_ix(
    program_id: &Pubkey,
    instance: &Pubkey,
    user: &Pubkey,
    mint: &Pubkey,
    user_token_account: &Pubkey,
    amount: u64,
) -> Result<Instruction> {
    let (user_balance, _) = derive_user_balance_pda(instance, user, mint, program_id);
    let (instance_vault, _) = derive_instance_vault(instance, mint, program_id);
    let (vault_authority, _) = derive_vault_authority(instance, program_id);
    let data = encode_ix("deposit", &AmountArgs { amount })?;
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*instance, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(user_balance, false),
            AccountMeta::new(*user_token_account, false),
            AccountMeta::new(instance_vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new(*user, true),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(sysvar_rent_id(), false),
        ],
        data,
    })
}

/// Build the `withdraw` instruction. User-signed.
pub fn withdraw_ix(
    program_id: &Pubkey,
    instance: &Pubkey,
    user: &Pubkey,
    mint: &Pubkey,
    user_token_account: &Pubkey,
    amount: u64,
) -> Result<Instruction> {
    let (user_balance, _) = derive_user_balance_pda(instance, user, mint, program_id);
    let (instance_vault, _) = derive_instance_vault(instance, mint, program_id);
    let (vault_authority, _) = derive_vault_authority(instance, program_id);
    let data = encode_ix("withdraw", &AmountArgs { amount })?;
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*instance, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(user_balance, false),
            AccountMeta::new(*user_token_account, false),
            AccountMeta::new(instance_vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new_readonly(*user, true),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data,
    })
}

// -- Gasless `open` / `open_for` client helpers ---------------------------
//
// The `open_for` Midrib instruction is the Solana counterpart to EVM's
// `lock_for_order_gasless`: the arborter pays the fee, but the user must
// have signed the canonical `OpenForSignedPayload` bytes with their
// Ed25519 key. The Ed25519SigVerify precompile then verifies the signature
// on-chain before `open_for` accepts the instruction.

/// Arguments to the Midrib `open` and `open_for` instructions — user-level
/// order intent.
#[derive(borsh::BorshSerialize, Clone, Debug)]
pub struct OpenOrderArgs {
    pub order_id: [u8; 32],
    pub origin_chain_id: u64,
    pub destination_chain_id: u64,
    pub input_token: Pubkey,
    pub input_amount: u64,
    pub output_token: [u8; 32],
    pub output_amount: u64,
}

/// The exact payload the user must sign for a gasless `open_for`. Structure
/// must match the arborter verbatim — it re-serializes this and feeds it to
/// `ed25519_verify_ix` alongside the user's signature.
#[derive(borsh::BorshSerialize, Debug)]
pub struct OpenForSignedPayload {
    pub instance: Pubkey,
    pub user: Pubkey,
    pub deadline: u64,
    pub order: OpenOrderArgs,
}

/// Arborter-facing `open_for` args: the signed payload fields plus the
/// user's 64-byte Ed25519 signature.
#[derive(borsh::BorshSerialize, Debug)]
pub struct OpenForArgs {
    pub order: OpenOrderArgs,
    pub user: Pubkey,
    pub deadline: u64,
    pub signature: [u8; 64],
}

/// Produce the exact bytes a user's Ed25519 key must sign to authorize a
/// gasless lock on Solana. The arborter will reconstruct the same payload
/// and check the signature via the Ed25519SigVerify precompile.
pub fn gasless_lock_signing_message(
    instance: &Pubkey,
    user: &Pubkey,
    deadline: u64,
    order: &OpenOrderArgs,
) -> Result<Vec<u8>> {
    let payload = OpenForSignedPayload {
        instance: *instance,
        user: *user,
        deadline,
        order: order.clone(),
    };
    borsh::to_vec(&payload).map_err(|e| eyre!("borsh encode OpenForSignedPayload: {}", e))
}

/// Build an Ed25519Program instruction that verifies `signature` was
/// produced by `pubkey` over `message`. Data layout matches the Solana
/// Ed25519SigVerify precompile's expectation: a 16-byte header followed by
/// `signature(64) || pubkey(32) || message`.
///
/// Pair this with the paired Midrib `open_for` instruction in the same
/// transaction — the program reads the sysvar instructions list and verifies
/// the preceding Ed25519Program ix matches.
pub fn ed25519_verify_ix(pubkey: &[u8; 32], signature: &[u8; 64], message: &[u8]) -> Instruction {
    let signature_offset: u16 = 16;
    let public_key_offset: u16 = 16 + 64;
    let message_offset: u16 = 16 + 64 + 32;
    let message_size: u16 = message.len() as u16;

    let mut data = Vec::with_capacity(16 + 64 + 32 + message.len());
    data.push(1); // num_signatures
    data.push(0); // padding
    data.extend_from_slice(&signature_offset.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes()); // signature_ix_index (same ix)
    data.extend_from_slice(&public_key_offset.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(&message_offset.to_le_bytes());
    data.extend_from_slice(&message_size.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(signature);
    data.extend_from_slice(pubkey);
    data.extend_from_slice(message);

    Instruction {
        program_id: ed25519_program_id(),
        accounts: vec![],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_is_deterministic() {
        let a = anchor_ix_discriminator("deposit");
        let b = anchor_ix_discriminator("deposit");
        assert_eq!(a, b);
        // sha256("global:deposit")[..8]
        let mut h = Sha256::new();
        h.update(b"global:deposit");
        assert_eq!(&a[..], &h.finalize()[..8]);
    }

    #[test]
    fn deposit_and_withdraw_have_signer_at_user_slot() {
        let pid = Pubkey::new_from_array([1; 32]);
        let inst = Pubkey::new_from_array([2; 32]);
        let user = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let ata = Pubkey::new_from_array([5; 32]);
        let dep = deposit_ix(&pid, &inst, &user, &mint, &ata, 100).unwrap();
        assert!(dep.accounts.iter().any(|a| a.is_signer && a.pubkey == user));
        let wd = withdraw_ix(&pid, &inst, &user, &mint, &ata, 100).unwrap();
        assert!(wd.accounts.iter().any(|a| a.is_signer && a.pubkey == user));
    }

    #[test]
    fn spl_token_program_id_is_canonical() {
        assert_eq!(
            SPL_TOKEN_PROGRAM_ID.to_string(),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
    }

    #[test]
    fn well_known_program_ids_parse() {
        // These are .unwrap()s in the helpers — pin them with an explicit
        // test so a typo fails here rather than at first runtime use.
        assert_eq!(
            sysvar_instructions_id().to_string(),
            "Sysvar1nstructions1111111111111111111111111"
        );
        assert_eq!(
            ed25519_program_id().to_string(),
            "Ed25519SigVerify111111111111111111111111111"
        );
        assert_eq!(
            ata_program_id().to_string(),
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        );
    }

    #[test]
    fn gasless_lock_signing_message_is_deterministic() {
        let instance = Pubkey::new_from_array([1; 32]);
        let user = Pubkey::new_from_array([2; 32]);
        let order = OpenOrderArgs {
            order_id: [3; 32],
            origin_chain_id: 501,
            destination_chain_id: 8453,
            input_token: Pubkey::new_from_array([4; 32]),
            input_amount: 100,
            output_token: [5; 32],
            output_amount: 200,
        };
        let a = gasless_lock_signing_message(&instance, &user, 1_000, &order).unwrap();
        let b = gasless_lock_signing_message(&instance, &user, 1_000, &order).unwrap();
        assert_eq!(a, b);
        // Borsh layout: 32+32+8 + (32+8+8+32+8+32+8) = 200 bytes
        assert_eq!(a.len(), 32 + 32 + 8 + 32 + 8 + 8 + 32 + 8 + 32 + 8);
    }

    #[test]
    fn ed25519_verify_ix_has_no_accounts_and_targets_precompile() {
        let ix = ed25519_verify_ix(&[0; 32], &[0; 64], b"hi");
        assert!(ix.accounts.is_empty());
        assert_eq!(ix.program_id, ed25519_program_id());
        // header(16) + sig(64) + pk(32) + message(2)
        assert_eq!(ix.data.len(), 16 + 64 + 32 + 2);
    }

    #[test]
    fn pdas_are_stable() {
        let program_id = Pubkey::new_from_array([9; 32]);
        let (factory_a, _) = derive_factory_pda(&program_id);
        let (factory_b, _) = derive_factory_pda(&program_id);
        assert_eq!(factory_a, factory_b);
        let (inst, _) = derive_instance_pda(&factory_a, 1, &program_id);
        let (order, _) = derive_order_pda(&inst, &[7; 32], &program_id);
        assert_ne!(order, inst);
    }
}
