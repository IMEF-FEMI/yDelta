use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

use crate::program::YdeltaError;
use crate::require;
use crate::state::user_account::{
    user_account_expand, user_account_pda, UserAccountFixed, USER_ACCOUNT_SEED,
};
use crate::state::USER_ACCOUNT_FIXED_SIZE;
use crate::validation::{Signer, YdeltaAccountInfo};

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

    let needs_init = user_account_ai.data_is_empty();
    if needs_init {
        let rent = Rent::get()?;
        let required = rent.minimum_balance(USER_ACCOUNT_FIXED_SIZE);
        let owner_bytes = owner.to_bytes();
        let bump_arr = [bump];
        let signer_seeds: &[&[u8]] = &[USER_ACCOUNT_SEED, &owner_bytes, &bump_arr];

        let current = user_account_ai.lamports();
        if current < required {
            invoke(
                &system_instruction::transfer(
                    signer.info.key,
                    user_account_ai.key,
                    required - current,
                ),
                &[
                    signer.info.clone(),
                    user_account_ai.clone(),
                    system_program_ai.clone(),
                ],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(user_account_ai.key, USER_ACCOUNT_FIXED_SIZE as u64),
            &[user_account_ai.clone(), system_program_ai.clone()],
            &[signer_seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(user_account_ai.key, &crate::id()),
            &[user_account_ai.clone(), system_program_ai.clone()],
            &[signer_seeds],
        )?;

        {
            let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
            let header: &mut UserAccountFixed =
                bytemuck::from_bytes_mut(&mut data[..USER_ACCOUNT_FIXED_SIZE]);
            *header = UserAccountFixed::new_empty(*owner, bump);
        }
    }

    let typed = YdeltaAccountInfo::<UserAccountFixed>::new(user_account_ai)?;

    {
        let dynamic_len = user_account_ai
            .data_len()
            .saturating_sub(USER_ACCOUNT_FIXED_SIZE);
        let fixed = typed.get_fixed()?;
        let in_range = |idx: u32| idx == hypertree::NIL || (idx as usize) < dynamic_len;
        require!(
            in_range(fixed.vault_positions_root_index)
                && in_range(fixed.market_positions_root_index)
                && in_range(fixed.open_loans_root_index)
                && in_range(fixed.free_list_head_index)
                && (fixed.num_bytes_allocated as usize) <= dynamic_len,
            YdeltaError::IncorrectAccount,
            "user_account dynamic-region pointer out of bounds"
        )?;
    }

    let has_free = typed.get_fixed()?.has_free_block();
    if !has_free {
        expand_user_account(signer, user_account_ai)?;
    }
    Ok(typed)
}

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
