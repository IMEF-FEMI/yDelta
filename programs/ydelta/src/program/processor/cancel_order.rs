use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, CancelOrderLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::{
    loan::{LoanFixed, LOAN_FIXED_SIZE},
    market_helpers::{cancel_order_by_index, get_seat_index_with_hint, lookup_order_by_seq},
    MarketFixed,
};
use crate::validation::loaders::OrderContext;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct CancelOrderParams {
    pub order_sequence_number: u64,
    pub order_index_hint: Option<DataIndex>,
    pub seat_index_hint: Option<DataIndex>,
}

pub fn process_cancel_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CancelOrderParams::try_from_slice(data)?;

    let OrderContext {
        payer,
        market,
        _system_program,
        user_account_ai,
        secondary_loan_ai,
    } = OrderContext::load(accounts)?;

    let market_key = *market.info.key;

    let secondary_loan_pda: Option<Pubkey> = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index =
            get_seat_index_with_hint(da.fixed, da.dynamic, payer.info.key, params.seat_index_hint)?;
        let order_index = lookup_order_by_seq(
            da.fixed,
            da.dynamic,
            seat_index,
            params.order_sequence_number,
            params.order_index_hint,
        )?;
        cancel_order_by_index(da.fixed, da.dynamic, seat_index, order_index)?
    };

    // Clear the loan's `has_resting_secondary_bid` flag (O(1)
    // duplicate-check counterpart) when canceling a SecondaryLoanSale
    // bid. The optional trailing loan account is required iff the
    // canceled order was secondary.
    if let Some(loan_pda) = secondary_loan_pda {
        let loan_ai = secondary_loan_ai.ok_or_else(|| {
            solana_program::msg!(
                "cancel_order: secondary bid cancel requires the loan account as the trailing accounts entry so the resting-bid flag can be cleared"
            );
            YdeltaError::IncorrectAccount
        })?;
        require!(
            *loan_ai.key == loan_pda,
            YdeltaError::IncorrectAccount,
            "cancel_order: trailing loan account {} does not match canceled-bid loan_pda {}",
            loan_ai.key,
            loan_pda
        )?;
        require!(
            loan_ai.owner == &crate::ID,
            YdeltaError::IncorrectAccount,
            "cancel_order: trailing loan account is not owned by yDelta"
        )?;
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan_ai.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.has_resting_secondary_bid = 0;
    }

    emit_stack(CancelOrderLog {
        market: market_key,
        trader: *payer.info.key,
        sequence: params.order_sequence_number,
    })?;

    // Sync the signer's MarketPosition mirror after cancel refunds
    // encumbered balances onto the seat.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}
