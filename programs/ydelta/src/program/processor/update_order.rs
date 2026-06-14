//! `UpdateOrder` — borrower cancel-and-replaces their own
//! resting bid: new rate / term / expiry under a fresh sequence
//! (repricing forfeits time priority). Principal, collateral, and the
//! encumbrance snapshot are untouched — the order keeps its block, so
//! no unencumber/re-encumber round-trip happens. Not a taking ix:
//! a cross left at rest is legal and `MatchCrank` resolves it.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, OrderCanceledLog, OrderPlacedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::market::get_helper_order;
use crate::state::market_helpers::{
    get_seat_index_with_hint, lookup_order_by_seq, update_user_bid,
};
use crate::state::user_account::{insert_user_order, remove_user_order, UserAccountFixed};
use crate::state::{get_now_unix_ts, MarketFixed, OrderType, Side, USER_ACCOUNT_FIXED_SIZE};
use crate::validation::loaders::CancelOrderContext;

use super::shared::get_mut_dynamic_account;

/// Cancel-and-replace parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateOrderParams {
    /// Per-market sequence the bid was assigned at placement.
    pub order_sequence: u64,
    /// Optional `ClaimedSeat` hint; fallback lookup runs when stale.
    pub seat_index_hint: Option<DataIndex>,
    /// Replacement maximum borrower-paid rate in bps.
    pub new_rate_bps: u16,
    /// Replacement loan term in seconds; must be > 0.
    pub new_term_seconds: u32,
    /// Replacement expiry; `0` = never, otherwise must be in the future.
    pub new_last_valid_unix_ts: i64,
    /// Replacement borrower LTV buffer in bps (0 disables). Lets the
    /// borrower tighten or relax their resting bid's origination ceiling.
    pub new_ltv_buffer_bps: u16,
}

/// Borrower-signed cancel-and-replace of a resting bid.
pub fn process_update_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateOrderParams::try_from_slice(data)?;
    require!(
        params.new_ltv_buffer_bps <= 10_000,
        YdeltaError::InvalidArgument,
        "new_ltv_buffer_bps {} exceeds 10000",
        params.new_ltv_buffer_bps
    )?;
    let CancelOrderContext {
        payer,
        market,
        user_account_ai,
    } = CancelOrderContext::load(accounts)?;

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    let (new_sequence, seat_index, principal_atoms, collateral_atoms): (u64, DataIndex, u64, u64) = {
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
        let order = *get_helper_order(da.dynamic, order_index).get_value();
        require!(
            order.side == Side::Bid as u8,
            YdeltaError::IncorrectAccount,
            "update_order: order {} is not a bid (side={})",
            params.order_sequence,
            order.side
        )?;
        let new_sequence = update_user_bid(
            da.fixed,
            da.dynamic,
            seat_index,
            order_index,
            params.new_rate_bps,
            params.new_term_seconds,
            params.new_last_valid_unix_ts,
            params.new_ltv_buffer_bps,
            now,
        )?;
        (
            new_sequence,
            seat_index,
            order.principal_atoms,
            order.collateral_atoms,
        )
    };

    // Re-key the user-side order ref to the fresh sequence. The removal
    // frees a block, so the insert never needs an expand.
    {
        let data = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        remove_user_order(fixed, dynamic, market_key, params.order_sequence)?;
        insert_user_order(
            fixed,
            dynamic,
            market_key,
            new_sequence,
            Side::Bid as u8,
            params.new_rate_bps,
            params.new_term_seconds,
            principal_atoms,
            now,
        )?;
    }

    emit_stack(OrderCanceledLog {
        market: market_key,
        trader: *payer.info.key,
        sequence: params.order_sequence,
        side: Side::Bid as u8,
        _pad0: [0; 7],
    })?;
    emit_stack(OrderPlacedLog {
        market: market_key,
        trader: *payer.info.key,
        trader_seat_index: seat_index,
        side: Side::Bid as u8,
        _reserved_kind: 0,
        order_type: OrderType::Limit as u8,
        _padding1: 0,
        rate_bps: params.new_rate_bps,
        _padding2: 0,
        term_seconds: params.new_term_seconds,
        principal_atoms,
        collateral_atoms,
        sequence: new_sequence,
        last_valid_unix_ts: params.new_last_valid_unix_ts,
    })?;

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}
