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

/// The WSOL (wrapped native SOL) mint — native SOL's on-venue identity. A
/// deposit/withdraw against this mint is a native-SOL flow: clients wrap
/// (system-transfer + `SyncNative`) before depositing and unwrap
/// (`CloseAccount`) after withdrawing; the on-chain midrib program treats it
/// as an ordinary SPL mint throughout.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// `true` if `mint` is the WSOL mint (base58 is case-sensitive; exact match).
pub fn is_wsol_mint(mint: &str) -> bool {
    mint == WSOL_MINT
}

/// System-program `Transfer` (instruction discriminant 2): move `lamports`
/// from `from` to `to`. Hand-encoded (u32-LE discriminant || u64-LE lamports —
/// the System program's stable bincode wire format) so the lean signing build
/// needs no extra system-interface crate. Used to fund a WSOL ATA during a
/// native-SOL wrap.
pub fn system_transfer_ix(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

/// SPL Token `SyncNative` (instruction discriminant 17): syncs a WSOL token
/// account's recorded amount up to its lamport balance. Submit after a
/// system-transfer of lamports into the ATA to complete a wrap.
pub fn sync_native_ix(ata: &Pubkey) -> Instruction {
    Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![AccountMeta::new(*ata, false)],
        data: vec![17],
    }
}

/// SPL Token `CloseAccount` (instruction discriminant 9): closes `ata` and
/// sends its ENTIRE lamport balance — wrapped SOL plus rent — to `dest`.
/// This is the WSOL unwrap; note it unwraps the account's whole balance, not
/// just a withdrawn amount (standard wallet behavior).
pub fn close_token_account_ix(ata: &Pubkey, dest: &Pubkey, owner: &Pubkey) -> Instruction {
    Instruction {
        program_id: SPL_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*ata, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data: vec![9],
    }
}

/// PDA seeds — must match the on-chain `midrib` program.
pub mod seeds {
    /// Seed for per-(instance, user) balance PDAs.
    pub const BALANCE_SEED: &[u8] = b"balance";
    /// Seed for the per-instance SPL token vault authority / account.
    pub const INSTANCE_VAULT_SEED: &[u8] = b"instance_vault";
    /// Seed for the single-use withdrawal-voucher tombstone. Distinct from the
    /// program's `SETTLE_NONCE_SEED` so settlement and withdrawal nonce spaces
    /// never collide.
    pub const WITHDRAW_NONCE_SEED: &[u8] = b"withdraw_nonce";
    /// Seed for the per-(instance, mint) WithdrawEpoch PDA — the per-token
    /// per-epoch withdrawal cap plus the current epoch's running total.
    pub const WITHDRAW_EPOCH_SEED: &[u8] = b"withdraw_epoch";
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

/// Derive the per-(instance, mint) `WithdrawEpoch` PDA — the per-token
/// per-epoch withdrawal cap and the current epoch's running total. Seeds:
/// `[WITHDRAW_EPOCH_SEED, instance, mint]`.
///
/// `withdraw_voucher` takes this account `init_if_needed`, so it must be passed
/// WRITABLE even on the first withdrawal for a mint.
pub fn derive_withdraw_epoch_pda(
    instance: &Pubkey,
    mint: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[seeds::WITHDRAW_EPOCH_SEED, instance.as_ref(), mint.as_ref()],
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
    // Account order is fixed by the ATA program. (It does not require the
    // rent sysvar, so it is omitted.)
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

// -- Withdrawal voucher ---------------------------------------------------

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
    let (withdraw_epoch, _) = derive_withdraw_epoch_pda(instance, mint, program_id);
    let data = encode_ix("withdraw_voucher", args)?;
    // Account order MUST match the program's `WithdrawVoucher` accounts struct —
    // Anchor binds POSITIONALLY, so a missing or misordered entry is not a
    // "missing account" error, it silently reinterprets the account at that
    // index as something else. `withdraw_voucher_accounts_match_program` pins
    // this list; update both together.
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
            // `init_if_needed` on the program side → writable, not a signer.
            AccountMeta::new(withdraw_epoch, false),
            AccountMeta::new_readonly(*account, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(sysvar_instructions_id(), false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data,
    })
}

// -- Per-epoch withdrawal cap (operator-admin authority) -------------------

/// Args to the Midrib `set_withdraw_epoch_cap` instruction.
#[derive(borsh::BorshSerialize, Debug)]
pub struct SetWithdrawEpochCapArgs {
    /// Base-unit ceiling on withdrawals per `(instance, mint)` per epoch.
    /// `0` = unlimited (the shipped default), matching MidribV3 on EVM.
    pub cap: u64,
}

/// Build the `set_withdraw_epoch_cap` instruction — arm (or disarm) the
/// per-`(instance, mint)` per-epoch withdrawal ceiling.
///
/// `operator_admin` is the sole signer and the fee payer; it MUST equal the
/// on-chain `instance.operator_admin` or the program rejects the call with
/// `Unauthorized`. It is writable because the `withdraw_epoch` PDA is
/// `init_if_needed` with `payer = operator_admin` — arming a cap for a mint
/// that has never been withdrawn against creates the account and debits rent.
///
/// This is deliberately NOT signed by the instance's TEE `signer` key: the cap
/// exists to bound a misbehaving TEE, so a TEE able to raise its own cap would
/// defeat it. Sign this with an offline operator key, never through the
/// arborter.
///
/// `cap` is in the mint's BASE units (same scale the program accumulates
/// withdrawals in), and `0` means unlimited. The epoch is a tumbling window of
/// 9,000 slots (~1 hour), so up to `2 * cap` can leave across a boundary.
pub fn set_withdraw_epoch_cap_ix(
    program_id: &Pubkey,
    instance: &Pubkey,
    mint: &Pubkey,
    operator_admin: &Pubkey,
    cap: u64,
) -> Result<Instruction> {
    let (withdraw_epoch, _) = derive_withdraw_epoch_pda(instance, mint, program_id);
    let data = encode_ix("set_withdraw_epoch_cap", &SetWithdrawEpochCapArgs { cap })?;
    // Account order MUST match the program's `SetWithdrawEpochCap` accounts
    // struct — Anchor binds POSITIONALLY, so a misordered entry is not a
    // "wrong account" error, the program reinterprets whatever sits at that
    // index. `set_withdraw_epoch_cap_accounts_match_program` pins this list
    // against the built IDL; update both together.
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*instance, false),
            AccountMeta::new_readonly(*mint, false),
            // `init_if_needed` on the program side → writable, not a signer.
            AccountMeta::new(withdraw_epoch, false),
            // `mut` (rent payer) + `Signer` on the program side.
            AccountMeta::new(*operator_admin, true),
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
    fn deposit_has_signer_at_user_slot() {
        let pid = Pubkey::new_from_array([1; 32]);
        let inst = Pubkey::new_from_array([2; 32]);
        let user = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let ata = Pubkey::new_from_array([5; 32]);
        let dep = deposit_ix(&pid, &inst, &user, &mint, &ata, 100).unwrap();
        assert!(dep.accounts.iter().any(|a| a.is_signer && a.pubkey == user));
    }

