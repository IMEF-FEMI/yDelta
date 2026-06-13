//! `CancelOrder` — borrower cancels their own resting bid.
//! Releases the bid's encumbered collateral back to the seat's
//! withdrawable bucket at the order's stored share-price snapshot,
//! removes the bid from the bids tree, and drops the `UserOrderRef`.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, OrderCanceledLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::market::get_helper_order;
use crate::state::market_helpers::{cancel_order_by_index, get_seat_index_with_hint, lookup_order_by_seq};
use crate::state::user_account::{remove_user_order, UserAccountFixed};
use crate::state::{MarketFixed, Side, USER_ACCOUNT_FIXED_SIZE};
use crate::validation::loaders::CancelOrderContext;

use super::shared::get_mut_dynamic_account;

/// Cancel parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct CancelOrderParams {
    /// Per-market sequence the bid was assigned at placement.
    pub order_sequence: u64,
    /// Optional `ClaimedSeat` hint; fallback lookup runs when stale.
    pub seat_index_hint: Option<DataIndex>,
}

/// Borrower-signed cancel of a resting bid.
pub fn process_cancel_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CancelOrderParams::try_from_slice(data)?;
    let CancelOrderContext {
        payer,
        market,
        user_account_ai,
    } = CancelOrderContext::load(accounts)?;

    let market_key = *market.info.key;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);

        let seat_index = get_seat_index_with_hint(
            da.fixed,
            da.dynamic,
            payer.info.key,
            params.seat_index_hint,
        )?;
        let order_index = lookup_order_by_seq(
            da.fixed,
            da.dynamic,
            seat_index,
            params.order_sequence,
            None,
        )?;
        // v1: every user-owned resting order is a bid (asks are
        // sub-vault-only). Defense-in-depth against tree confusion.
        let side = get_helper_order(da.dynamic, order_index).get_value().side;
        require!(
            side == Side::Bid as u8,
            YdeltaError::IncorrectAccount,
            "cancel_order: order {} is not a bid (side={})",
            params.order_sequence,
            side
        )?;
        cancel_order_by_index(da.fixed, da.dynamic, seat_index, order_index)?;
    }

    // Drop the user-side order ref (idempotent on NIL).
    {
        let data = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        remove_user_order(fixed, dynamic, market_key, params.order_sequence)?;
    }

    emit_stack(OrderCanceledLog {
        market: market_key,
        trader: *payer.info.key,
        sequence: params.order_sequence,
        side: Side::Bid as u8,
        _pad0: [0; 7],
    })?;

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}
