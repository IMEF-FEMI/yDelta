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
        system_program: _,
        borrower_marginfi_account,
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
        global_vault_integration_account,
        global_vault_signer,
        global_vault_signer_bump,
        cranker_refund,
    } = ctx;

    let market_key = *market.info.key;
    let global_vault_key = *global_vault.info.key;
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

    // Per-loan live liability via accrue-prediction. We compute the
    // marginfi LSV projected to `now_unix_ts` using marginfi's own accrue
    // formula, then multiply by THIS loan's recorded share count (clamped
    // to the borrower's account total for defense). Marginfi's LSV is
    // monotonically non-decreasing and the on-chain CPI uses the same
    // slot's Clock, so this prediction matches the LSV marginfi will see
    // when our repay CPI runs — no cushion required.
    let pre_loan_shares: u128 = loan.get_fixed()?.borrower_marginfi_borrow_shares;
    let pre_account_shares: u128 = crate::state::ltv::read_borrower_liability_shares(
        borrower_marginfi_account.info,
        debt_bank.info.key,
    )?;
    let this_loan_shares: u128 = pre_loan_shares.min(pre_account_shares);
    let predicted_lsv_fp48: u128 = MarginfiV18Adapter.calc_projected_liability_share_value_fp48(
        debt_bank.info,
        marginfi_group.info,
        now_unix_ts,
    )?;
    let live_outstanding_atoms: u64 = MarginfiV18Adapter
        .liability_shares_to_atoms_ceil_at_lsv(this_loan_shares, predicted_lsv_fp48)?;
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
            Some(global_vault.info),
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

    // Convert is must-full-fill: if the orderbook didn't fully refinance
    // the live debt, abort. Borrower stays on variable rate, retries
    // later (or with a higher rate cap). No partial fixed/variable
    // mixed-rate state allowed.
    require!(
        total_filled_principal == live_outstanding_atoms,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: matched {} < live debt {} — orderbook \
         lacked sufficient cumulative liquidity at acceptable rates",
        total_filled_principal,
        live_outstanding_atoms
    )?;

    // Withdraw the exact predicted live debt from the vault. The predict-
    // ed LSV matches what marginfi will use inside the repay CPI (same
    // slot Clock + same bank.last_update), so this amount is enough to
    // burn exactly `this_loan_shares` of borrower liability without
    // dipping into siblings or stranding residual on this loan.
    let vault_signer_bump_arr = [global_vault_signer_bump];
    let vault_signer_seeds: &[&[u8]] = &[
        crate::state::vault::GLOBAL_VAULT_SIGNER_SEED,
        global_vault_key.as_ref(),
        &vault_signer_bump_arr,
    ];
    let mut vault_withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        global_vault_integration_account.info.clone(),
        global_vault_signer.clone(),
        debt_bank.info.clone(),
        market_debt_vault.info.clone(),
        debt_bank_lva.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        debt_bank.info.clone(),
    ];
    for ai in &debt_oracle_ais.ais {
        vault_withdraw_accounts.push((*ai).clone());
    }
    // Dispatch on whether this loan is the only one in the borrower's
    // per-market marginfi account.
    //
    // SINGLE-LOAN PATH (this_loan_shares == pre_account_shares):
    //   Over-fund by 1 atom and use marginfi.repay_atoms_full
    //   (repay_all=true). Marginfi computes the exact post-accrue
    //   liability internally, pulls only that much from market_debt_vault
    //   (the `amount` parameter is IGNORED when repay_all=true), and
    //   burns all account shares (= this loan, no siblings). Any unused
    //   atoms (0 or 1) stay in market_debt_vault as dust for the next
    //   op on this market to consume — cheaper than a refund CPI.
    //
    // MULTI-LOAN PATH (siblings exist):
    //   Pay exactly `live_outstanding_atoms` via marginfi.repay_atoms.
    //   Never use repay_all (would drain siblings). Accept ≤1 atom of
    //   orphan dust on this loan's marginfi residual as the cost of
    //   sibling isolation.
    const ACCRUE_SAFETY_ATOMS: u64 = 1;
    let this_loan_is_account: bool = this_loan_shares == pre_account_shares;
    let atoms_to_fund: u64 = if this_loan_is_account {
        live_outstanding_atoms
            .checked_add(ACCRUE_SAFETY_ATOMS)
            .ok_or(ProgramError::ArithmeticOverflow)?
    } else {
        live_outstanding_atoms
    };
    let withdraw_shares: u128 = MarginfiV18Adapter
        .amount_to_asset_shares_ceil(&[debt_bank.info.clone()], atoms_to_fund)?;
    let (funded_atoms, _shares_burned) = MarginfiV18Adapter.withdraw(
        &vault_withdraw_accounts,
        withdraw_shares,
        &[vault_signer_seeds],
    )?;
    require!(
        funded_atoms >= live_outstanding_atoms,
        YdeltaError::InvalidArgument,
        "convert_p2pool_to_fixed: vault funded {} < live debt {}",
        funded_atoms,
        live_outstanding_atoms
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
    let _shares_burned: u128 = if this_loan_is_account {
        MarginfiV18Adapter.repay_atoms_full(&repay_accounts, atoms_to_fund, &[market_signer_seeds])?
    } else {
        MarginfiV18Adapter.repay_atoms(
            &repay_accounts,
            live_outstanding_atoms,
            &[market_signer_seeds],
        )?
    };

    // Per-loan burn attribution (C-2): compute how many shares were
    // burned and attribute to THIS loan only. Sub-atom dust residue
    // (post_loan_shares > 0 but < 1 raw atom in atom-form) is treated
    // as fully cleared — that's marginfi's fp48 rounding noise, not
    // real debt.
    let post_account_shares: u128 = crate::state::ltv::read_borrower_liability_shares(
        borrower_marginfi_account.info,
        debt_bank.info.key,
    )?;
    let burned: u128 = pre_account_shares.saturating_sub(post_account_shares);
    let post_loan_shares: u128 = this_loan_shares.saturating_sub(burned);
    // ≤1 atom of residual variable debt is marginfi's accrue-during-repay
    // rounding cost (the share burn ceil leaves at most 1 atom when LSV
    // moved by ≤1 fp48 bit between our prediction and the CPI). Treat as
    // logically-full conversion: close the PDA and leave the marginfi-side
    // dust as protocol noise (will be cleaned up by the borrower's next
    // marginfi operation on the same bank).
    let post_loan_atoms_floor: u64 = MarginfiV18Adapter
        .liability_shares_to_atoms_floor(&[debt_bank.info.clone()], post_loan_shares)?;
    let did_full_conversion: bool = post_loan_atoms_floor <= 1;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.open_borrow_count = seat
            .open_borrow_count
            .checked_add(num_fills)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    if did_full_conversion {
        // Full conversion. Close the P2Pool PDA, decrement
        // open_borrow_count by 1 to retire the original P2Pool slot
        // (the `+= num_fills` above already covers the new Fixed
        // loans), and release any pro-rata-floored collateral residual.
        {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
        }

        // Release the collateral the original P2Pool bid encumbered that
        // the new Fixed loans don't carry forward. With ≥2 crosses the
        // sum of per-cross floored collateral can be slightly less than
        // the original loan collateral; without releasing the diff it
        // stays encumbered on the seat forever (no PDA left to release).
        let unfilled_collateral_atoms: u64 =
            loan_collateral_atoms.saturating_sub(total_filled_collateral);
        if unfilled_collateral_atoms > 0 {
            let release_shares = crate::state::market_helpers::atoms_to_shares_at_snapshot(
                unfilled_collateral_atoms,
                loan_borrower_collateral_snapshot,
            )?;
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            crate::state::market_helpers::release_loan_collateral(
                da.dynamic,
                borrower_seat_index,
                release_shares,
                /*returned_shares=*/ release_shares,
            )?;
        }

        close_p2pool_loan_pda(&loan, cranker_refund)?;
    } else {
        // Logically-full match (`total_filled == live_outstanding_atoms`)
        // but marginfi's repay rounding left a sub-atom residual on the
        // borrower's marginfi liability. The P2Pool PDA stays alive
        // tracking just this residual; the next op (repay/liquidate/
        // settle) cleans it up.
        //
        // We DON'T orphan the residual by closing the PDA, and we DON'T
        // hard-fail (which would be UX-hostile since prediction is
        // probabilistically exact). The collateral split has already
        // been moved to the new Fixed loans, so the P2Pool body's
        // collateral_atoms reflects only what's still pledged against
        // the residual liability.
        let post_repay_outstanding_atoms: u64 = crate::state::ltv::liability_shares_to_atoms_ceil(
            debt_bank.info,
            post_loan_shares,
        )?;
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        header.principal_debt_atoms = loan_principal_atoms.saturating_sub(total_filled_principal);
        header.outstanding_debt_atoms = post_repay_outstanding_atoms;
        header.collateral_atoms = loan_collateral_atoms
            .checked_sub(total_filled_collateral)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        header.borrower_marginfi_borrow_shares = post_loan_shares;
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
        let share_value_fp48 = read_bank_asset_share_value_fp48(debt_bank.info)?;

        let data: &mut RefMut<&mut [u8]> = &mut global_vault.info.try_borrow_mut_data()?;
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
                .ok_or(crate::program::YdeltaError::MathOverflow)?;
            profile.total_weighted_rate_bps = profile
                .total_weighted_rate_bps
                .checked_add(weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let net_weighted_delta: u128 = crate::math::mul_div(
                weighted_delta,
                (crate::state::loan::BPS_PER_UNIT as u128) - curator_fee_bps as u128,
                crate::state::loan::BPS_PER_UNIT as u128,
                false,
            )?;
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
        borrow_shares_burned: burned,
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
