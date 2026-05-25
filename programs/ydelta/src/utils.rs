//! Cross-module utility helpers shared across the program crate.
//! Provides the keccak-based account / log discriminant derivation and a
//! thin wrapper around `system_instruction::create_account` for
//! `invoke_signed` PDAs.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, keccak, program::invoke_signed,
    pubkey::Pubkey, rent::Rent, system_instruction,
};

/// Derives the 8-byte discriminant for `type_id` as
/// `keccak(program_id || type_id)[..8]` interpreted as little-endian `u64`.
/// Used by both the [`crate::logs`] event encoder and on-disk account
/// header tagging — same string must round-trip to the same bytes.
pub fn get_discriminant(type_id: &str) -> u64 {
    let bytes: [u8; 8] = keccak::hashv(&[crate::ID.as_ref(), type_id.as_bytes()]).as_ref()[..8]
        .try_into()
        .expect("keccak hash sliced to [..8] is always 8 bytes");
    u64::from_le_bytes(bytes)
}

/// CPIs `system_instruction::create_account` for a PDA at `new_account`.
/// `seeds` are the full PDA seeds including the bump; this wraps the
/// outer slice that `invoke_signed` requires. `space` is funded at
/// rent-exempt minimum and the resulting account is assigned to
/// `program_owner`.
pub fn create_account<'a, 'info>(
    payer: &'a AccountInfo<'info>,
    new_account: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    program_owner: &Pubkey,
    rent: &Rent,
    space: u64,
    seeds: Vec<Vec<u8>>,
) -> ProgramResult {
    invoke_signed(
        &system_instruction::create_account(
            payer.key,
            new_account.key,
            rent.minimum_balance(space as usize),
            space,
            program_owner,
        ),
        &[payer.clone(), new_account.clone(), system_program.clone()],
        &[seeds
            .iter()
            .map(|seed| seed.as_slice())
            .collect::<Vec<&[u8]>>()
            .as_slice()],
    )
}
