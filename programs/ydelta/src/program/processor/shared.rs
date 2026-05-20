use std::cell::{Ref, RefMut};
use std::mem::size_of;

use bytemuck::Pod;
use hypertree::{
    get_helper, get_mut_helper, FreeListNode, Get, HyperTreeReadOperations,
    HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL,
};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, instruction::Instruction,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};

use crate::state::market::{ClaimedSeatTreeReadOnly, MarketUnusedFreeListPadding};
use crate::state::user_account::{
    get_mut_helper_market_position, upsert_market_position, UserAccountFixed,
};
use crate::state::{
    market_helpers::market_expand, ClaimedSeat, DynamicAccount, MarketFixed, RestingOrder,
    MARKET_BLOCK_SIZE, USER_ACCOUNT_FIXED_SIZE,
};
use crate::validation::{Signer, YdeltaAccount, YdeltaAccountInfo};

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

/// Count the resting orders in the market's asks RB-tree.
///
/// The asks tree holds only unbounded vault risk-profile asks, and a
/// borrower bid crosses each at most once. The count is therefore a
/// safe upper bound on the number of `MatchedLoan` blocks a single
/// `match_borrower_bid` pass can allocate.
pub(crate) fn count_resting_asks<'a, 'info, T: YdeltaAccount + Pod + Clone>(
    market_account_info: &YdeltaAccountInfo<'a, 'info, T>,
) -> Result<usize, solana_program::program_error::ProgramError> {
    let market_data: &Ref<&mut [u8]> = &market_account_info.try_borrow_data()?;
    let fixed: &MarketFixed = get_helper::<MarketFixed>(market_data, 0_u32);
    let root = fixed.asks_root_index;
    let dynamic = &market_data[size_of::<MarketFixed>()..];
    let tree = RedBlackTreeReadOnly::<RestingOrder>::new(dynamic, root, NIL);
    Ok(tree.iter::<RestingOrder>().count())
}

/// Count the blocks currently on the market's free list by walking the
/// `FreeListNode` chain from `free_list_head_index`.
fn count_market_free_blocks(market_data: &[u8]) -> usize {
    let fixed: &MarketFixed = get_helper::<MarketFixed>(market_data, 0_u32);
    let dynamic = &market_data[size_of::<MarketFixed>()..];
    let mut count = 0usize;
    let mut idx = fixed.free_list_head_index;
    while idx != NIL {
        let node: &FreeListNode<MarketUnusedFreeListPadding> =
            get_helper::<FreeListNode<MarketUnusedFreeListPadding>>(dynamic, idx);
        count += 1;
        idx = node.get_next_index();
    }
    count
}

/// Grow the market account until its free list holds at least
/// `min_free_blocks` blocks. Each iteration reallocs one
/// `MARKET_BLOCK_SIZE` block, tops up rent, and links it onto the free
/// list. No-op when enough free blocks already exist.
pub(crate) fn expand_market_to_free_blocks<'a, 'info, T: YdeltaAccount + Pod + Clone>(
    payer: &AccountInfo<'info>,
    market_account_info: &YdeltaAccountInfo<'a, 'info, T>,
    min_free_blocks: usize,
) -> ProgramResult {
    let payer = Signer::new_payer(payer)?;
    loop {
        let free_count: usize = {
            let market_data: &Ref<&mut [u8]> = &market_account_info.try_borrow_data()?;
            count_market_free_blocks(market_data)
        };
        if free_count >= min_free_blocks {
            break;
        }
        expand_market(&payer, market_account_info)?;
    }
    Ok(())
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

/// Zero an account's data and refund its rent lamports to `refund_target`.
pub fn close_account_and_refund(
    account: &AccountInfo<'_>,
    refund_target: &AccountInfo<'_>,
) -> ProgramResult {
    {
        let mut data = account.try_borrow_mut_data()?;
        for byte in data.iter_mut() {
            *byte = 0;
        }
    }

    let lamports = account.lamports();
    **account.try_borrow_mut_lamports()? = 0;
    **refund_target.try_borrow_mut_lamports()? = refund_target
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(())
}
