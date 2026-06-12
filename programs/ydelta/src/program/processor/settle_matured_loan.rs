//! `SettleMaturedLoan` — permissionless keeper-driven settlement of a loan
//! past maturity + grace period. Keeper supplies debt-token funds, marginfi
//! repays/deposits, and the proportional share of collateral is seized to the
//! keeper's collateral token account. Supports partial settlement (must clear
//! at least max(1% of outstanding, 1000 atoms)) and full settlement; full
//! settlement performs the same close-out as `repay` for Fixed loans
//! (lender seat credit, encumbrance + open_lend_count decrement, risk-profile
//! NAV/weighted-rate/pending-claim/curator-fee updates, loan PDA closed and
//! rent refunded to the original cranker).

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

use crate::logs::{emit_stack, LoanLiquidatedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{
    accrue_loan, apply_partial_resolution, assert_loan_conservation, LoanFixed, LoanState,
    LoanType, LOAN_FIXED_SIZE,
};
use crate::state::market::{get_mut_helper_seat, MarketFixed};
use crate::state::market_helpers::atoms_to_shares_at_snapshot;
use crate::validation::loaders::SettleMaturedLoanContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

/// Settle a matured loan past its grace period. `data` is empty or an 8-byte
/// little-endian `repay_atoms_max`; `0` (or empty) means settle in full.
/// Permissionless; the keeper provides the debt-side atoms and receives
/// collateral pro-rata to repaid principal.
pub fn process_settle_matured_loan(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    require!(
        data.is_empty() || data.len() >= 8,
        YdeltaError::InvalidArgument,
        "settle_matured_loan: instruction data must be empty or >= 8 bytes"
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
    let (grace_period_seconds, liquidation_protocol_bps): (u32, u16) = {
        let m = market.get_fixed()?;
        (
            m.fee_config.grace_period_seconds,
            m.fee_config.liquidation_protocol_bps,
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

    {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::assert_past_grace_period(header, grace_period_seconds, now)?;
    }

    let outstanding_live_atoms: u64 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::loan_live_outstanding_atoms(
            header,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?
    };

    require!(
        outstanding_live_atoms > 0,
        YdeltaError::InvalidArgument,
        "live outstanding is 0 — already settled"
    )?;

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
            "outstanding {} below partial settlement floor ({}); \
             keeper must full-repay sub-floor residuals",
            outstanding_live_atoms,
            MIN_PARTIAL_REPAY_FLOOR_ATOMS
        )?;
        let min_partial_repay = (outstanding_live_atoms / 100).max(MIN_PARTIAL_REPAY_FLOOR_ATOMS);
        require!(
            actual_repay_atoms >= min_partial_repay,
            YdeltaError::InvalidArgument,
            "partial settlement must repay >= max(1% of outstanding, {} atoms): {} of {}",
            MIN_PARTIAL_REPAY_FLOOR_ATOMS,
            actual_repay_atoms,
            outstanding_live_atoms
        )?;
    }

    let seized_collateral_atoms: u64 = if is_full_repay {
        collateral_atoms
    } else {
        crate::math::mul_div_u64(
            collateral_atoms,
            actual_repay_atoms,
            outstanding_live_atoms,
            false,
        )?
    };

    let p2pool_full_close: bool = loan_type == LoanType::P2Pool && is_full_repay;

    let staged_atoms: u64 = if p2pool_full_close {
        actual_repay_atoms
            .saturating_add(actual_repay_atoms / 50)
            .saturating_add(64)
    } else {
        actual_repay_atoms
    };

    {
        let acct_data = liquidator_debt_token.info.try_borrow_data()?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&acct_data[64..72]);
        let bal = u64::from_le_bytes(buf);
        require!(
            bal >= staged_atoms,
            YdeltaError::LiquidatorPaymentInsufficient,
            "liquidator has {} atoms < staged {}",
            bal,
            staged_atoms
        )?;
    }

    let debt_vault_pre_stage: u64 = {
        let d = market_debt_vault.info.try_borrow_data()?;
        u64::from_le_bytes(d[64..72].try_into().expect("slice is 8 bytes"))
    };

    transfer_user_to_vault(
        token_program.info,
        liquidator_debt_token.info,
        market_debt_vault.info,
        debt_mint.info,
        payer.info,
        staged_atoms,
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
    // (seat credit). 0 on P2Pool (no deposit-back happens — atoms flow
    // into the borrower account via marginfi.repay).
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
                    staged_atoms,
                    &[market_signer_seeds],
                )?
            } else {
                MarginfiV18Adapter.repay_atoms(
                    &repay_accounts,
                    actual_repay_atoms,
                    &[market_signer_seeds],
                )?
            };

            let vault_now: u64 = {
                let d = market_debt_vault.info.try_borrow_data()?;
                u64::from_le_bytes(d[64..72].try_into().expect("slice is 8 bytes"))
            };
            let vault_remainder: u64 = vault_now.saturating_sub(debt_vault_pre_stage);
            if vault_remainder > 0 {
                transfer_signed(
                    token_program.info,
                    market_debt_vault.info,
                    liquidator_debt_token.info,
                    debt_mint.info,
                    market_signer,
                    &market_key,
                    market_signer_bump,
                    vault_remainder,
                    debt_mint.mint.decimals,
                )?;
            }
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
        // repay share-rounding dust. Treat as fully settled.
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

    let withdrawn_atoms: u64 = if seized_collateral_atoms > 0 {
        let collateral_shares: u128 = MarginfiV18Adapter
            .amount_to_asset_shares(&[collateral_bank.info.clone()], seized_collateral_atoms)?;

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
        let (actual, _shares_burned) = MarginfiV18Adapter.withdraw(
            &withdraw_accounts,
            collateral_shares,
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
            actual,
            collateral_mint.mint.decimals,
        )?;
        actual
    } else {
        0
    };

    let collateral_snapshot_fp48: crate::math::Fp48 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        header.borrower_collateral_share_price_snapshot_fp48
    };
    if withdrawn_atoms > 0 {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let collateral_shares_at_snapshot: u128 =
            atoms_to_shares_at_snapshot(withdrawn_atoms, collateral_snapshot_fp48)?;
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.collateral_encumbered_shares = seat
            .collateral_encumbered_shares
            .saturating_sub(collateral_shares_at_snapshot);
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
        collateral_seized_atoms: seized_collateral_atoms,
        liquidation_kind: 0,
        is_partial: if did_full_repay { 0 } else { 1 },
        _padding: [0; 14],
    })?;

    // ===== Fixed-loan close-out (mirrors repay's full-repay close path) =====
    if loan_type == LoanType::Fixed && fixed_credited_shares > 0 {
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
                    "settle_matured_loan: Fixed full-repay requires global_vault account"
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
                "settle_matured_loan: profile_id {} not found on global_vault",
                lender_profile_id
            )?;
            let profile = crate::state::vault::get_mut_helper_risk_profile(dynamic, profile_idx)
                .get_mut_value();
            let share_value_fp48 =
                crate::state::vault::read_bank_asset_share_value_fp48(debt_bank.info)?;
            crate::state::vault::accrue_risk_profile(profile, now, share_value_fp48)?;

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
