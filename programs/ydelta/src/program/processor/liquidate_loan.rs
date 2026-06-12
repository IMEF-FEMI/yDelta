//! `LiquidateLoan` instruction. Permissionless when current LTV
//! breaches the marginfi maintenance threshold. Liquidator pays debt
//! atoms in and seizes collateral plus a keeper bonus. Full repay
//! closes the loan PDA; partial repay decrements live state in place.

use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::logs::{emit_stack, BadDebtLog, LoanLiquidatedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{
    accrue_loan, apply_partial_resolution, assert_loan_conservation, LoanFixed, LoanState,
    LoanType, LOAN_FIXED_SIZE,
};
use crate::state::ltv::get_required_quote_collateral_to_back_debt;
use crate::state::market::{get_mut_helper_seat, MarketFixed};
use crate::state::market_helpers::atoms_to_shares_at_snapshot;
use crate::validation::loaders::SettleMaturedLoanContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

/// Liquidate a loan that breaches its LTV gate. Instruction data is
/// either empty (full repay) or 8 bytes (`u64 repay_atoms_max`).
/// Partial repays require `>= max(1% of outstanding, 1000 atoms)`. On
/// `did_full_repay` the PDA closes and rent refunds to the cranker.
pub fn process_liquidate_loan(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    require!(
        data.is_empty() || data.len() >= 8,
        YdeltaError::InvalidArgument,
        "liquidate_loan: instruction data must be empty (full repay) or >= 8 bytes \
         (u64 repay_atoms_max); rejecting truncated payload that would silently \
         coerce to full liquidation"
    )?;
    let repay_atoms_max: u64 = if data.len() >= 8 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&data[0..8]);
        u64::from_le_bytes(buf)
    } else {
        0
    };

    let SettleMaturedLoanContext {
        payer,
        market,
        loan,
        liquidator_debt_token,
        liquidator_collateral_token,
        market_debt_vault,
        market_collateral_vault,
        market_signer,
        market_signer_bump,
        lender_marginfi_account,
        borrower_marginfi_account,
        debt_bank,
        collateral_bank,
        debt_liquidity_vault,
        collateral_liquidity_vault,
        collateral_bank_lva,
        debt_oracle_ais,
        collateral_oracle_ais,
        debt_mint,
        collateral_mint,
        token_program,
        marginfi_group,
        marginfi_program,
        cranker_refund,
        global_vault,
    } = SettleMaturedLoanContext::load(accounts)?;

    let market_key = *market.info.key;
    let now: i64 = Clock::get()?.unix_timestamp;
    let (
        grace_period_seconds,
        bonus_bps,
        liquidation_protocol_bps,
        debt_mint_decimals,
        collateral_mint_decimals,
    ) = {
        let m = market.get_fixed()?;
        (
            m.fee_config.grace_period_seconds,
            m.fee_config.liquidation_keeper_bps,
            m.fee_config.liquidation_protocol_bps,
            m.debt_mint_decimals,
            m.collateral_mint_decimals,
        )
    };

    let (_, collateral_atoms, borrower_seat_index, loan_type) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "loan already in Repaid state"
        )?;
        accrue_loan(header, now, grace_period_seconds)?;
        (
            header.outstanding_debt_atoms,
            header.collateral_atoms,
            header.borrower_seat_index,
            header.loan_type()?,
        )
    };

    let outstanding_live_atoms: u64 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::loan_live_outstanding_atoms(
            header,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?
    };

    let debt_oracle_args = crate::validation::oracle_price_args(debt_bank.info, &debt_oracle_ais);
    let collateral_oracle_args =
        crate::validation::oracle_price_args(collateral_bank.info, &collateral_oracle_ais);
    crate::state::ltv::assert_ltv_breach(
        outstanding_live_atoms,
        collateral_atoms,
        debt_bank.info,
        &debt_oracle_args,
        collateral_bank.info,
        &collateral_oracle_args,
        debt_mint_decimals,
        collateral_mint_decimals,
    )?;

    let debt_price_fp48 =
        crate::math::Fp48::from_raw(MarginfiV18Adapter.oracle_price(&debt_oracle_args)?);
    let collateral_price_fp48 =
        crate::math::Fp48::from_raw(MarginfiV18Adapter.oracle_price(&collateral_oracle_args)?);

    let actual_repay_atoms: u64 = if repay_atoms_max == 0 {
        outstanding_live_atoms
    } else {
        repay_atoms_max.min(outstanding_live_atoms)
    };
    require!(
        actual_repay_atoms > 0,
        YdeltaError::InvalidArgument,
        "actual_repay_atoms is 0"
    )?;

    let is_full_repay: bool = actual_repay_atoms == outstanding_live_atoms;
    if !is_full_repay {
        const MIN_PARTIAL_REPAY_FLOOR_ATOMS: u64 = 1_000;
        require!(
            outstanding_live_atoms >= MIN_PARTIAL_REPAY_FLOOR_ATOMS,
            YdeltaError::InvalidArgument,
            "outstanding {} below partial liquidation floor ({}); \
             liquidator must full-repay sub-floor residuals",
            outstanding_live_atoms,
            MIN_PARTIAL_REPAY_FLOOR_ATOMS
        )?;
        let min_partial_repay = (outstanding_live_atoms / 100).max(MIN_PARTIAL_REPAY_FLOOR_ATOMS);
        require!(
            actual_repay_atoms >= min_partial_repay,
            YdeltaError::InvalidArgument,
            "partial liquidation must repay >= max(1% of outstanding, {} atoms): {} of {}",
            MIN_PARTIAL_REPAY_FLOOR_ATOMS,
            actual_repay_atoms,
            outstanding_live_atoms
        )?;
    }

    const FP48_ONE: crate::math::Fp48 = crate::math::Fp48::ONE;
    let repay_value_in_collateral_atoms = get_required_quote_collateral_to_back_debt(
        actual_repay_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        FP48_ONE,
        FP48_ONE,
        0,
        debt_mint_decimals,
        collateral_mint_decimals,
    )?;
    let CollateralSplit {
        liquidator_seizes_atoms,

        surplus_atoms: _,
        bad_debt_gap_atoms,
        ..
    } = compute_collateral_split(repay_value_in_collateral_atoms, collateral_atoms, bonus_bps)?;

    let liquidator_debt_balance = {
        let acct_data = liquidator_debt_token.info.try_borrow_data()?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&acct_data[64..72]);
        u64::from_le_bytes(buf)
    };
    require!(
        liquidator_debt_balance >= actual_repay_atoms,
        YdeltaError::LiquidatorPaymentInsufficient,
        "liquidator has {} atoms < repay {}",
        liquidator_debt_balance,
        actual_repay_atoms
    )?;

    transfer_user_to_vault(
        token_program.info,
        liquidator_debt_token.info,
        market_debt_vault.info,
        debt_mint.info,
        payer.info,
        actual_repay_atoms,
        debt_mint.mint.decimals,
    )?;

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    let (pre_loan_shares, pre_account_shares): (u128, u128) = if loan_type == LoanType::P2Pool {
        let pre_loan = {
            let loan_data = loan.info.try_borrow_data()?;
            let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
            header.borrower_marginfi_borrow_shares
        };
        let pre_account = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        (pre_loan, pre_account)
    } else {
        (0, 0)
    };
    // Capture marginfi.deposit's credited_shares for Fixed-loan close-out
    // (seat credit). 0 on P2Pool path (no deposit-back happens — atoms
    // flow into the borrower account via marginfi.repay).
    let mut fixed_credited_shares: u128 = 0;
    match loan_type {
        LoanType::Fixed => {
            let deposit_accounts: Vec<AccountInfo> = vec![
                marginfi_group.info.clone(),
                lender_marginfi_account.info.clone(),
                market_signer.clone(),
                debt_bank.info.clone(),
                market_debt_vault.info.clone(),
                debt_liquidity_vault.info.clone(),
                token_program.info.clone(),
                marginfi_program.info.clone(),
            ];
            fixed_credited_shares = MarginfiV18Adapter.deposit(
                &deposit_accounts,
                actual_repay_atoms,
                &[market_signer_seeds],
            )?;
        }
        LoanType::P2Pool => {
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
            let this_loan_is_account = pre_loan_shares == pre_account_shares;
            let use_marginfi_repay_all = is_full_repay && this_loan_is_account;
            let _shares_burned: u128 = if use_marginfi_repay_all {
                MarginfiV18Adapter.repay_atoms_full(
                    &repay_accounts,
                    actual_repay_atoms,
                    &[market_signer_seeds],
                )?
            } else {
                MarginfiV18Adapter.repay_atoms(
                    &repay_accounts,
                    actual_repay_atoms,
                    &[market_signer_seeds],
                )?
            };
        }
    }
    let (post_loan_shares, did_full_repay, post_repay_outstanding_atoms): (
        Option<u128>,
        bool,
        Option<u64>,
    ) = if loan_type == LoanType::P2Pool {
        let post_account_shares = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        let burned = pre_account_shares.saturating_sub(post_account_shares);
        let post_loan_shares = pre_loan_shares.saturating_sub(burned);
        // ≤1 atom of residual on this loan is marginfi's accrue-during-
        // repay share-rounding dust. Treat as fully liquidated.
        let post_atoms_floor: u64 = MarginfiV18Adapter
            .liability_shares_to_atoms_floor(&[debt_bank.info.clone()], post_loan_shares)?;
        let did_full = post_atoms_floor <= 1;
        let post_atoms = if did_full {
            Some(0u64)
        } else {
            Some(crate::state::ltv::liability_shares_to_atoms_ceil(
                debt_bank.info,
                post_loan_shares,
            )?)
        };
        (Some(post_loan_shares), did_full, post_atoms)
    } else {
        (None, is_full_repay, None)
    };

    let liquidator_seizes_shares: u128 = MarginfiV18Adapter
        .amount_to_asset_shares(&[collateral_bank.info.clone()], liquidator_seizes_atoms)?;

    let mut withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        borrower_marginfi_account.info.clone(),
        market_signer.clone(),
        collateral_bank.info.clone(),
        market_collateral_vault.info.clone(),
        collateral_bank_lva.clone(),
        collateral_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        collateral_bank.info.clone(),
    ];
    for ai in &collateral_oracle_ais.ais {
        withdraw_accounts.push((*ai).clone());
    }
    if loan_type == LoanType::P2Pool {
        withdraw_accounts.push(debt_bank.info.clone());
        for ai in &debt_oracle_ais.ais {
            withdraw_accounts.push((*ai).clone());
        }
    }
    let (withdrawn_atoms, _shares_burned) = MarginfiV18Adapter.withdraw(
        &withdraw_accounts,
        liquidator_seizes_shares,
        &[market_signer_seeds],
    )?;

    transfer_signed(
        token_program.info,
        market_collateral_vault.info,
        liquidator_collateral_token.info,
        collateral_mint.info,
        market_signer,
        &market_key,
        market_signer_bump,
        withdrawn_atoms,
        collateral_mint.mint.decimals,
    )?;

    let collateral_snapshot_fp48: crate::math::Fp48 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        header.borrower_collateral_share_price_snapshot_fp48
    };

    let surplus_atoms_actual: u64 = if did_full_repay {
        collateral_atoms.saturating_sub(withdrawn_atoms)
    } else {
        0
    };
    let surplus_shares: u128 = if surplus_atoms_actual > 0 {
        atoms_to_shares_at_snapshot(surplus_atoms_actual, collateral_snapshot_fp48)?
    } else {
        0
    };
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();

        let release_shares: u128 = if did_full_repay {
            atoms_to_shares_at_snapshot(collateral_atoms, collateral_snapshot_fp48)?
        } else {
            atoms_to_shares_at_snapshot(withdrawn_atoms, collateral_snapshot_fp48)?
        };
        seat.collateral_encumbered_shares = seat
            .collateral_encumbered_shares
            .saturating_sub(release_shares);
        if surplus_shares > 0 {
            seat.collateral_withdrawable_shares = seat
                .collateral_withdrawable_shares
                .checked_add(surplus_shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        if did_full_repay {
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
        }
    }

    let liquidation_protocol_atoms: u64 =
        if loan_type == LoanType::Fixed && liquidation_protocol_bps > 0 {
            crate::math::mul_div_u64(
                actual_repay_atoms,
                liquidation_protocol_bps as u64,
                10_000u64,
                false,
            )?
        } else {
            0
        };

    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        if loan_type == LoanType::P2Pool {
            header.borrower_marginfi_borrow_shares = post_loan_shares.unwrap_or(0);
            if did_full_repay {
                header.outstanding_debt_atoms = 0;
                header.collateral_atoms = 0;
                header.state = LoanState::Repaid as u8;
            } else {
                header.outstanding_debt_atoms =
                    post_repay_outstanding_atoms.ok_or(ProgramError::ArithmeticOverflow)?;
                header.collateral_atoms = collateral_atoms.saturating_sub(withdrawn_atoms);
            }
        } else {
            apply_partial_resolution(header, actual_repay_atoms)?;

            if liquidation_protocol_atoms > 0 {
                let fee: u64 = liquidation_protocol_atoms.min(header.lender_claimable_atoms);
                header.lender_claimable_atoms = header
                    .lender_claimable_atoms
                    .checked_sub(fee)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
                header.accumulated_protocol_fee_atoms = header
                    .accumulated_protocol_fee_atoms
                    .checked_add(fee)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }

            if bad_debt_gap_atoms > 0 && header.accumulated_curator_fee_atoms > 0 {
                let clawback: u64 = header.accumulated_curator_fee_atoms;
                header.accumulated_curator_fee_atoms = 0;
                header.lender_claimable_atoms = header
                    .lender_claimable_atoms
                    .checked_add(clawback)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }

            if did_full_repay {
                header.collateral_atoms = 0;
                header.state = LoanState::Repaid as u8;
            } else {
                header.collateral_atoms = collateral_atoms.saturating_sub(withdrawn_atoms);
            }

            assert_loan_conservation(header)?;
        }
    }

    emit_stack(LoanLiquidatedLog {
        market: market_key,
        loan: *loan.info.key,
        liquidator: *payer.info.key,
        debt_paid_atoms: actual_repay_atoms,
        collateral_seized_atoms: liquidator_seizes_atoms,
        liquidation_kind: 1,
        is_partial: if did_full_repay { 0 } else { 1 },
        _padding: [0; 14],
    })?;
    if bad_debt_gap_atoms > 0 {
        let debt_atoms_remaining: u64 = if did_full_repay {
            0
        } else {
            post_repay_outstanding_atoms
                .unwrap_or_else(|| outstanding_live_atoms.saturating_sub(actual_repay_atoms))
        };
        emit_stack(BadDebtLog {
            market: market_key,
            loan: *loan.info.key,
            gap_collateral_atoms: bad_debt_gap_atoms,
            debt_atoms_remaining,
            _padding: [0; 16],
        })?;
    }

    // ===== Fixed-loan close-out (mirrors repay's full-repay close path) =====
    if loan_type == LoanType::Fixed && fixed_credited_shares > 0 {
        // Snapshot the FINAL (post-mutation) loan body. After the loan
        // body section above ran, accumulated_protocol_fee_atoms has the
        // liquidation_protocol_atoms bump applied; accumulated_curator_fee_atoms
        // reflects the bad-debt clawback; lender_claimable_atoms is final.
        let (
            lender_seat_index,
            lender_profile_id,
            loan_principal,
            loan_lender_rate,
            loan_curator_fee_bps,
            loan_started_at,
            loan_lender_claimable,
            loan_accumulated_curator_fee_atoms,
            loan_accumulated_protocol_fee_atoms,
        ): (
            hypertree::DataIndex,
            u8,
            u64,
            u16,
            u16,
            i64,
            u64,
            u64,
            u64,
        ) = {
            let loan_data = loan.info.try_borrow_data()?;
            let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
            (
                header.lender_seat_index,
                header.lender_profile_id,
                header.principal_debt_atoms,
                header.lender_rate_bps,
                header.curator_fee_bps_snapshot,
                header.started_at_unix,
                header.lender_claimable_atoms,
                header.accumulated_curator_fee_atoms,
                header.accumulated_protocol_fee_atoms,
            )
        };

        // protocol_fee_shares only computed/applied at FULL close.
        let protocol_fee_shares: u128 =
            if did_full_repay && loan_accumulated_protocol_fee_atoms > 0 {
                MarginfiV18Adapter.amount_to_asset_shares(
                    &[debt_bank.info.clone()],
                    loan_accumulated_protocol_fee_atoms,
                )?
            } else {
                0
            };
        let lender_claim_shares: u128 = fixed_credited_shares.saturating_sub(protocol_fee_shares);

        // Apply seat + market accumulators.
        {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            if protocol_fee_shares > 0 {
                da.fixed.accumulated_protocol_fee_shares = da
                    .fixed
                    .accumulated_protocol_fee_shares
                    .checked_add(protocol_fee_shares)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
            require!(
                lender_seat_index != hypertree::NIL,
                YdeltaError::InvalidArgument,
                "Fixed loan has no lender_seat_index — should be set at promotion"
            )?;
            let lender_seat =
                get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
            if lender_claim_shares > 0 {
                lender_seat.debt_withdrawable_shares = lender_seat
                    .debt_withdrawable_shares
                    .checked_add(lender_claim_shares)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
            if did_full_repay {
                // Vault asks take no seat-level debt encumbrance (the
                // profile's atom counters are the lender ledger); just
                // retire the open-lend counter stamped at fill time.
                lender_seat.open_lend_count = lender_seat
                    .open_lend_count
                    .checked_sub(1)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }

        if did_full_repay {
            let global_vault = global_vault.as_ref().ok_or_else(|| {
                solana_program::msg!(
                    "liquidate_loan: Fixed full-repay requires global_vault account"
                );
                YdeltaError::IncorrectAccount
            })?;
            let vault_data: &mut RefMut<&mut [u8]> =
                &mut global_vault.info.try_borrow_mut_data()?;
            let (fixed_bytes, dynamic) =
                vault_data.split_at_mut(crate::state::GLOBAL_VAULT_FIXED_SIZE);
            let header: &crate::state::vault::GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
            let probe = crate::state::vault::RiskProfile::new_empty(
                lender_profile_id,
                Pubkey::default(),
                1,
                1,
            );
            let profile_idx = {
                let tree = crate::state::vault::RiskProfileTreeReadOnly::new(
                    dynamic,
                    header.risk_profiles_root_index,
                    hypertree::NIL,
                );
                <crate::state::vault::RiskProfileTreeReadOnly as hypertree::HyperTreeReadOperations>::lookup_index(&tree, &probe)
            };
            require!(
                profile_idx != hypertree::NIL,
                YdeltaError::VaultProfileNotFound,
                "liquidate_loan: profile_id {} not found on global_vault",
                lender_profile_id
            )?;
            let profile = crate::state::vault::get_mut_helper_risk_profile(dynamic, profile_idx)
                .get_mut_value();
            let share_value_fp48 =
                crate::state::vault::read_bank_asset_share_value_fp48(debt_bank.info)?;
            crate::state::vault::accrue_risk_profile(profile, now, share_value_fp48)?;

            // Per-loan weighted-rate decrements.
            let weighted_delta: u128 = (loan_principal as u128)
                .checked_mul(loan_lender_rate as u128)
                .ok_or(YdeltaError::MathOverflow)?;
            profile.total_weighted_rate_bps = profile
                .total_weighted_rate_bps
                .checked_sub(weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let net_weighted_delta: u128 = crate::math::mul_div(
                weighted_delta,
                (crate::state::loan::BPS_PER_UNIT as u128) - loan_curator_fee_bps as u128,
                crate::state::loan::BPS_PER_UNIT as u128,
                false,
            )?;
            profile.total_weighted_net_rate_bps = profile
                .total_weighted_net_rate_bps
                .checked_sub(net_weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.deployed_principal_atoms = profile
                .deployed_principal_atoms
                .checked_sub(loan_principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            // Realized-vs-estimated reconciliation (same as repay).
            let loan_lifetime: u128 = (now.saturating_sub(loan_started_at)).max(0) as u128;
            let yield_denom: u128 = (crate::state::loan::BPS_PER_UNIT as u128)
                .checked_mul(crate::state::loan::SECONDS_PER_YEAR as u128)
                .ok_or(YdeltaError::MathOverflow)?;
            let estimated_accrued_atoms: u128 =
                crate::math::mul_div(net_weighted_delta, loan_lifetime, yield_denom, false)?;
            let realized_net: i128 =
                (loan_lender_claimable as i128) - (loan_principal as i128);
            if realized_net >= 0 {
                profile.total_principal_atoms = profile
                    .total_principal_atoms
                    .checked_add(realized_net as u64)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                profile.total_principal_atoms =
                    profile.total_principal_atoms.saturating_sub((-realized_net) as u64);
            }
            let assets_delta: i128 = realized_net - (estimated_accrued_atoms as i128);
            if assets_delta >= 0 {
                profile.total_assets_atoms = profile
                    .total_assets_atoms
                    .checked_add(assets_delta as u64)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                profile.total_assets_atoms =
                    profile.total_assets_atoms.saturating_sub((-assets_delta) as u64);
            }
            crate::state::vault::restore_assets_principal_invariant(profile);

            // pending_claim + curator fee accumulators.
            profile.pending_claim_atoms = profile
                .pending_claim_atoms
                .checked_add(loan_lender_claimable)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            if loan_accumulated_curator_fee_atoms > 0 {
                profile.accumulated_curator_fee_atoms = profile
                    .accumulated_curator_fee_atoms
                    .checked_add(loan_accumulated_curator_fee_atoms)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }
    }

    if did_full_repay {
        super::shared::close_account_and_refund(loan.info, cranker_refund)?;
    }

    Ok(())
}

/// Outcome of splitting a loan's collateral between the liquidator,
/// the borrower (surplus), and the bad-debt gap, given a bonus rate.
pub struct CollateralSplit {
    /// Collateral atoms the liquidator takes (capped by available).
    pub liquidator_seizes_atoms: u64,
    /// Excess collateral returned to the borrower after the seize.
    pub surplus_atoms: u64,
    /// Shortfall when collateral cannot cover the seize target.
    pub bad_debt_gap_atoms: u64,
    /// Bonus portion of the seize, derived from `bonus_bps`.
    pub bonus_atoms: u64,
}

/// Compute the collateral split for a liquidation. `bonus_bps` is
/// bounded by `MAX_LIQUIDATION_KEEPER_BPS`; the seize target is
/// `debt_value_in_collateral_atoms + bonus`, capped at available
/// collateral, with any shortfall reported as `bad_debt_gap_atoms`.
pub fn compute_collateral_split(
    debt_value_in_collateral_atoms: u64,
    collateral_atoms: u64,
    bonus_bps: u16,
) -> Result<CollateralSplit, ProgramError> {
    require!(
        bonus_bps <= crate::program::processor::fee_config_helpers::MAX_LIQUIDATION_KEEPER_BPS,
        YdeltaError::InvalidArgument,
        "bonus_bps {} exceeds MAX_LIQUIDATION_KEEPER_BPS ({})",
        bonus_bps,
        crate::program::processor::fee_config_helpers::MAX_LIQUIDATION_KEEPER_BPS,
    )?;
    let bonus_atoms: u64 = crate::math::mul_div_u64(
        debt_value_in_collateral_atoms,
        bonus_bps as u64,
        10_000u64,
        false,
    )?;
    let total_seize_target: u64 = debt_value_in_collateral_atoms.saturating_add(bonus_atoms);
    let liquidator_seizes_atoms: u64 = collateral_atoms.min(total_seize_target);
    let bad_debt_gap_atoms: u64 = total_seize_target.saturating_sub(collateral_atoms);
    let surplus_atoms: u64 = collateral_atoms.saturating_sub(liquidator_seizes_atoms);
    Ok(CollateralSplit {
        liquidator_seizes_atoms,
        surplus_atoms,
        bad_debt_gap_atoms,
        bonus_atoms,
    })
}

#[cfg(test)]
mod split_tests {
    use super::compute_collateral_split;

    #[test]
    fn over_collateralized_with_zero_bonus() {
        let s = compute_collateral_split(80, 100, 0).unwrap();
        assert_eq!(s.liquidator_seizes_atoms, 80);
        assert_eq!(s.surplus_atoms, 20);
        assert_eq!(s.bad_debt_gap_atoms, 0);
        assert_eq!(s.bonus_atoms, 0);
    }

    #[test]
    fn over_collateralized_with_bonus() {
        let s = compute_collateral_split(80, 100, 750).unwrap();
        assert_eq!(s.bonus_atoms, 6);
        assert_eq!(s.liquidator_seizes_atoms, 86);
        assert_eq!(s.surplus_atoms, 14);
        assert_eq!(s.bad_debt_gap_atoms, 0);
    }

    #[test]
    fn exactly_collateralized_no_surplus() {
        let s = compute_collateral_split(100, 100, 0).unwrap();
        assert_eq!(s.liquidator_seizes_atoms, 100);
        assert_eq!(s.surplus_atoms, 0);
        assert_eq!(s.bad_debt_gap_atoms, 0);
    }

    #[test]
    fn under_collateralized_bad_debt() {
        let s = compute_collateral_split(100, 80, 0).unwrap();
        assert_eq!(s.liquidator_seizes_atoms, 80);
        assert_eq!(s.surplus_atoms, 0);
        assert_eq!(s.bad_debt_gap_atoms, 20);
    }

    #[test]
    fn under_collateralized_with_bonus_increases_gap() {
        let s = compute_collateral_split(100, 90, 1_000).unwrap();
        assert_eq!(s.bonus_atoms, 10);
        assert_eq!(s.liquidator_seizes_atoms, 90);
        assert_eq!(s.surplus_atoms, 0);
        assert_eq!(s.bad_debt_gap_atoms, 20);
    }

    #[test]
    fn rejects_bonus_bps_above_full_scale() {
        assert!(compute_collateral_split(100, 100, 10_001).is_err());
        assert!(compute_collateral_split(100, 100, u16::MAX).is_err());
    }

    #[test]
    fn rejects_bonus_bps_above_max_liquidation_keeper_cap() {
        use crate::program::processor::fee_config_helpers::MAX_LIQUIDATION_KEEPER_BPS;
        assert!(compute_collateral_split(50, 200, MAX_LIQUIDATION_KEEPER_BPS + 1).is_err());
        assert!(compute_collateral_split(50, 200, 10_000).is_err());
    }

    #[test]
    fn accepts_bonus_bps_at_keeper_cap() {
        use crate::program::processor::fee_config_helpers::MAX_LIQUIDATION_KEEPER_BPS;
        let s = compute_collateral_split(50, 200, MAX_LIQUIDATION_KEEPER_BPS).unwrap();
        // 50 × 5000 / 10000 = 25 bonus atoms.
        assert_eq!(s.bonus_atoms, 25);
        assert_eq!(s.liquidator_seizes_atoms, 75);
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_user_to_vault<'info>(
    token_program: &AccountInfo<'info>,
    src: &AccountInfo<'info>,
    dst: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            src.key,
            mint.key,
            dst.key,
            owner.key,
            &[],
            amount,
            decimals,
        )?;
        invoke(
            &ix,
            &[
                src.clone(),
                mint.clone(),
                dst.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            src.key,
            dst.key,
            owner.key,
            &[],
            amount,
        )?;
        invoke(
            &ix,
            &[
                src.clone(),
                dst.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_signed<'info>(
    token_program: &AccountInfo<'info>,
    src: &AccountInfo<'info>,
    dst: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    market_signer: &AccountInfo<'info>,
    market_key: &Pubkey,
    market_signer_bump: u8,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    let bytes = market_key.to_bytes();
    let bump_arr = [market_signer_bump];
    let signer_seeds: &[&[u8]] = &[MARKET_SIGNER_SEED, &bytes, &bump_arr];
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            src.key,
            mint.key,
            dst.key,
            market_signer.key,
            &[],
            amount,
            decimals,
        )?;
        invoke_signed(
            &ix,
            &[
                src.clone(),
                mint.clone(),
                dst.clone(),
                market_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            src.key,
            dst.key,
            market_signer.key,
            &[],
            amount,
        )?;
        invoke_signed(
            &ix,
            &[
                src.clone(),
                dst.clone(),
                market_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    }
}
