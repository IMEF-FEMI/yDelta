//! Convert `lender_claimable_atoms` into lender seat shares.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanClaimedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{accrue_loan, LoanFixed, LoanState, LOAN_FIXED_SIZE};
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::validation::loaders::ClaimRepaymentContext;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ClaimRepaymentParams {
    pub lender_seat_index_hint: Option<DataIndex>,
}

pub fn process_claim_repayment(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ClaimRepaymentParams::try_from_slice(data)?;
    let ctx = ClaimRepaymentContext::load(accounts)?;
    let ClaimRepaymentContext {
        payer,
        market,
        loan,
        debt_bank,
        user_account_ai,
        cranker_refund,
    } = ctx;

    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;
    let _ = params.lender_seat_index_hint;

    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    // Accrue and snapshot the relevant fields.
    // Gate on outstanding_debt == 0: claim_repayment models a
    // post-settlement drain. Allowing it mid-loan would credit the
    // lender against atoms that haven't physically arrived in
    // market.lender_integration yet (lender_claimable_atoms includes
    // accrued interest from accrue_loan plus the principal stamped at
    // match, but only repaid atoms have actually been deposited).
    let (
        lender_seat_index,
        claimable_atoms_pre,
        will_close,
        protocol_fee_atoms_pre,
        lender_debt_snapshot_fp48,
    ): (DataIndex, u64, bool, u64, u128) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        accrue_loan(header, now_unix_ts, grace_period_seconds)?;
        require!(
            header.outstanding_debt_atoms == 0,
            YdeltaError::InvalidArgument,
            "claim_repayment: outstanding_debt_atoms is {} (must be 0; \
             claim is post-settlement only)",
            header.outstanding_debt_atoms
        )?;
        // Fixed-term lock-up. Lender cannot drain until the loan
        // reaches maturity, even if the borrower repaid early. This
        // preserves the term contract: a 30-day loan repaid on day 5
        // still locks the lender's principal for the full 30 days.
        // settle_matured_loan / liquidate_loan paths satisfy this
        // naturally (they run post-maturity).
        require!(
            now_unix_ts >= header.matures_at_unix,
            YdeltaError::LoanNotMatured,
            "claim_repayment: now ({}) < matures_at_unix ({}); \
             fixed-term lock-up requires waiting until maturity",
            now_unix_ts,
            { header.matures_at_unix }
        )?;
        let claimable_pre = header.lender_claimable_atoms;
        let close = true; // outstanding == 0 by the require! above
        (
            header.lender_seat_index,
            claimable_pre,
            close,
            header.accumulated_protocol_fee_atoms,
            header.lender_debt_share_price_snapshot_fp48,
        )
    };

    // Validate signer is the lender's seat owner.
    {
        let market_data = market.info.try_borrow_data()?;
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
        let seat = get_helper_seat(dynamic, lender_seat_index).get_value();
        require!(
            seat.owner == *payer.info.key,
            YdeltaError::OrderNotOwnedBySigner,
            "signer is not the lender of this loan"
        )?;
    }

    // Convert claimable atoms to debt-bank shares at the
    // lender's place-time snapshot, so the seat decrement is
    // byte-symmetric with the encumber that ran at place_order. Using
    // the live bank value here would introduce drift between the
    // place-time encumbrance and the post-claim release if marginfi's
    // share value moved during the loan's life.
    let claim_shares: u128 = if claimable_atoms_pre > 0 {
        crate::state::market_helpers::atoms_to_shares_at_snapshot(
            claimable_atoms_pre,
            lender_debt_snapshot_fp48,
        )
    } else {
        0
    };

    // Update the lender's seat. The lender's principal has been
    // sitting in `debt_encumbered_shares` since `place_order`'s
    // encumbrance. On claim we move (claim_shares) from encumbered →
    // withdrawable. Use saturating subtraction on encumbered to absorb
    // accrual rounding (lender_claimable can grow beyond the original
    // principal due to accrued interest; the excess is "minted" out
    // of the market's USDC asset growth — corresponds to the
    // matching marginfi.deposit done at repay time).
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
        seat.debt_encumbered_shares = seat.debt_encumbered_shares.saturating_sub(claim_shares);
        seat.debt_withdrawable_shares = seat
            .debt_withdrawable_shares
            .checked_add(claim_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        if will_close {
            // open_lend_count was bumped at place_order time; clear on
            // full settlement.
            seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
        }
    }

    // Zero claimable on the loan body. If both counters
    // (outstanding + claimable) are now zero, sweep the protocol fee
    // onto the market and close the PDA.
    let protocol_fee_shares_swept: u128;
    let did_close: bool;
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.lender_claimable_atoms = 0;
        if header.outstanding_debt_atoms == 0 {
            // Final settlement: protocol fee in atoms → shares.
            let protocol_fee_atoms = protocol_fee_atoms_pre;
            protocol_fee_shares_swept = if protocol_fee_atoms > 0 {
                MarginfiV18Adapter
                    .amount_to_asset_shares(&[debt_bank.info.clone()], protocol_fee_atoms)?
            } else {
                0
            };
            header.accumulated_protocol_fee_atoms = 0;
            // Mark the loan terminal — subsequent reads should see Repaid
            // (terminal state) before we zero the discriminator below.
            header.state = LoanState::Repaid as u8;
            did_close = true;
        } else {
            // Partial settlement (borrower hasn't fully repaid yet) —
            // leave the loan open for future repays + claims.
            protocol_fee_shares_swept = 0;
            did_close = false;
        }
    }

    if did_close {
        // Accumulate protocol fee on the market header.
        if protocol_fee_shares_swept > 0 {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            da.fixed.accumulated_protocol_fee_shares = da
                .fixed
                .accumulated_protocol_fee_shares
                .checked_add(protocol_fee_shares_swept)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        close_loan_pda(&loan, cranker_refund)?;
    }

    emit_stack(LoanClaimedLog {
        market: market_key,
        loan: *loan.info.key,
        lender: *payer.info.key,
        claimed_atoms: claimable_atoms_pre,
        _pad0: [0; 8],
        claimed_shares: claim_shares,
        protocol_fee_shares_swept,
        closed: did_close as u8,
        _padding: [0; 15],
    })?;

    // Sync the lender's MarketPosition mirror after we move shares
    // from encumbered → withdrawable on their seat. Drop the
    // open-loan ref on full settlement (the loan PDA gets closed).
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    if did_close {
        use crate::state::user_account::{remove_open_loan, UserAccountFixed};
        use crate::state::USER_ACCOUNT_FIXED_SIZE;
        // The loan PDA is now zeroed but its key is still valid as
        // a tree key; use the known address from `loan.info` before we
        // re-borrow the user_account.
        let loan_pda_key = *loan.info.key;
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let _ = remove_open_loan(fixed, dynamic, loan_pda_key);
    }

    Ok(())
}

