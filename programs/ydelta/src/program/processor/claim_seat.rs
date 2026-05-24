use std::cell::RefMut;

use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, ClaimSeatLog};
use crate::state::user_account::{upsert_market_position, UserAccountFixed};
use crate::state::{
    claimed_seat::OWNER_KIND_USER, market::ClaimedSeatTreeReadOnly, ClaimedSeat, MarketFixed,
    USER_ACCOUNT_FIXED_SIZE,
};
use crate::validation::loaders::ClaimSeatContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

pub fn process_claim_seat(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let ClaimSeatContext {
        payer,
        market,
        system_program: _,
        user_account: _,
        user_account_ai,
        system_program_ai: _,
    } = ClaimSeatContext::load(accounts)?;

    expand_market_if_needed(payer.info, &market)?;

    let market_key = *market.info.key;
    let trader = *payer.info.key;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
        da.claim_seat(&trader, OWNER_KIND_USER)?;
    }

    let seat_index_in_market = {
        let market_data = market.info.try_borrow_data()?;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..fixed_size]);
        let dynamic = &market_data[fixed_size..];
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        tree.lookup_index(&ClaimedSeat::new_empty(trader, OWNER_KIND_USER, 0))
    };

    {
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        upsert_market_position(fixed, dynamic, market_key, seat_index_in_market)?;
    }

    emit_stack(ClaimSeatLog {
        market: market_key,
        trader,
    })?;
    Ok(())
}
