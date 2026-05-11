//! Convert a P2Pool fallback loan into fixed loan exposure.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, P2PoolConvertedToFixedLog, SecondaryStaleBidDroppedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE};
use crate::state::ltv::get_required_quote_collateral_to_back_debt;
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::state::market_helpers::{
    match_p2pool_residual_against_asks, sweep_stale_secondary_bids_for_loan,
    MatchP2PoolRefinanceArgs,
};
use crate::validation::loaders::ConvertP2PoolToFixedContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ConvertP2PoolToFixedParams {
    /// Reject any ask whose `rate_bps` exceeds this value. Bounds the
    /// converted loans' interest cost up-front. Acts as the
    /// borrower-side "bid rate" for the refinance — the matching loop
    /// breaks the moment the rate-sorted asks tree exposes a maker
    /// above this cap.
    pub max_acceptable_rate_bps: u16,
}

pub fn process_convert_p2pool_to_fixed(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ConvertP2PoolToFixedParams::try_from_slice(data)?;
    let ctx = ConvertP2PoolToFixedContext::load(accounts)?;
    let ConvertP2PoolToFixedContext {
        payer,
        market,
        loan,
        borrower_marginfi_account,
        lender_marginfi_account,
        debt_bank,
        debt_liquidity_vault,
        debt_bank_lva,
        debt_oracle_ais,
        collateral_bank,
        collateral_oracle_ais,
        market_debt_vault,
        debt_mint: _,
        market_signer,
        market_signer_bump,
        token_program,
        marginfi_group,
        marginfi_program,
        cranker_refund,
    } = ctx;

    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    let (
        borrower_seat_index,
        loan_principal_atoms,
        loan_collateral_atoms,
        loan_matures_at_unix,
        loan_borrower_collateral_snapshot,
        loan_borrower_marginfi_borrow_shares,
    ) = {
        let l = loan.get_fixed()?;
        (
            l.borrower_seat_index,
            l.principal_debt_atoms,
            l.collateral_atoms,
            l.matures_at_unix,
            l.borrower_collateral_share_price_snapshot_fp48,
            l.borrower_marginfi_borrow_shares,
        )
    };
    require!(
        loan_collateral_atoms > 0,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: loan collateral_atoms is 0"
    )?;
    {
        let market_data = market.info.try_borrow_data()?;
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
        let seat = get_helper_seat(dynamic, borrower_seat_index).get_value();
        require!(
            seat.owner == *payer.info.key,
            YdeltaError::OrderNotOwnedBySigner,
            "convert_p2pool_to_fixed: signer is not the loan's borrower"
        )?;
    }
    require!(
        loan_matures_at_unix > now_unix_ts,
        YdeltaError::TermNotCompatible,
        "convert_p2pool_to_fixed: loan matures_at {} <= now {}; refinance \
         only meaningful for unmatured loans",
        loan_matures_at_unix,
        now_unix_ts
    )?;
    let term_remaining_seconds: u32 =
        u32::try_from((loan_matures_at_unix - now_unix_ts).max(0)).unwrap_or(u32::MAX);

    // Live outstanding read.
    //
    // `loan.principal_debt_atoms` is the borrower's BID amount, frozen
    // at place-order time. The actual debt the borrower owes marginfi
    // grows continuously via marginfi's variable-rate accrual on the
    // liability shares. Refinancing against the snapshot principal
    // would close the P2Pool but leave the accrued-interest portion
    // stranded on the borrower's marginfi-account, locked against
    // their collateral via marginfi's solvency check.
    //
    // So the principal cap for this refinance is the **live** marginfi
    // liability — `liability_shares × liability_share_value` — read via
    // the same `loan_live_outstanding_atoms` helper the liquidation
    // paths use.
    let live_outstanding_atoms: u64 = {
        let l = loan.get_fixed()?;
        crate::state::ltv::loan_live_outstanding_atoms(
            &l,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?
    };
    require!(
        live_outstanding_atoms > 0,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: live marginfi liability is 0 (already settled?)"
    )?;

    let debt_oracle_args = crate::validation::oracle_price_args(debt_bank.info, &debt_oracle_ais);
    let collateral_oracle_args =
        crate::validation::oracle_price_args(collateral_bank.info, &collateral_oracle_ais);
    let debt_oracle_price_fp48: u128 = MarginfiV18Adapter.oracle_price(&debt_oracle_args)?;
    let collateral_oracle_price_fp48: u128 =
        MarginfiV18Adapter.oracle_price(&collateral_oracle_args)?;
    let (_debt_asset_init, debt_liability_weight_init_fp48) =
        MarginfiV18Adapter.init_weight(&[debt_bank.info.clone()])?;
    let (collateral_asset_weight_init_fp48, _coll_liab_init) =
        MarginfiV18Adapter.init_weight(&[collateral_bank.info.clone()])?;
    let ltv_buffer_bps: u16 = market.get_fixed()?.fee_config.ltv_buffer_bps;
    let required_collateral = get_required_quote_collateral_to_back_debt(
        live_outstanding_atoms,
        debt_oracle_price_fp48,
        collateral_oracle_price_fp48,
        debt_liability_weight_init_fp48,
        collateral_asset_weight_init_fp48,
        ltv_buffer_bps,
    )?;
    require!(
        loan_collateral_atoms >= required_collateral,
        YdeltaError::CollateralBelowMatchLTV,
        "convert_p2pool_to_fixed: loan collateral {} < required {} at \
         oracle prices (live debt {})",
        loan_collateral_atoms,
        required_collateral,
        live_outstanding_atoms
    )?;

    let match_result = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        match_p2pool_residual_against_asks(
            da.fixed,
            da.dynamic,
            MatchP2PoolRefinanceArgs {
                market_pubkey: market_key,
                borrower_seat_index,
                principal_cap_atoms: live_outstanding_atoms,
                loan_collateral_atoms,
                borrower_collateral_share_price_snapshot_fp48: loan_borrower_collateral_snapshot,
                term_remaining_seconds,
                max_acceptable_rate_bps: params.max_acceptable_rate_bps,
                now_unix_ts,
            },
        )?
    };
    require!(
        match_result.total_filled_principal_atoms > 0,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: no asks crossed (rate cap or term \
         constraint left no compatible makers)"
    )?;

    let total_filled_principal = match_result.total_filled_principal_atoms;
    let total_filled_collateral = match_result.total_filled_collateral_atoms;
    let num_fills = match_result.num_fills;

    // Full conversion = matched the entire live marginfi liability.
    // Decided up-front (before the CPIs) so we can pick the right
    // marginfi.repay variant: `repay_atoms_full` retires every
    // remaining share and ignores rounding-residual dust, vs
    // `repay_atoms` which can leave sub-1-atom shares parked on the
    // borrower's marginfi-account from atoms→shares floor rounding.
    let did_full_conversion = total_filled_principal == live_outstanding_atoms;

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    // Build the [bank, …debt_oracles] active-balance health-check tail
    // for the lender side (asset-only on debt_bank).
    let mut withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        lender_marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        market_debt_vault.info.clone(),
        debt_bank_lva.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        debt_bank.info.clone(),
    ];
    for ai in &debt_oracle_ais.ais {
        withdraw_accounts.push((*ai).clone());
    }
    let withdraw_shares: u128 = MarginfiV18Adapter
        .amount_to_asset_shares(&[debt_bank.info.clone()], total_filled_principal)?;
    let withdrawn_atoms: u64 =
        MarginfiV18Adapter.withdraw(&withdraw_accounts, withdraw_shares, &[market_signer_seeds])?;

    let repay_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        borrower_marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        market_debt_vault.info.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    // Always atom-capped repay (no `repay_all = true`). Mid-tx
    // marginfi-bank accrual between our `live_outstanding_atoms` read
    // and this CPI bumps `liability_share_value` upward, so a true
    // `repay_all` would ask for more atoms than the staging vault
    // holds and the internal SPL transfer fails. The atom cap leaves
    // a sub-atom residual share count that's economically irrelevant
    // (worst case ≤ 1 atom of liability per refinance) but not 0.
    let borrow_shares_burned: u128 =
        MarginfiV18Adapter.repay_atoms(&repay_accounts, withdrawn_atoms, &[market_signer_seeds])?;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.open_borrow_count = seat
            .open_borrow_count
            .checked_add(num_fills)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    let new_borrower_marginfi_borrow_shares: u128 = loan_borrower_marginfi_borrow_shares
        .checked_sub(borrow_shares_burned)
        .unwrap_or(0);

    if did_full_conversion {
        // Full conversion: close the P2Pool PDA. Decrement
        // open_borrow_count by 1 to retire the original P2Pool slot
        // (the `+= num_fills` above already covers the new Fixed
        // loans). Sweep stale secondary bids — though P2Pool loans
        // shouldn't have rested secondary bids per the secondary-loan
        // type gate, the sweep is cheap defense in depth and matches
        // the close pattern in `liquidate_loan` / `settle_matured_loan`.
        {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
        }

        let loan_pda_key = *loan.info.key;
        {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let swept = sweep_stale_secondary_bids_for_loan(da.fixed, da.dynamic, &loan_pda_key)?;
            if swept > 0 {
                emit_stack(SecondaryStaleBidDroppedLog {
                    market: market_key,
                    loan_pda: loan_pda_key,
                    bid_sequence: 0,
                    seller_seat_index: hypertree::NIL,
                    swept_by: 3, // 3 = convert_p2pool_to_fixed sweep
                    _padding: [0; 3],
                })?;
            }
        }

        close_p2pool_loan_pda(&loan, cranker_refund)?;
    } else {
        // Partial conversion: shrink the P2Pool body in place. Loan
        // stays Active and `LoanType::P2Pool`. The remaining marginfi
        // liability stays on the borrower's marginfi-account and the
        // canonical residual is `liability_shares × liability_share_value`
        // (always re-read live on subsequent ixs).
        //
        // Body fields under partial conversion:
        //   - `principal_debt_atoms`: snapshot of the BID amount, kept
        //     decremented by what was refinanced. Display-only.
        //   - `outstanding_debt_atoms`: tracks `live_outstanding -
        //     total_filled` so off-chain readers see the post-repay
        //     residual without doing the live math.
        //   - `borrower_marginfi_borrow_shares`: live shares minus
        //     what marginfi.repay_atoms burned.
        //   - `collateral_atoms`: shrunk by the pro-rata split that
        //     went to the new Fixed loans.
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.principal_debt_atoms = loan_principal_atoms.saturating_sub(total_filled_principal);
        header.outstanding_debt_atoms = live_outstanding_atoms
            .checked_sub(total_filled_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        header.collateral_atoms = loan_collateral_atoms
            .checked_sub(total_filled_collateral)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        header.borrower_marginfi_borrow_shares = new_borrower_marginfi_borrow_shares;
        header.last_accrued_unix = now_unix_ts;
        header.state = LoanState::Active as u8;
        debug_assert_eq!(header.loan_type, LoanType::P2Pool as u8);
    }

    emit_stack(P2PoolConvertedToFixedLog {
        market: market_key,
        loan: *loan.info.key,
        borrower: *payer.info.key,
        new_lender_seat_index: hypertree::NIL,
        _pad0: [0; 4],
        matched_principal_atoms: total_filled_principal,
        borrow_shares_burned,
        new_lender_rate_bps: 0,
        did_full_fill_ask: if did_full_conversion { 1 } else { 0 },
        _padding: [0; 13],
    })?;

    Ok(())
}

/// Close a P2Pool `LoanFixed` PDA on full conversion. Mirror of the
/// close path in `claim_repayment::close_loan_pda` — zero the
/// discriminator, optionally refund lamports to the cranker who paid
/// the original PDA rent. If `cranker_refund` is missing or doesn't
/// match `loan.created_by`, lamports stay stranded on the (now-zeroed)
/// account; a future sweeper ix can recover them.
fn close_p2pool_loan_pda<'a, 'info>(
    loan: &crate::validation::YdeltaAccountInfo<'a, 'info, LoanFixed>,
    cranker_refund: Option<&'a AccountInfo<'info>>,
) -> ProgramResult {
    let loan_info = loan.info;
    let created_by: Pubkey = loan.get_fixed()?.created_by;

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
        // Mismatched `created_by`: silently skip the refund. Loan
        // still closes (data zeroed); lamports stay stranded.
    }

    Ok(())
}
