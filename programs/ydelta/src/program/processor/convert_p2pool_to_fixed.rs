//! Convert a P2Pool fallback loan into fixed loan exposure.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, P2PoolConvertedToFixedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE};
use crate::state::ltv::get_required_quote_collateral_to_back_debt;
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::state::market_helpers::{match_p2pool_residual_against_asks, MatchP2PoolRefinanceArgs};
use crate::state::vault::{
    accrue_risk_profile, get_mut_helper_risk_profile, read_bank_asset_share_value_fp48,
    GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
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
        // Consumed by the loader for the market-expansion rent top-up
        // CPI; the processor calls `expand_market_to_free_blocks` which
        // signs the transfer with the payer directly.
        system_program: _,
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
        global_vault,
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
        _,
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
    let (ltv_buffer_bps, debt_mint_decimals, collateral_mint_decimals): (u16, u8, u8) = {
        let m = market.get_fixed()?;
        (
            m.fee_config.ltv_buffer_bps,
            m.debt_mint_decimals,
            m.collateral_mint_decimals,
        )
    };
    let required_collateral = get_required_quote_collateral_to_back_debt(
        live_outstanding_atoms,
        debt_oracle_price_fp48,
        collateral_oracle_price_fp48,
        debt_liability_weight_init_fp48,
        collateral_asset_weight_init_fp48,
        ltv_buffer_bps,
        debt_mint_decimals,
        collateral_mint_decimals,
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

    // ─── Reserve MatchedLoan blocks for the matching pass ───
    //
    // Each cross against a vault ask allocates one `MatchedLoan` block
    // from the market free list and frees nothing (vault asks are
    // unbounded standing quotes — never removed by matching). The
    // refinance can cross each resting ask at most once, so reserve one
    // block per resting ask (the `+ 1` is slack, mirroring the
    // `place_order` pre-expansion). Without this, a convert crossing
    // 2+ asks fails with `AccountDataTooSmall`.
    {
        let ask_count = super::shared::count_resting_asks(&market)?;
        super::shared::expand_market_to_free_blocks(payer.info, &market, ask_count + 1)?;
    }

    let fee_floor_bps: u16 = market.get_fixed()?.fee_config.protocol_fee_bps_floor;

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
                fee_floor_bps,
                now_unix_ts,
                // Per-cross LTV-gate inputs — the same oracle
                // prices / init weights / decimals already snapshotted
                // for the aggregate market-level check above.
                debt_oracle_price_fp48,
                collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48,
                ltv_buffer_bps,
                debt_mint_decimals,
                collateral_mint_decimals,
            },
            Some(global_vault),
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
    // The borrower's NEW fixed debt is sized from
    // `total_filled_principal` (the converted `MatchedLoan` nodes),
    // while the OLD variable debt destroyed is whatever liability the
    // repay CPI below actually retires off the borrower's marginfi
    // account. The borrower must never end up owing more fixed debt
    // than the variable debt that was retired.
    //
    // The matcher fills up to `principal_cap_atoms == live_outstanding_atoms`.
    // Two cases:
    //
    //   * FULL refinance (`total_filled == live_outstanding`) — the
    //     whole P2Pool liability is being converted. A `repay_all` CPI
    //     retires the borrower's entire liability cleanly; the retired
    //     debt is then trivially `>= total_filled_principal`. An
    //     atom-capped over-repay can't be used here: marginfi rejects a
    //     repay that exceeds the live liability (`OperationRepayOnly`),
    //     and the liability-share floor of an exact-amount repay would
    //     under-retire by ~1 atom.
    //
    //   * PARTIAL refinance (`total_filled < live_outstanding`) — only
    //     part of the liability is converted. The repay amount is
    //     `total_filled_principal` rounded UP to a whole number of
    //     liability shares plus a small cushion (`repay_target_atoms`),
    //     so the floored repay still retires `>= total_filled_principal`.
    //     The cushion stays strictly below the live liability, so
    //     marginfi never sees an over-repay.
    //
    // In both cases the withdraw shares are rounded UP so the staging
    // vault holds enough atoms to fund the repay.
    let is_full_refinance: bool = total_filled_principal == live_outstanding_atoms;
    let repay_target_atoms: u64 = MarginfiV18Adapter
        .liability_atoms_to_fully_cover(&[debt_bank.info.clone()], total_filled_principal)?;
    let withdraw_shares: u128 = MarginfiV18Adapter
        .amount_to_asset_shares_ceil(&[debt_bank.info.clone()], repay_target_atoms)?;
    let withdrawn_atoms: u64 =
        MarginfiV18Adapter.withdraw(&withdraw_accounts, withdraw_shares, &[market_signer_seeds])?;
    // The withdraw must yield enough atoms to fund a repay that retires
    // `>= total_filled_principal` of the borrower's liability.
    // `repay_target_atoms` carries a cushion over `total_filled_principal`
    // (see `liability_atoms_to_fully_cover`); the marginfi `withdraw` may
    // return one atom under the share-derived expectation, so the bound
    // is `total_filled_principal` (the cushion absorbs the drift).
    require!(
        withdrawn_atoms >= total_filled_principal,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: withdrawn atoms {} < new fixed debt {} — \
         insufficient to retire the converted liability",
        withdrawn_atoms,
        total_filled_principal
    )?;

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
    let borrow_shares_burned: u128 = if is_full_refinance {
        // Full refinance: `repay_all = true` retires the entire
        // liability. The atom cap (`withdrawn_atoms`) still bounds the
        // internal SPL transfer; mid-tx accrual bumps the liability a
        // sub-atom amount, covered by the `repay_target_atoms` cushion.
        MarginfiV18Adapter.repay_atoms_full(
            &repay_accounts,
            withdrawn_atoms,
            &[market_signer_seeds],
        )?
    } else {
        // Partial refinance: atom-capped repay of the cushioned target,
        // clamped to the (stale) live liability so it can never exceed
        // the borrower's actual liability — which has only accrued
        // UPWARD since the pre-CPI read — and trip marginfi's
        // over-repay guard. The clamp still retires `>= total_filled`
        // because `total_filled < live_outstanding_atoms` on this
        // branch.
        let partial_repay_atoms = repay_target_atoms.min(live_outstanding_atoms);
        MarginfiV18Adapter.repay_atoms(
            &repay_accounts,
            partial_repay_atoms,
            &[market_signer_seeds],
        )?
    };

    // Gate the PDA close on the POST-CPI live liability.
    //
    // marginfi's `liability_share_value` accrues between the
    // `live_outstanding_atoms` read and the `repay_atoms` CPI, so a
    // pre-CPI `total_filled_principal == live_outstanding_atoms` equality
    // can hold while a sub-atom residual liability still sits on the
    // borrower's marginfi account. Closing the P2Pool PDA on that stale
    // comparison would orphan the residual: an untracked debt with no
    // PDA, un-repayable and un-liquidatable through yDelta, silently
    // encumbering the borrower's collateral on marginfi.
    //
    // So re-read `liability_shares` AFTER the repay CPI and only treat
    // the conversion as "full" (→ close the PDA) when the live
    // liability is exactly zero. Any residual leaves the loan Active.
    let post_repay_liability_shares: u128 = crate::state::ltv::read_borrower_liability_shares(
        borrower_marginfi_account.info,
        debt_bank.info.key,
    )?;
    let did_full_conversion: bool = post_repay_liability_shares == 0;

    // Invariant — the variable debt actually retired must cover the
    // NEW fixed debt minted against the borrower. `borrow_shares_burned`
    // is the liability-share count the repay CPI retired; its atom value
    // (floored at the live liability-share value) is the variable debt
    // destroyed, and the new fixed debt is `total_filled_principal`. The
    // repay-target up-rounding above guarantees `retired >=
    // total_filled_principal`; the check turns any residual
    // marginfi-side rounding drift into a hard fault rather than silent
    // phantom debt on the borrower. (A full conversion — zero residual
    // liability — trivially retired the whole live debt.)
    if !did_full_conversion {
        let retired_liability_atoms: u64 = MarginfiV18Adapter
            .liability_shares_to_atoms_floor(&[debt_bank.info.clone()], borrow_shares_burned)?;
        require!(
            retired_liability_atoms >= total_filled_principal,
            YdeltaError::InvalidArgument,
            "convert_p2pool_to_fixed: retired variable debt {} < new fixed \
             debt {} — borrower would owe phantom debt",
            retired_liability_atoms,
            total_filled_principal
        )?;
    }

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.open_borrow_count = seat
            .open_borrow_count
            .checked_add(num_fills)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // Post-CPI live shares — the canonical residual. `borrow_shares_burned`
    // tracks what this CPI retired; the loan-body field is kept in sync
    // with the live marginfi value rather than the (potentially stale)
    // pre-CPI `borrower_marginfi_borrow_shares - burned` arithmetic.
    let new_borrower_marginfi_borrow_shares: u128 = post_repay_liability_shares;

    if did_full_conversion {
        // Full conversion: close the P2Pool PDA. Decrement
        // open_borrow_count by 1 to retire the original P2Pool slot
        // (the `+= num_fills` above already covers the new Fixed
        // loans).
        {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
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
        //   - `outstanding_debt_atoms`: decorative for P2Pool — set to
        //     the live post-CPI liability so off-chain readers see the
        //     true residual. `loan_live_outstanding_atoms` re-derives it
        //     authoritatively from `liability_shares` on every ix.
        //   - `borrower_marginfi_borrow_shares`: the post-CPI live
        //     liability shares (re-read after the repay CPI).
        //   - `collateral_atoms`: shrunk by the pro-rata split that
        //     went to the new Fixed loans.
        let post_repay_outstanding_atoms: u64 = crate::state::ltv::liability_shares_to_atoms_ceil(
            debt_bank.info,
            post_repay_liability_shares,
        )?;
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.principal_debt_atoms = loan_principal_atoms.saturating_sub(total_filled_principal);
        header.outstanding_debt_atoms = post_repay_outstanding_atoms;
        header.collateral_atoms = loan_collateral_atoms
            .checked_sub(total_filled_collateral)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        header.borrower_marginfi_borrow_shares = new_borrower_marginfi_borrow_shares;
        header.last_accrued_unix = now_unix_ts;
        header.state = LoanState::Active as u8;
        debug_assert_eq!(header.loan_type, LoanType::P2Pool as u8);
    }

    // ─── Per-profile vault bookkeeping for the crossed asks ───
    //
    // `match_p2pool_residual_against_asks` bumped each crossed profile's
    // `encumbered_in_orders_atoms` at accept time. The emitted nodes are
    // stamped `VAULT_PRESETTLED`, so the cranker (`process_matched_loan`)
    // SKIPS `do_vault_settle`. The convert processor must therefore run
    // the `encumbered_in_orders → deployed_principal` transition and the
    // weighted-rate folds itself — exactly as `do_vault_settle` would —
    // otherwise `encumbered_in_orders_atoms` stays permanently inflated
    // and the crossed profile's idle pool is frozen for good.
    {
        let curator_fee_bps: u16 = market.get_fixed()?.fee_config.curator_fee_bps;
        let share_value_fp48 = read_bank_asset_share_value_fp48(debt_bank.info);

        let data: &mut RefMut<&mut [u8]> = &mut global_vault.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let root = {
            let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
            header.risk_profiles_root_index
        };

        for cross in &match_result.crosses {
            let probe = RiskProfile::new_empty(cross.lender_profile_id, Pubkey::default(), 1, 1);
            let profile_idx = {
                let tree = RiskProfileTreeReadOnly::new(dynamic, root, NIL);
                tree.lookup_index(&probe)
            };
            require!(
                profile_idx != NIL,
                YdeltaError::VaultProfileNotFound,
                "convert_p2pool_to_fixed: crossed profile {} not found on global_vault",
                cross.lender_profile_id
            )?;
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            // Crystallise yield at the OLD weighted rate before folding
            // in the new loan's contribution.
            accrue_risk_profile(profile, now_unix_ts, share_value_fp48)?;
            let principal = cross.filled_principal_atoms;
            profile.deployed_principal_atoms = profile
                .deployed_principal_atoms
                .checked_add(principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let weighted_delta: u128 = (principal as u128)
                .checked_mul(cross.lender_rate_bps as u128)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_weighted_rate_bps = profile
                .total_weighted_rate_bps
                .checked_add(weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let net_weighted_delta: u128 = weighted_delta
                .checked_mul((crate::state::loan::BPS_PER_UNIT as u128) - curator_fee_bps as u128)
                .and_then(|x| x.checked_div(crate::state::loan::BPS_PER_UNIT as u128))
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_weighted_net_rate_bps = profile
                .total_weighted_net_rate_bps
                .checked_add(net_weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            // `saturating_sub` mirrors `do_vault_settle`: the matching
            // accept bumped `encumbered_in_orders_atoms` by exactly this
            // `principal`.
            profile.encumbered_in_orders_atoms =
                profile.encumbered_in_orders_atoms.saturating_sub(principal);
        }
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

/// Close a P2Pool `LoanFixed` PDA on full conversion: zero the data and
/// refund the rent lamports to the keeper who paid the PDA rent.
/// `cranker_refund` is bound by the loader to `loan.created_by`, so the
/// refund is unconditional — the keeper is always reimbursed.
fn close_p2pool_loan_pda<'a, 'info>(
    loan: &crate::validation::YdeltaAccountInfo<'a, 'info, LoanFixed>,
    cranker_refund: &'a AccountInfo<'info>,
) -> ProgramResult {
    let loan_info = loan.info;

    {
        let mut data: RefMut<&mut [u8]> = loan_info.try_borrow_mut_data()?;
        for byte in data.iter_mut() {
            *byte = 0;
        }
    }

    let lamports = loan_info.lamports();
    **loan_info.try_borrow_mut_lamports()? = 0;
    **cranker_refund.try_borrow_mut_lamports()? = cranker_refund
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    Ok(())
}
