//! `UserAccount` loader and lazy-creation helper.

use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed,
    program_error::ProgramError, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::program::YdeltaError;
use crate::require;
use crate::state::user_account::{
    user_account_expand, user_account_pda, UserAccountFixed, USER_ACCOUNT_SEED,
};
use crate::state::USER_ACCOUNT_FIXED_SIZE;
use crate::validation::{Signer, YdeltaAccountInfo};

/// Wrap or lazy-allocate the signer's `UserAccount` PDA, expanding the
/// dynamic region if no free block is available.
pub fn ensure_user_account_for_signer<'a, 'info>(
    signer: &Signer<'a, 'info>,
    user_account_ai: &'a AccountInfo<'info>,
    system_program_ai: &'a AccountInfo<'info>,
) -> Result<YdeltaAccountInfo<'a, 'info, UserAccountFixed>, ProgramError> {
    let owner = signer.info.key;
    let (expected_pda, bump) = user_account_pda(owner);
    require!(
        *user_account_ai.key == expected_pda,
        YdeltaError::IncorrectAccount,
        "user_account PDA does not match [b\"user\", signer]"
    )?;

    let needs_init = user_account_ai.lamports() == 0;
    if needs_init {
        // Allocate PDA at the right size + transfer rent.
        let rent = Rent::get()?;
        let lamports = rent.minimum_balance(USER_ACCOUNT_FIXED_SIZE);
        let owner_bytes = owner.to_bytes();
        let bump_arr = [bump];
        let signer_seeds: &[&[u8]] = &[USER_ACCOUNT_SEED, &owner_bytes, &bump_arr];
        invoke_signed(
            &system_instruction::create_account(
                signer.info.key,
                user_account_ai.key,
                lamports,
                USER_ACCOUNT_FIXED_SIZE as u64,
                &crate::id(),
            ),
            &[
                signer.info.clone(),
                user_account_ai.clone(),
                system_program_ai.clone(),
            ],
            &[signer_seeds],
        )?;

        // Stamp the fresh header.
        {
            let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
            let header: &mut UserAccountFixed =
                bytemuck::from_bytes_mut(&mut data[..USER_ACCOUNT_FIXED_SIZE]);
            *header = UserAccountFixed::new_empty(*owner, bump);
        }
    }

    // Wrap with the typed view (validates ownership + discriminant).
    let typed = YdeltaAccountInfo::<UserAccountFixed>::new(user_account_ai)?;

    // Make sure there's a free block available for any upsert the
    // caller is about to do. Cheap check (discriminator + counter) on
    // the typical hot path; only re-allocs on cold first use.
    let has_free = typed.get_fixed()?.has_free_block();
    if !has_free {
        expand_user_account(signer, user_account_ai)?;
    }
    Ok(typed)
}

/// Realloc the user account by one `USER_ACCOUNT_BLOCK_SIZE` block
/// and link the fresh chunk onto the free list. Mirrors
/// `expand_market`. Caller is responsible for ensuring the signer is
/// the rent-paying owner.
pub fn expand_user_account<'a, 'info>(
    payer: &Signer<'a, 'info>,
    user_account_ai: &'a AccountInfo<'info>,
) -> ProgramResult {
    let new_size = user_account_ai.data_len() + crate::state::USER_ACCOUNT_BLOCK_SIZE;
    let rent: Rent = Rent::get()?;
    let new_min = rent.minimum_balance(new_size);
    let old_min = rent.minimum_balance(user_account_ai.data_len());
    let lamports_diff = new_min.saturating_sub(old_min);
    if lamports_diff > 0 {
        invoke_signed(
            &system_instruction::transfer(payer.info.key, user_account_ai.key, lamports_diff),
            &[payer.info.clone(), user_account_ai.clone()],
            &[],
        )?;
    }

    #[allow(deprecated)]
    user_account_ai.realloc(new_size, false)?;

    let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
    let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
    user_account_expand(fixed, dynamic)?;
    Ok(())
}
