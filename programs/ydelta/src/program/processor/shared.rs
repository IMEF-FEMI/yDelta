use std::cell::{Ref, RefMut};
use std::mem::size_of;

use bytemuck::Pod;
use hypertree::{get_helper, get_mut_helper, Get};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, instruction::Instruction, pubkey::Pubkey,
    rent::Rent, sysvar::Sysvar,
};

use crate::state::{market_helpers::market_expand, DynamicAccount, MarketFixed, MARKET_BLOCK_SIZE};
use crate::validation::{Signer, YdeltaAccount, YdeltaAccountInfo};

/// Re-stamp the market-side vault `ClaimedSeat`'s
/// `risk_profile_max_ltv_bps` from the live `RiskProfile`. Call at the
/// top of every profile-affecting ix
/// (place/cancel/update_order_for_risk_profile, claim_curator_fee).
/// Picks up the latest policy when curators rotate risk via
/// `update_risk_profile` without requiring re-claim of the seat.
///
/// Silent no-op if either the seat or profile lookup misses (defensive
/// against partial tear-down). Idempotent: calling repeatedly without
/// state change is a no-op.
pub(crate) fn sync_vault_seat_from_profile(
    market_ai: &AccountInfo<'_>,
    vault_ai: &AccountInfo<'_>,
    profile_id: u8,
) -> ProgramResult {
    use crate::state::claimed_seat::{ClaimedSeat, OWNER_KIND_RISK_PROFILE};
    use crate::state::market::{get_mut_helper_seat, ClaimedSeatTreeReadOnly};
    use crate::state::vault::{
        get_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
    };
    use crate::state::GLOBAL_VAULT_FIXED_SIZE;
    use hypertree::{HyperTreeReadOperations, NIL};
    use solana_program::pubkey::Pubkey;

    let vault_key = *vault_ai.key;

    // Read the profile's live max_ltv_bps.
    let live_max_ltv: u16 = {
        let vault_data = vault_ai.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        if idx == NIL {
            return Ok(());
        }
        get_helper_risk_profile(dynamic, idx)
            .get_value()
            .max_ltv_bps
    };

    // Re-stamp the market-side seat.
    let mut market_data: RefMut<&mut [u8]> = market_ai.try_borrow_mut_data()?;
    let dyn_offset = std::mem::size_of::<MarketFixed>();
    let header: &MarketFixed = bytemuck::from_bytes(&market_data[..dyn_offset]);
    let root = header.claimed_seats_root_index;
    let dynamic = &mut market_data[dyn_offset..];
    let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, profile_id);
    let seat_idx = {
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, root, NIL);
        tree.lookup_index(&probe)
    };
    if seat_idx != NIL {
        let seat = get_mut_helper_seat(dynamic, seat_idx).get_mut_value();
        if seat.risk_profile_max_ltv_bps != live_max_ltv {
            seat.risk_profile_max_ltv_bps = live_max_ltv;
        }
    }
    Ok(())
}

/// Borrow the market account as a mutable `DynamicAccount` view (header
/// borrowed mutably, dynamic region borrowed mutably).
pub fn get_mut_dynamic_account<'a, T: Get>(
    data: &'a mut RefMut<'_, &mut [u8]>,
) -> DynamicAccount<&'a mut T, &'a mut [u8]> {
    let (fixed_data, dynamic) = data.split_at_mut(size_of::<T>());
    let fixed: &mut T = get_mut_helper::<T>(fixed_data, 0_u32);
    DynamicAccount { fixed, dynamic }
}

/// Read-only counterpart to `get_mut_dynamic_account`.
pub fn get_dynamic_account<'a, T: Get>(
    data: &'a Ref<'a, &'a mut [u8]>,
) -> DynamicAccount<&'a T, &'a [u8]> {
    let (fixed_data, dynamic) = data.split_at(size_of::<T>());
    let fixed: &T = get_helper::<T>(fixed_data, 0_u32);
    DynamicAccount { fixed, dynamic }
}

