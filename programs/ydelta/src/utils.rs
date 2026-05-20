use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, keccak, program::invoke_signed,
    pubkey::Pubkey, rent::Rent, system_instruction,
};

/// Canonical discriminant for a named type. Hashes the program ID with
/// an explicit, caller-supplied `type_id` string.
///
/// The `type_id` MUST be a stable source-level string (callers
/// pass `stringify!(TypeName)`), never `std::any::type_name`: the latter
/// has no stable-ABI guarantee, so a rustc upgrade would silently change
/// every discriminant and break off-chain indexers / account decoding.
pub fn get_discriminant(type_id: &str) -> u64 {
    let bytes: [u8; 8] = keccak::hashv(&[crate::ID.as_ref(), type_id.as_bytes()]).as_ref()[..8]
        .try_into()
        .expect("keccak hash sliced to [..8] is always 8 bytes");
    u64::from_le_bytes(bytes)
}

/// Wraps the system-program `create_account` CPI with a single PDA-seed
/// signer.
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
