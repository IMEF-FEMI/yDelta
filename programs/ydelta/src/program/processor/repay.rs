//! Borrower repayment for an active loan.
//!
//! Branches on `loan.loan_type`:
//!
//! - **Fixed** (`loan_type == 0`): the loan body's `outstanding_debt_atoms`
//!   is the authoritative debt. Repay atoms transfer into the market's
//!   staging vault and top up `market.lender_integration_account` via
//!   `marginfi.deposit` — the risk-profile lender later drains them via
//!   `claim_repayment_for_risk_profile`. On full repay the borrower's
//!   collateral seat shares are credited back at the loan's place-time
//!   snapshot and the loan is marked `Repaid`.
//!
//! - **P2Pool** (`loan_type == 1`): the body's `outstanding_debt_atoms`
//!   is decorative. The real debt is the borrower marginfi account's
//!   live variable-rate `liability_shares`. Repay atoms transfer into the
//!   staging vault and retire that liability directly via
//!   `marginfi.repay_atoms` (the same CPI `liquidate_loan` /
//!   `settle_matured_loan` use on their P2Pool arm). The loan is marked
//!   `Repaid` and its PDA closed ONLY when the POST-CPI live liability is
//!   exactly zero — never on a pre-CPI comparison, because marginfi
//!   accrues between the read and the CPI.
//!   A P2Pool loan's seat collateral encumbrance (set when the bid was
//!   placed) is released back to `collateral_withdrawable_shares` on full
//!   repay via `release_loan_collateral` — see the P2Pool close block
//!   below. The borrower's marginfi-side collateral is additionally freed
//!   by marginfi's own solvency check once the liability hits zero.