    #[test]
    fn spl_token_program_id_is_canonical() {
        assert_eq!(
            SPL_TOKEN_PROGRAM_ID.to_string(),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
    }

    /// Pin `withdraw_voucher`'s account list — order, length, and the
    /// writable/signer flags — against the on-chain `WithdrawVoucher` accounts
    /// struct. Anchor binds accounts POSITIONALLY: a missing entry does not
    /// surface as "too few accounts", it shifts every later account up a slot
    /// and the program reinterprets whatever now sits at that index. That is
    /// exactly how `withdraw_epoch` (index 7) was omitted here while 144 other
    /// SDK tests stayed green — the withdrawer's pubkey landed in its slot and
    /// every voucher withdrawal failed on-chain with `ConstraintSeeds`.
    ///
    /// EXPECTED LIST SOURCE: `arborter/chains/solana/programs/midrib/src/
    /// instructions/withdraw_voucher.rs`, struct `WithdrawVoucher`, field order
    /// top to bottom. Reproduce it from the built IDL with:
    ///   cd arborter/chains/solana
    ///   anchor idl build -o /tmp/midrib.json -p midrib
    ///   jq -r '.instructions[] | select(.name=="withdraw_voucher")
    ///          | .accounts[] | .name' /tmp/midrib.json
    /// REGENERATE THIS TEST whenever that accounts struct changes — adding,
    /// removing, or reordering a field there is a breaking wire change here.
    #[test]
    fn withdraw_voucher_accounts_match_program() {
        let pid = Pubkey::new_from_array([1; 32]);
        let instance = Pubkey::new_from_array([2; 32]);
        let account = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let user_token_account = Pubkey::new_from_array([5; 32]);
        let payer = Pubkey::new_from_array([6; 32]);
        let args = WithdrawVoucherArgs {
            amount: 1,
            nonce: 7,
            deadline: 99,
            signature: [0; 64],
        };
        let ix = withdraw_voucher_ix(
            &pid,
            &instance,
            &account,
            &mint,
            &user_token_account,
            &payer,
            &args,
        )
        .expect("build withdraw_voucher ix");

        let (user_balance, _) = derive_user_balance_pda(&instance, &account, &mint, &pid);
        let (instance_vault, _) = derive_instance_vault(&instance, &mint, &pid);
        let (vault_authority, _) = derive_vault_authority(&instance, &pid);
        let (used_nonce, _) = derive_withdraw_nonce_pda(&instance, &account, args.nonce, &pid);
        let (withdraw_epoch, _) = derive_withdraw_epoch_pda(&instance, &mint, &pid);

        // (name, pubkey, writable, signer) — index i here is account i on-chain.
        let expected: &[(&str, Pubkey, bool, bool)] = &[
            ("instance", instance, false, false),
            ("mint", mint, false, false),
            ("user_balance", user_balance, true, false),
            ("user_token_account", user_token_account, true, false),
            ("instance_vault", instance_vault, true, false),
            ("vault_authority", vault_authority, false, false),
            ("used_nonce", used_nonce, true, false),
            ("withdraw_epoch", withdraw_epoch, true, false),
            ("account", account, false, false),
            ("payer", payer, true, true),
            ("instructions", sysvar_instructions_id(), false, false),
            ("token_program", SPL_TOKEN_PROGRAM_ID, false, false),
            ("system_program", SYSTEM_PROGRAM_ID, false, false),
        ];

        assert_eq!(
            ix.accounts.len(),
            expected.len(),
            "withdraw_voucher account COUNT drifted from the program's \
             WithdrawVoucher struct (expected {}, got {})",
            expected.len(),
            ix.accounts.len()
        );
        for (i, (name, pubkey, writable, signer)) in expected.iter().enumerate() {
            let got = &ix.accounts[i];
            assert_eq!(got.pubkey, *pubkey, "account {i} should be `{name}`");
            assert_eq!(got.is_writable, *writable, "`{name}` writable flag");
            assert_eq!(got.is_signer, *signer, "`{name}` signer flag");
        }
    }

    /// Pin `set_withdraw_epoch_cap`'s account list — order, length, and the
    /// writable/signer flags — against the on-chain `SetWithdrawEpochCap`
    /// accounts struct. Same hazard as `withdraw_voucher` above: Anchor binds
    /// accounts POSITIONALLY, so a wrong order silently misassigns them rather
    /// than erroring. Getting it wrong here is worse than a failed tx — this
    /// instruction arms the safety ceiling on a fund-custody program, and the
    /// signer slot is the authority check.
    ///
    /// EXPECTED LIST SOURCE: the BUILT IDL, not memory. Reproduce with:
    ///   cd arborter/chains/solana
    ///   anchor idl build -o /tmp/midrib.json -p midrib
    ///   jq '.instructions[] | select(.name=="set_withdraw_epoch_cap")
    ///       | .accounts' /tmp/midrib.json
    /// (IDL omits `writable`/`signer` when false.) REGENERATE THIS TEST
    /// whenever that accounts struct changes.
    #[test]
    fn set_withdraw_epoch_cap_accounts_match_program() {
        let pid = Pubkey::new_from_array([1; 32]);
        let instance = Pubkey::new_from_array([2; 32]);
        let mint = Pubkey::new_from_array([3; 32]);
        let operator_admin = Pubkey::new_from_array([4; 32]);

        let ix = set_withdraw_epoch_cap_ix(&pid, &instance, &mint, &operator_admin, 1_000)
            .expect("build set_withdraw_epoch_cap ix");

        let (withdraw_epoch, _) = derive_withdraw_epoch_pda(&instance, &mint, &pid);

        // (name, pubkey, writable, signer) — index i here is account i on-chain.
        let expected: &[(&str, Pubkey, bool, bool)] = &[
            ("instance", instance, false, false),
            ("mint", mint, false, false),
            ("withdraw_epoch", withdraw_epoch, true, false),
            ("operator_admin", operator_admin, true, true),
            ("system_program", SYSTEM_PROGRAM_ID, false, false),
        ];

        assert_eq!(
            ix.accounts.len(),
            expected.len(),
            "set_withdraw_epoch_cap account COUNT drifted from the program's \
             SetWithdrawEpochCap struct (expected {}, got {})",
            expected.len(),
            ix.accounts.len()
        );
        for (i, (name, pubkey, writable, signer)) in expected.iter().enumerate() {
            let got = &ix.accounts[i];
            assert_eq!(got.pubkey, *pubkey, "account {i} should be `{name}`");
            assert_eq!(got.is_writable, *writable, "`{name}` writable flag");
            assert_eq!(got.is_signer, *signer, "`{name}` signer flag");
        }

        // Discriminator + borsh args: sha256("global:set_withdraw_epoch_cap")[..8]
        // then the u64 cap, little-endian.
        assert_eq!(
            &ix.data[..8],
            &[251, 61, 122, 122, 154, 228, 208, 222],
            "discriminator drifted from the IDL's set_withdraw_epoch_cap"
        );
        assert_eq!(&ix.data[8..], &1_000u64.to_le_bytes());
        assert_eq!(ix.program_id, pid);
    }

    /// `cap = 0` is the "unlimited" sentinel and must reach the program as a
    /// literal zero — never rejected or coerced client-side.
    #[test]
    fn set_withdraw_epoch_cap_encodes_zero_as_unlimited() {
        let pid = Pubkey::new_from_array([1; 32]);
        let ix = set_withdraw_epoch_cap_ix(
            &pid,
            &Pubkey::new_from_array([2; 32]),
            &Pubkey::new_from_array([3; 32]),
            &Pubkey::new_from_array([4; 32]),
            0,
        )
        .expect("build set_withdraw_epoch_cap ix with cap=0");
        assert_eq!(&ix.data[8..], &0u64.to_le_bytes());
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
        assert!(
            a[0].is_signer && a[0].is_writable,
            "payer signs + funds rent"
        );
        assert_eq!(a[1].pubkey, ata);
        assert!(
            a[1].is_writable && !a[1].is_signer,
            "ata is created (writable)"
        );
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
    fn system_transfer_ix_matches_wire_format() {
        // Pin the hand-encoded System `Transfer` layout: u32-LE discriminant 2
        // followed by u64-LE lamports, [from signer+writable, to writable].
        let from = Pubkey::new_from_array([1; 32]);
        let to = Pubkey::new_from_array([2; 32]);
        let ix = system_transfer_ix(&from, &to, 1_234_567);
        assert_eq!(ix.program_id, SYSTEM_PROGRAM_ID);
        let mut expected = 2u32.to_le_bytes().to_vec();
        expected.extend_from_slice(&1_234_567u64.to_le_bytes());
        assert_eq!(ix.data, expected);
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable);
        assert!(!ix.accounts[1].is_signer && ix.accounts[1].is_writable);
    }

    #[test]
    fn wsol_wrap_unwrap_ixs_are_exact() {
        // WSOL mint decodes to the well-known pubkey (a valid 32-byte base58).
        let wsol = Pubkey::from_str(WSOL_MINT).expect("WSOL mint parses");
        assert!(is_wsol_mint(WSOL_MINT));
        assert!(!is_wsol_mint("So11111111111111111111111111111111111111111")); // the "native mint" lookalike

        let ata = Pubkey::new_from_array([3; 32]);
        let dest = Pubkey::new_from_array([4; 32]);
        let owner = Pubkey::new_from_array([5; 32]);

        // SyncNative: token program, single writable non-signer account, data [17].
        let sync = sync_native_ix(&ata);
        assert_eq!(sync.program_id, SPL_TOKEN_PROGRAM_ID);
        assert_eq!(sync.data, vec![17]);
        assert_eq!(sync.accounts.len(), 1);
        assert_eq!(sync.accounts[0].pubkey, ata);
        assert!(sync.accounts[0].is_writable && !sync.accounts[0].is_signer);

        // CloseAccount: [ata w, dest w, owner signer], data [9].
        let close = close_token_account_ix(&ata, &dest, &owner);
        assert_eq!(close.program_id, SPL_TOKEN_PROGRAM_ID);
        assert_eq!(close.data, vec![9]);
        assert_eq!(close.accounts.len(), 3);
        assert_eq!(close.accounts[0].pubkey, ata);
        assert!(close.accounts[0].is_writable && !close.accounts[0].is_signer);
        assert_eq!(close.accounts[1].pubkey, dest);
        assert!(close.accounts[1].is_writable && !close.accounts[1].is_signer);
        assert_eq!(close.accounts[2].pubkey, owner);
        assert!(!close.accounts[2].is_writable && close.accounts[2].is_signer);

        let _ = wsol; // parsed above; the mint constant is the assertion
    }

    #[test]
    fn pdas_are_stable() {
        let program_id = Pubkey::new_from_array([9; 32]);
        let inst = Pubkey::new_from_array([6; 32]);
        let user = Pubkey::new_from_array([7; 32]);
        let mint = Pubkey::new_from_array([8; 32]);
        let (bal_a, _) = derive_user_balance_pda(&inst, &user, &mint, &program_id);
        let (bal_b, _) = derive_user_balance_pda(&inst, &user, &mint, &program_id);
        assert_eq!(bal_a, bal_b);
        assert_ne!(bal_a, inst);
    }
}
