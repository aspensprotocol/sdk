//! Solana on-chain program (Midrib) client-side helpers.
//!
//! Mirrors `arborter/app/chain-solana` — keep PDA seeds, account orderings,
//! and Anchor discriminators in sync with the on-chain `midrib` program.
//! Anchor instruction data layout: `sha256("global:<method>")[..8] || borsh(args)`.

use borsh::BorshSerialize;
use eyre::{Result, eyre};
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
    /// Seed for the singleton factory PDA.
    pub const FACTORY_SEED: &[u8] = b"factory";
    /// Seed for per-market `instance` PDAs (one per trading pair).
    pub const INSTANCE_SEED: &[u8] = b"instance";
    /// Seed for per-(instance, user) balance PDAs.
    pub const BALANCE_SEED: &[u8] = b"balance";
    /// Seed for the per-instance SPL token vault authority / account.
    pub const INSTANCE_VAULT_SEED: &[u8] = b"instance_vault";
    /// Seed for the single-use withdrawal-voucher tombstone (Track A §8),
    /// distinct from `USED_NONCE_SEED` so order and withdrawal nonces never
    /// collide.
    pub const WITHDRAW_NONCE_SEED: &[u8] = b"withdraw_nonce";
    /// Seed for the per-(instance, mint) FeeAccrual PDA — running total of
    /// settle-time fees awaiting `sweep_fees`.
    pub const FEE_ACCRUAL_SEED: &[u8] = b"fee_accrual";
}

/// Sysvar Rent — `"SysvarRent111111111111111111111111111111111"`.
pub fn sysvar_rent_id() -> Pubkey {
    Pubkey::from_str("SysvarRent111111111111111111111111111111111")
        .expect("SysvarRent id is a well-known constant; parse must succeed")
}

/// Sysvar Instructions — `"Sysvar1nstructions1111111111111111111111111"`.
/// Required as an account for any Midrib instruction that reads the
/// transaction's instruction list (e.g. `openFor`, which verifies that an
/// Ed25519Program instruction precedes it).
pub fn sysvar_instructions_id() -> Pubkey {
    Pubkey::from_str("Sysvar1nstructions1111111111111111111111111")
        .expect("Sysvar Instructions id is a well-known constant; parse must succeed")
}

/// SPL Associated Token Account program ID —
/// `"ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"`.
pub fn ata_program_id() -> Pubkey {
    Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
        .expect("ATA program id is a well-known constant; parse must succeed")
}

/// Ed25519 signature-verification precompile program id —
/// `"Ed25519SigVerify111111111111111111111111111"`.
pub fn ed25519_program_id() -> Pubkey {
    Pubkey::from_str("Ed25519SigVerify111111111111111111111111111")
        .expect("Ed25519 precompile id is a well-known constant; parse must succeed")
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

/// Derive the per-(instance, mint) `FeeAccrual` PDA — the running total of
/// settle-time protocol fees awaiting `sweep_fees`. Seeds:
/// `[FEE_ACCRUAL_SEED, instance, mint]`.
pub fn derive_fee_accrual_pda(
    instance: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[seeds::FEE_ACCRUAL_SEED, instance.as_ref(), mint.as_ref()],
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

/// Build an idempotent "create associated token account" instruction (ATA
/// program `CreateIdempotent`, discriminant `1`). A no-op if `ata` already
/// exists, so it is safe to submit unconditionally — prepend it before a
/// `withdraw_voucher` whose SPL transfer credits `ata`, which the program does
/// NOT `init` (SOL-VOUCHER-ATA: a withdrawer's recipient ATA may not exist yet,
/// e.g. the received leg of a cross-chain trade in a token they never held on
/// this chain). `payer` funds the rent (~0.002 SOL when actually created) +
/// signs the tx.
pub fn create_idempotent_ata_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    ata: &Pubkey,
) -> Instruction {
    // Account order is fixed by the ATA program. (Recent ATA-program versions
    // no longer require the rent sysvar, so it is omitted.)
    Instruction {
        program_id: ata_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![1], // CreateIdempotent
    }
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

// -- Withdrawal voucher (Track A §8) --------------------------------------

/// Derive the single-use withdrawal-voucher tombstone PDA
/// (`[WITHDRAW_NONCE_SEED, instance, account, nonce]`). Mirrors the program's
/// `withdraw_voucher` account seeds.
pub fn derive_withdraw_nonce_pda(
    instance: &Pubkey,
    account: &Pubkey,
    nonce: u64,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            seeds::WITHDRAW_NONCE_SEED,
            instance.as_ref(),
            account.as_ref(),
            &nonce.to_le_bytes(),
        ],
        program_id,
    )
}

/// The exact bytes the instance `signer` (TEE) Ed25519-signs to authorize a
/// `withdraw_voucher`. Borsh layout MUST match the program's + adapter's
/// `WithdrawalVoucherPayload` byte-for-byte.
#[derive(borsh::BorshSerialize, Debug)]
pub struct WithdrawalVoucherPayload {
    pub instance: Pubkey,
    pub account: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub nonce: u64,
    pub deadline: u64,
}