use std::cell::{Ref, RefMut};

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanRepaidLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{
    accrue_loan, apply_partial_resolution, LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE,
};
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::state::user_account::{remove_open_loan, UserAccountFixed};
use crate::state::USER_ACCOUNT_FIXED_SIZE;
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
        // `collateral_bank` is loaded by the context but unused here —
        // repay's full-repay credit-back uses the loan's place-time
        // snapshot, not a live bank read.
        collateral_bank: _,
        borrower_marginfi_account,
        market_signer,
        market_signer_bump,
        marginfi_program,
        user_account_ai,
        cranker_refund,
    } = ctx;

    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    // Assert the loan is Active and accrue interest to now.
    // Read grace_period_seconds off the market for accrual.
    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    let (borrower_seat_index, loan_type, body_outstanding, collateral_atoms, collateral_snapshot_fp48) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "Loan is already Repaid (state={})",
            header.state
        )?;
        // `accrue_loan` is a no-op for P2Pool loans (marginfi compounds
        // the variable-rate debt); for Fixed loans it grows the body's
        // `outstanding_debt_atoms` / `lender_claimable_atoms`.
        accrue_loan(header, now_unix_ts, grace_period_seconds)?;
        (
            header.borrower_seat_index,
            header.loan_type()?,
            header.outstanding_debt_atoms,
            header.collateral_atoms,
            header.borrower_collateral_share_price_snapshot_fp48,
        )
    };

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

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    // ───────────────────────── P2Pool branch ─────────────────────────
    if loan_type == LoanType::P2Pool {
        // Multiple P2Pool loans on a market share ONE per-market borrower
        // marginfi account (`[b"borrower_marginfi_account", market]`), so
        // the bank liability is COMMINGLED across siblings. Each loan owns
        // a SLICE of it, recorded in `borrower_marginfi_borrow_shares` when
        // the residual borrowed (patched post-CPI by `process_matched_loan`).
        // Repay retires THIS loan's slice and closes THIS loan, independent
        // of the siblings.
        //
        // (The previous design keyed the close on the ACCOUNT-TOTAL
        // liability hitting zero. With ≥2 P2Pool loans that meant only the
        // one repaid with `repay_all` could ever close; the rest were
        // stranded `Active` — a follow-up repay tripped the "liability is 0"
        // guard. Closing per-loan-slice fixes that.)
        let loan_shares: u128 = {
            let l: Ref<LoanFixed> = loan.get_fixed()?;
            l.borrower_marginfi_borrow_shares
        };
        require!(
            loan_shares > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: loan has no recorded marginfi shares (already settled?)"
        )?;

        // Total liability shares across ALL sibling loans on the account.
        let account_shares_pre: u128 = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        require!(
            account_shares_pre > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: live marginfi liability is 0 (already settled?)"
        )?;

        // If this loan owns the whole remaining liability it is the LAST
        // open P2Pool loan on the account — retire it with `repay_all`
        // (marginfi `repay_all = true`) so any sub-share dust goes to
        // EXACTLY zero and nothing is orphaned. Otherwise repay only this
        // loan's slice (atom-capped), leaving the siblings intact.
        let is_last_loan: bool = loan_shares >= account_shares_pre;

        // This loan's live debt in atoms (ceil) at the bank's current
        // `liability_share_value`. Capped at the account total so a
        // recorded-share over-count (sub-atom dust a sibling pushed onto
        // this account via an earlier ceil repay) can't ask for more than
        // actually exists.
        let loan_live_atoms: u64 = crate::state::ltv::liability_shares_to_atoms_ceil(
            debt_bank.info,
            loan_shares.min(account_shares_pre),
        )?;
        require!(
            loan_live_atoms > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: this loan's live debt rounds to 0"
        )?;

        // `full_repay` retires this loan's entire slice. A partial repay
        // repays the caller's `repay_atoms`, clamped to this loan's slice
        // so a caller can't pay down a sibling's debt through this loan.
        let repay_atoms: u64 = if params.full_repay {
            loan_live_atoms
        } else {
            require!(
                params.repay_atoms > 0,
                YdeltaError::InvalidArgument,
                "repay_atoms must be > 0"
            )?;
            require!(
                params.repay_atoms <= loan_live_atoms,
                YdeltaError::InvalidArgument,
                "repay_atoms ({}) exceeds this loan's live debt ({})",
                params.repay_atoms,
                loan_live_atoms
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

        // Retire the borrower's marginfi liability. The repay CPI runs no
        // health check (repaying only improves account health), so no
        // oracle tail is needed in the account list.
        let repay_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            borrower_marginfi_account.info.clone(),
            market_signer.clone(),
            debt_bank.info.clone(),
            vault.info.clone(),
            liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];
        // `repay_all` ONLY when fully retiring the LAST loan — it zeroes
        // the whole account (incl. sub-share dust) so nothing is orphaned.
        // Every other case is an atom-capped `repay_atoms` that reduces the
        // shared liability by this loan's slice and leaves the siblings.
        // `loan_live_atoms` is the ceil of this loan's slice, so the SPL
        // transfer always covers it.
        let use_repay_all = params.full_repay && is_last_loan;
        let _shares_burned: u128 = if use_repay_all {
            MarginfiV18Adapter.repay_atoms_full(
                &repay_accounts,
                repay_atoms,
                &[market_signer_seeds],
            )?
        } else {
            MarginfiV18Adapter.repay_atoms(&repay_accounts, repay_atoms, &[market_signer_seeds])?
        };

        // ─── Attribute the burn to THIS loan ───
        //
        // Shares burned by this repay = account pre − post. This loan's
        // remaining slice = its recorded shares − burned (saturating). A
        // `full_repay` retired the whole slice (ceil atoms guarantee
        // `burned >= loan_shares`), so the loan closes; a partial closes
        // only if it happened to retire the entire slice.
        let account_shares_post: u128 = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        let burned: u128 = account_shares_pre.saturating_sub(account_shares_post);
        let loan_shares_post: u128 = loan_shares.saturating_sub(burned);
        let did_full_repay: bool = if params.full_repay {
            true
        } else {
            loan_shares_post == 0
        };

        // Keep the loan's recorded share / decorative atoms in sync with
        // its remaining slice so off-chain readers see the true residual.
        let loan_remaining_shares: u128 = if did_full_repay { 0 } else { loan_shares_post };
        let post_repay_outstanding_atoms: u64 = if did_full_repay {
            0
        } else {
            crate::state::ltv::liability_shares_to_atoms_ceil(
                debt_bank.info,
                loan_remaining_shares,
            )?
        };
        {
            let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
            let header: &mut LoanFixed =
                bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
            header.outstanding_debt_atoms = post_repay_outstanding_atoms;
            header.borrower_marginfi_borrow_shares = loan_remaining_shares;
            header.last_accrued_unix = now_unix_ts;
            if did_full_repay {
                header.state = LoanState::Repaid as u8;
            }
        }

        emit_stack(LoanRepaidLog {
            market: market_key,
            loan: *loan.info.key,
            borrower: *payer.info.key,
            repay_atoms,
            outstanding_after: post_repay_outstanding_atoms,
            full_repay: did_full_repay as u8,
            _padding: [0; 7],
        })?;

        super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;

        // Close THIS P2Pool PDA when its own slice is retired.
        if did_full_repay {
            {
                let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
                let da = get_mut_dynamic_account::<MarketFixed>(market_data);
                // Release the seat collateral that backed this loan
                // (encumbered → withdrawable). min-capped inside the helper,
                // so a legacy loan whose collateral was never encumbered (it
                // sits in `withdrawable` already) is a safe no-op.
                if collateral_atoms > 0 {
                    let collateral_shares =
                        crate::state::market_helpers::atoms_to_shares_at_snapshot(
                            collateral_atoms,
                            collateral_snapshot_fp48,
                        );
                    crate::state::market_helpers::release_loan_collateral(
                        da.dynamic,
                        borrower_seat_index,
                        collateral_shares,
                        /*returned_shares=*/ collateral_shares,
                    )?;
                }
                // One open loan closed → decrement the borrow counter
                // (paired with the per-loan bump in `match_borrower_bid`'s
                // P2Pool arm).
                let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
                seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
            }
            drop_open_loan_ref(user_account_ai, loan.info.key)?;
            super::shared::close_account_and_refund(loan.info, cranker_refund)?;
        }

        return Ok(());
    }

    // ───────────────────────── Fixed branch ──────────────────────────
    // The body's `outstanding_debt_atoms` is the authoritative debt;
    // repay atoms top up the lender side via `marginfi.deposit`.
    let outstanding_pre: u64 = body_outstanding;
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

    // Fixed loans top up the market's USDC asset via `marginfi.deposit`
    // onto `market.lender_integration_account`. The risk-profile lender
    // drains it via `claim_repayment_for_risk_profile`.
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
    let _shares_credited: u128 =
        MarginfiV18Adapter.deposit(&adapter_accounts, repay_atoms, &[market_signer_seeds])?;

    // Update the loan; on full repay, credit collateral back.
    //
    // `apply_partial_resolution` retires the `repay_atoms` slice of
    // `outstanding_debt_atoms` AND records the slice's lender / protocol
    // / curator portions on the cumulative `*_retired_atoms` trackers,
    // preserving the conservation identity (`outstanding +
    // Σretired == lender_claimable + protocol_fee + curator_fee`,
    // asserted inside the helper). Every `repay_atoms` atom was just
    // deposited into `lender_marginfi_account` by the `marginfi.deposit`
    // CPI above; the claim ix drains the cumulative claimable totals.
    let did_full_repay: bool = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        apply_partial_resolution(header, repay_atoms)?;
        let full = header.outstanding_debt_atoms == 0;
        if full {
            header.state = LoanState::Repaid as u8;
        }
        full
    };

    if did_full_repay && collateral_atoms > 0 {
        // Release the loan's collateral from the borrower seat's
        // `encumbered` bucket back to `withdrawable`, at the loan's
        // place-time snapshot so the share count is byte-symmetric with
        // the place_order encumber (the live bank value would drift the
        // payout by the share-value growth over the loan's life).
        //
        // Fixed-cross collateral now stays seat-encumbered for the
        // loan's life (matching no longer pulls it out per cross), so
        // this is a MOVE (encumbered → withdrawable) via
        // `release_loan_collateral` — NOT a net add. A net add would
        // double-credit a properly-encumbered loan (collateral counted
        // in both buckets). `release_loan_collateral` is min-capped, so
        // a legacy loan whose collateral was never encumbered (it sits
        // in `withdrawable` already) is a safe no-op — the collateral
        // simply stays withdrawable, exactly where the old program left
        // it.
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
        crate::state::market_helpers::release_loan_collateral(
            da.dynamic,
            borrower_seat_index,
            collateral_shares,
            /*returned_shares=*/ collateral_shares,
        )?;
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
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
        drop_open_loan_ref(user_account_ai, loan.info.key)?;
    }

    Ok(())
}

/// Drop the loan PDA from the borrower's `UserAccount` open-loan list.
fn drop_open_loan_ref(user_account_ai: &AccountInfo, loan_pda_key: &Pubkey) -> ProgramResult {
    let loan_pda_key = *loan_pda_key;
    let Ok(mut data) = user_account_ai.try_borrow_mut_data() else {
        return Ok(());
    };
    let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
    let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
    // Propagate: a count underflow in `remove_open_loan` is a real
    // accounting desync and must fail the repay, not be swallowed.
    remove_open_loan(fixed, dynamic, loan_pda_key)?;
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