/// Close a `LoanFixed` PDA: zero its discriminator (so any subsequent
/// `try_from_account_data` fails) and — if the optional `cranker_refund`
/// AccountInfo was passed and matches `loan.created_by` — drain the
/// rent lamports to it. If absent, lamports stay stranded on the
/// (now-zeroed) loan address; a future sweeper ix can recover them.
fn close_loan_pda<'a, 'info>(
    loan: &crate::validation::YdeltaAccountInfo<'a, 'info, LoanFixed>,
    cranker_refund: Option<&'a AccountInfo<'info>>,
) -> ProgramResult {
    let loan_info = loan.info;
    let created_by: Pubkey = loan.get_fixed()?.created_by;

    // Zero the data first (drops `borrow_data`'s lifetime before we
    // touch lamports).
    {
        let mut data: RefMut<&mut [u8]> = loan_info.try_borrow_mut_data()?;
        for byte in data.iter_mut() {
            *byte = 0;
        }
    }

    if let Some(refund_ai) = cranker_refund {
        if *refund_ai.key == created_by {
            let lamports = loan_info.lamports();
            **loan_info.try_borrow_mut_lamports()? = 0;
            **refund_ai.try_borrow_mut_lamports()? = refund_ai
                .lamports()
                .checked_add(lamports)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        // If the passed account doesn't match `created_by`, silently
        // skip the refund — the loan still closes (data zeroed), the
        // lamports just stay stranded. Loud reject would be
        // user-hostile when callers fall back on the no-refund path.
    }

    Ok(())
}