/// Grow the market account by one `MARKET_BLOCK_SIZE` block when the free
/// list is empty. No-op when at least one free block is available.
pub(crate) fn expand_market_if_needed<'a, 'info, T: YdeltaAccount + Pod + Clone>(
    payer: &AccountInfo<'info>,
    market_account_info: &YdeltaAccountInfo<'a, 'info, T>,
) -> ProgramResult {
    let need_expand: bool = {
        let market_data: &Ref<&mut [u8]> = &market_account_info.try_borrow_data()?;
        let fixed: &MarketFixed = get_helper::<MarketFixed>(market_data, 0_u32);
        !fixed.has_free_block()
    };
    if !need_expand {
        return Ok(());
    }
    expand_market(&Signer::new_payer(payer)?, market_account_info)
}

/// Realloc the market account by one block, top up rent, and link the new
/// block onto the free list.
pub(crate) fn expand_market<'a, 'info, T: YdeltaAccount + Pod + Clone>(
    payer: &Signer<'a, 'info>,
    market_account_info: &YdeltaAccountInfo<'a, 'info, T>,
) -> ProgramResult {
    let expandable: &AccountInfo = market_account_info.info;
    let new_size = expandable.data_len() + MARKET_BLOCK_SIZE;

    let rent: Rent = Rent::get()?;
    let new_min = rent.minimum_balance(new_size);
    let old_min = rent.minimum_balance(expandable.data_len());
    let lamports_diff = new_min.saturating_sub(old_min);

    let payer_info: &AccountInfo = payer.info;
    invoke(
        &solana_program::system_instruction::transfer(
            payer_info.key,
            expandable.key,
            lamports_diff,
        ),
        &[payer_info.clone(), expandable.clone()],
    )?;

    #[allow(deprecated)]
    expandable.realloc(new_size, false)?;

    let market_data: &mut RefMut<&mut [u8]> = &mut expandable.try_borrow_mut_data()?;
    let da: DynamicAccount<&mut MarketFixed, &mut [u8]> = get_mut_dynamic_account(market_data);
    market_expand(da.fixed, da.dynamic)?;
    Ok(())
}

/// Signer-side mirror sync helper. Reads the signer's `ClaimedSeat`
/// from the market and copies the four balance fields onto the
/// corresponding `MarketPosition` on the signer's `UserAccount`.
/// Called by every signer-side processor after the canonical seat
/// mutation. Idempotent — upserts the MarketPosition node if missing,
/// otherwise overwrites the existing balances.
pub fn sync_signer_market_position(
    market_ai: &AccountInfo,
    user_account_ai: &AccountInfo,
    signer_pubkey: &Pubkey,
) -> ProgramResult {
    use crate::state::user_account::{
        get_mut_helper_market_position, upsert_market_position, UserAccountFixed,
    };
    use crate::state::{
        market::ClaimedSeatTreeReadOnly, ClaimedSeat, MarketFixed, USER_ACCOUNT_FIXED_SIZE,
    };
    use hypertree::{HyperTreeReadOperations, NIL};

    // Read the canonical seat first.
    let market_key = *market_ai.key;
    let (seat_index, seat_snapshot): (hypertree::DataIndex, ClaimedSeat) = {
        let market_data = market_ai.try_borrow_data()?;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..fixed_size]);
        let dynamic = &market_data[fixed_size..];
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let probe = ClaimedSeat::new_empty(*signer_pubkey, /*owner_kind=*/ 0, 0);
        let idx = tree.lookup_index(&probe);
        if idx == NIL {
            // No seat → nothing to mirror. Caller must ensure this is
            // never called before claim_seat in the same flow.
            return Ok(());
        }
        let seat = *crate::state::market::get_helper_seat(dynamic, idx).get_value();
        (idx, seat)
    };

    // Upsert the mirror + write the four balance fields.
    let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
    let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
    let mp_idx = upsert_market_position(fixed, dynamic, market_key, seat_index)?;
    let mp_node = get_mut_helper_market_position(dynamic, mp_idx);
    mp_node.get_mut_value().sync_from_seat(&seat_snapshot);
    Ok(())
}

pub fn invoke(ix: &Instruction, account_infos: &[AccountInfo<'_>]) -> ProgramResult {
    #[cfg(target_os = "solana")]
    {
        solana_invoke::invoke_unchecked(ix, account_infos)
    }
    #[cfg(not(target_os = "solana"))]
    {
        solana_program::program::invoke(ix, account_infos)
    }
}
