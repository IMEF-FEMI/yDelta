//! Borrower repayment for an active `LoanFixed`.

use std::cell::{Ref, RefMut};

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanRepaidLog, SecondaryStaleBidDroppedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{accrue_loan, LoanFixed, LoanState, LOAN_FIXED_SIZE};
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::validation::loaders::RepayContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::{get_mut_dynamic_account, invoke};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct RepayParams {
    pub repay_atoms: u64,
    pub full_repay: bool,
    pub borrower_seat_index_hint: Option<DataIndex>,
}

pub fn process_repay(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let params: RepayParams = RepayParams::try_from_slice(data)?;
    let ctx = RepayContext::load(accounts)?;
    let RepayContext {
        payer,
        market,
        loan,
        borrower_token,
        vault,
        token_program,
        debt_mint,
        marginfi_group,
        marginfi_account,
        debt_bank,
        liquidity_vault,
        // `collateral_bank` is loaded by the context but no longer
        // read here — repay's full-repay credit-back uses the loan's
        // place-time snapshot, not a live bank read.
        collateral_bank: _,
        market_signer,
        market_signer_bump,
        marginfi_program,
        user_account_ai,
    } = ctx;

    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    // Assert the loan is Active and accrue interest to now.
    // Read grace_period_seconds off the market for accrual.
    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    let (borrower_seat_index, outstanding_pre, collateral_atoms) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "Loan is already Repaid (state={})",
            header.state
        )?;
        accrue_loan(header, now_unix_ts, grace_period_seconds)?;
        (
            header.borrower_seat_index,
            header.outstanding_debt_atoms,
            header.collateral_atoms,
        )
    };
    let _ = params.borrower_seat_index_hint;

    // Validate signer is the borrower seat's owner.
    {
        let market_data: Ref<&mut [u8]> = market.info.try_borrow_data()?;
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
        let seat = get_helper_seat(dynamic, borrower_seat_index).get_value();
        require!(
            seat.owner == *payer.info.key,
            YdeltaError::OrderNotOwnedBySigner,
            "signer is not the borrower of this loan"
        )?;
    }

    let repay_atoms: u64 = if params.full_repay {
        outstanding_pre
    } else {
        require!(
            params.repay_atoms > 0,
            YdeltaError::InvalidArgument,
            "repay_atoms must be > 0"
        )?;
        require!(
            params.repay_atoms <= outstanding_pre,
            YdeltaError::InvalidArgument,
            "repay_atoms ({}) exceeds outstanding_debt_atoms ({})",
            params.repay_atoms,
            outstanding_pre
        )?;
        params.repay_atoms
    };

    // Transfer borrower atoms into the staging vault.
    transfer_user_to_vault(
        token_program.info,
        borrower_token.info,
        vault.info,
        debt_mint.info,
        payer.info,
        repay_atoms,
        debt_mint.mint.decimals,
    )?;

    // Route atoms onward.
    // Repay atoms land on market.lender_integration_account regardless
    // of lender kind. Wallet lenders drain via `claim_repayment`;
    // risk-profile lenders drain via `claim_repayment_for_risk_profile`,
    // which additionally moves the realized atoms back into the
    // GlobalVault's marginfi account.
    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
    let adapter_accounts = [
        marginfi_group.info.clone(),
        marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        vault.info.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    // Fixed and P2Pool loans both top up the market's USDC asset via
    // `marginfi.deposit`. When the P2Pool borrow CPI is wired, the
    // P2Pool branch will switch to `MarginfiV18Adapter.repay_atoms(...)`
    // to reduce the borrower's liability directly.
    let _shares_credited: u128 =
        MarginfiV18Adapter.deposit(&adapter_accounts, repay_atoms, &[market_signer_seeds])?;

    // Update the loan; on full repay, credit collateral back.
    // Lender-side accumulators (`lender_claimable_atoms`,
    // `accumulated_protocol_fee_atoms`) stay on the loan body — the
    // claim ix drains them.
    let did_full_repay: bool = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.outstanding_debt_atoms = header
            .outstanding_debt_atoms
            .checked_sub(repay_atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let full = header.outstanding_debt_atoms == 0;
        if full {
            header.state = LoanState::Repaid as u8;
            // Clear the resting-secondary-bid flag — the post-full-repay
            // sweep below removes any resting bid for this loan, so the
            // O(1) duplicate guard must come down with it.
            header.has_resting_secondary_bid = 0;
        }
        full
    };

    if did_full_repay && collateral_atoms > 0 {
        // Credit the collateral back at the loan's place-time
        // snapshot so the share count is byte-symmetric with the
        // place_order encumber. Using the live bank value here would
        // drift withdrawable from what the borrower originally posted
        // by the share-value growth across the loan's life.
        let collateral_snapshot_fp48: u128 = {
            let loan_data = loan.info.try_borrow_data()?;
            let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
            header.borrower_collateral_share_price_snapshot_fp48
        };
        let collateral_shares: u128 = crate::state::market_helpers::atoms_to_shares_at_snapshot(
            collateral_atoms,
            collateral_snapshot_fp48,
        );
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.collateral_withdrawable_shares = seat
            .collateral_withdrawable_shares
            .checked_add(collateral_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
    }

    // Post-full-repay secondary-bid sweep.
    // Any resting `SecondaryLoanSale` bid for this loan is now stale
    // (no lender side left to transfer). Walk the bids tree and remove
    // matches. Cheap — secondary bids are rare. The matching engine
    // also drops stale bids, but doing it here means off-chain UIs and
    // future matching passes don't see the stale order at all.
    if did_full_repay {
        let loan_pda_key = *loan.info.key;
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let swept = crate::state::market_helpers::sweep_stale_secondary_bids_for_loan(
            da.fixed,
            da.dynamic,
            &loan_pda_key,
        )?;
        if swept > 0 {
            // Single log per repay (count, not per-bid). Indexers can
            // re-walk the bids tree if they need per-bid detail.
            emit_stack(SecondaryStaleBidDroppedLog {
                market: market_key,
                loan_pda: loan_pda_key,
                bid_sequence: 0,
                seller_seat_index: hypertree::NIL,
                swept_by: 0, // 0 = process_repay sweep
                _padding: [0; 3],
            })?;
            let _ = swept;
        }
    }

    let outstanding_after = outstanding_pre.saturating_sub(repay_atoms);
    emit_stack(LoanRepaidLog {
        market: market_key,
        loan: *loan.info.key,
        borrower: *payer.info.key,
        repay_atoms,
        outstanding_after,
        full_repay: did_full_repay as u8,
        _padding: [0; 7],
    })?;

    // Sync the borrower's MarketPosition mirror (collateral was
    // credited back to seat on full repay) and drop the open-loan
    // ref on full repay.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    if did_full_repay {
        use crate::state::user_account::{remove_open_loan, UserAccountFixed};
        use crate::state::USER_ACCOUNT_FIXED_SIZE;
        let loan_pda_key = *loan.info.key;
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let _ = remove_open_loan(fixed, dynamic, loan_pda_key);
    }

    Ok(())
}

/// SPL-transfer `amount` atoms from the user's `borrower_token` to the
/// market's staging `vault`, signed by the user.
fn transfer_user_to_vault<'info>(
    token_program: &AccountInfo<'info>,
    trader_token: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            trader_token.key,
            mint.key,
            vault.key,
            owner.key,
            &[],
            amount,
            decimals,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                mint.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            trader_token.key,
            vault.key,
            owner.key,
            &[],
            amount,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    }
}