/// Args to the Midrib `withdraw_voucher` instruction.
#[derive(borsh::BorshSerialize, Debug)]
pub struct WithdrawVoucherArgs {
    pub amount: u64,
    pub nonce: u64,
    pub deadline: u64,
    /// The TEE's 64-byte Ed25519 signature (informational on-chain; the
    /// verified copy lives in the paired Ed25519Program ix).
    pub signature: [u8; 64],
}

/// Produce the exact bytes the arborter signed for a withdrawal voucher — what
/// the SDK must put in the `ed25519_verify_ix` message region.
pub fn withdrawal_voucher_signing_message(
    instance: &Pubkey,
    account: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    nonce: u64,
    deadline: u64,
) -> Result<Vec<u8>> {
    let payload = WithdrawalVoucherPayload {
        instance: *instance,
        account: *account,
        mint: *mint,
        amount,
        nonce,
        deadline,
    };
    borsh::to_vec(&payload).map_err(|e| eyre!("borsh encode WithdrawalVoucherPayload: {}", e))
}

/// Build the `withdraw_voucher` instruction. Pair it (in the same tx, AFTER the
/// matching [`ed25519_verify_ix`]) — the program introspects the preceding ix.
/// `payer` is the fee payer + sole tx signer; `account` is the withdrawer (funds
/// go to `user_token_account`), which does NOT sign.
#[allow(clippy::too_many_arguments)]
pub fn withdraw_voucher_ix(
    program_id: &Pubkey,
    instance: &Pubkey,
    account: &Pubkey,
    mint: &Pubkey,
    user_token_account: &Pubkey,
    payer: &Pubkey,
    args: &WithdrawVoucherArgs,
) -> Result<Instruction> {
    let (user_balance, _) = derive_user_balance_pda(instance, account, mint, program_id);
    let (instance_vault, _) = derive_instance_vault(instance, mint, program_id);
    let (vault_authority, _) = derive_vault_authority(instance, program_id);
    let (used_nonce, _) = derive_withdraw_nonce_pda(instance, account, args.nonce, program_id);
    let data = encode_ix("withdraw_voucher", args)?;
    // Account order MUST match the program's `WithdrawVoucher` accounts struct.
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*instance, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(user_balance, false),
            AccountMeta::new(*user_token_account, false),
            AccountMeta::new(instance_vault, false),
            AccountMeta::new_readonly(vault_authority, false),
            AccountMeta::new(used_nonce, false),
            AccountMeta::new_readonly(*account, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(sysvar_instructions_id(), false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data,
    })
}

/// Build an Ed25519Program instruction that verifies `signature` was
/// produced by `pubkey` over `message`. Data layout matches the Solana
/// Ed25519SigVerify precompile's expectation: a 16-byte header followed by
/// `signature(64) || pubkey(32) || message`.
///
/// Pair this with the Midrib `withdraw_voucher` instruction in the same
/// transaction — the program reads the sysvar instructions list and verifies
/// the preceding Ed25519Program ix matches the TEE-signed voucher.
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
    fn ed25519_verify_ix_has_no_accounts_and_targets_precompile() {
        let ix = ed25519_verify_ix(&[0; 32], &[0; 64], b"hi");
        assert!(ix.accounts.is_empty());
        assert_eq!(ix.program_id, ed25519_program_id());
        // header(16) + sig(64) + pk(32) + message(2)
        assert_eq!(ix.data.len(), 16 + 64 + 32 + 2);
    }

    #[test]
    fn create_idempotent_ata_ix_layout() {
        let payer = Pubkey::new_from_array([1; 32]);
        let owner = Pubkey::new_from_array([2; 32]);
        let mint = Pubkey::new_from_array([3; 32]);
        let ata = Pubkey::new_from_array([4; 32]);
        let ix = create_idempotent_ata_ix(&payer, &owner, &mint, &ata);

        assert_eq!(ix.program_id, ata_program_id());
        assert_eq!(ix.data, vec![1], "CreateIdempotent discriminant");
        // Account order is fixed by the ATA program; a wrong order fails silently
        // on-chain, so pin it.
        let a = &ix.accounts;
        assert_eq!(a.len(), 6);
        assert_eq!(a[0].pubkey, payer);
        assert!(a[0].is_signer && a[0].is_writable, "payer signs + funds rent");
        assert_eq!(a[1].pubkey, ata);
        assert!(a[1].is_writable && !a[1].is_signer, "ata is created (writable)");
        assert_eq!(a[2].pubkey, owner);
        assert_eq!(a[3].pubkey, mint);
        assert_eq!(a[4].pubkey, SYSTEM_PROGRAM_ID);
        assert_eq!(a[5].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(
            a[2..].iter().all(|m| !m.is_signer && !m.is_writable),
            "owner/mint/programs are readonly"
        );
    }

    #[test]
    fn pdas_are_stable() {
        let program_id = Pubkey::new_from_array([9; 32]);
        let (factory_a, _) = derive_factory_pda(&program_id);
        let (factory_b, _) = derive_factory_pda(&program_id);
        assert_eq!(factory_a, factory_b);
        let (inst, _) = derive_instance_pda(&factory_a, 1, &program_id);
        let user = Pubkey::new_from_array([7; 32]);
        let mint = Pubkey::new_from_array([8; 32]);
        let (bal, _) = derive_user_balance_pda(&inst, &user, &mint, &program_id);
        assert_ne!(bal, inst);
    }
}
