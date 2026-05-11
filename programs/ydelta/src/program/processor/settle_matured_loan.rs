//! Permissionless post-maturity settlement.

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

use crate::logs::{emit_stack, LoanLiquidatedLog, SecondaryStaleBidDroppedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{accrue_loan, LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE};
use crate::state::market::{get_mut_helper_seat, MarketFixed};
use crate::validation::loaders::SettleMaturedLoanContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

pub fn process_settle_matured_loan(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    // Optional 8-byte tail: `repay_atoms_max` (LE u64). 0 (or absent)
    // means "repay full outstanding".
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

    // Accrue (no-op for P2Pool) and read settlement parameters.
    let (body_outstanding_atoms, collateral_atoms, borrower_seat_index, loan_type) = {
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

    // Time gate via shared helper (so the simulation ix runs the
    // identical check). For P2Pool loans `matures_at_unix` is still the
    // `started_at + term` snapshot from place-order time — gate behaves
    // the same regardless of loan_type.
    {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::assert_past_grace_period(header, grace_period_seconds, now)?;
    }

    // Live outstanding read.
    //   Fixed: == body_outstanding_atoms (already accrued).
    //   P2Pool: liability_shares × liability_share_value (live).
    let outstanding_live_atoms: u64 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::loan_live_outstanding_atoms(
            header,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?
    };
    let _ = body_outstanding_atoms;

    require!(
        outstanding_live_atoms > 0,
        YdeltaError::InvalidArgument,
        "live outstanding is 0 — already settled"
    )?;

    // Resolve partial vs full repay amount.
    // `repay_atoms_max == 0` is the legacy "full repay" sentinel.
    // Otherwise clamp to outstanding_live_atoms.
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

    // Pro-rata collateral seize: matured-loan v1 has no oracle and no
    // bonus, so the liquidator gets the same fraction of collateral as
    // the fraction of debt they paid. Floor on division leaves at most
    // `outstanding_live_atoms - 1` atom-level dust on the borrower's
    // side, which the next caller can sweep.
    let seized_collateral_atoms: u64 = if is_full_repay {
        collateral_atoms
    } else {
        ((collateral_atoms as u128)
            .checked_mul(actual_repay_atoms as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / outstanding_live_atoms as u128) as u64
    };

    // Liquidator must hold enough debt-mint atoms for the chosen repay.
    let liquidator_debt_balance = {
        let acct_data = liquidator_debt_token.info.try_borrow_data()?;
        // SPL token Account.amount is at offset 64..72.
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

    // Transfer liquidator debt atoms into the market staging vault.
    transfer_user_to_vault(
        token_program.info,
        liquidator_debt_token.info,
        market_debt_vault.info,
        debt_mint.info,
        payer.info,
        actual_repay_atoms,
        debt_mint.mint.decimals,
    )?;

    // Debt-side CPI dispatch.
    //
    // Fixed: liquidator's atoms top up the lender side via
    // `marginfi.deposit market_debt_vault → lender_marginfi_account`.
    //
    // P2Pool: no human lender. Atoms retire the borrower's marginfi
    // liability_shares directly via `marginfi.repay_atoms
    // market_debt_vault → borrower_marginfi_account`. Without this
    // branch a residual liability would stay on the borrower's marginfi
    // account, locked against their collateral via marginfi's solvency
    // check, even after the yDelta loan body shows `outstanding == 0`.
    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
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
            let _credited_shares: u128 = MarginfiV18Adapter.deposit(
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
            let _shares_burned: u128 = MarginfiV18Adapter.repay_atoms(
                &repay_accounts,
                actual_repay_atoms,
                &[market_signer_seeds],
            )?;
            let _ = &lender_marginfi_account;
        }
    }

    // Withdraw seized collateral from borrower.marginfi into staging.
    // `withdrawn_atoms` captures the actual CPI return so the seat
    // decrement and SPL transfer downstream use the same value
    // marginfi physically moved (within ±1 of `seized_collateral_atoms`).
    let withdrawn_atoms: u64 = if seized_collateral_atoms > 0 {
        let collateral_shares: u128 = MarginfiV18Adapter
            .amount_to_asset_shares(&[collateral_bank.info.clone()], seized_collateral_atoms)?;
        // Active-balance health-check pairs must mirror the
        // borrower's marginfi balances in slot order. Fixed/OB-only
        // loans hold a single collateral asset balance; P2Pool loans
        // add a debt liability balance.
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
        let actual = MarginfiV18Adapter.withdraw(
            &withdraw_accounts,
            collateral_shares,
            &[market_signer_seeds],
        )?;

        // Transfer staged collateral to the liquidator.
        // Transfer the ACTUAL atoms marginfi moved (avoids
        // overdrawing the staging vault on a -1 atom drift).
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

    // Update the borrower seat and release proportional encumbrance.
    // Release the share count corresponding to the atoms marginfi
    // actually withdrew, scaled at the loan's place-time snapshot.
    // Using the snapshot (vs the LIVE bank value) is byte-symmetric
    // with the place_order encumber — the seat decrement matches the
    // exact share quantity that was added at match time, modulo the
    // ±1 atom drift between requested and actual withdraw. Settling
    // against `withdrawn_atoms` (the CPI return value) rather than
    // the pre-CPI `seized_collateral_atoms` keeps the seat ledger in
    // sync with marginfi's authoritative book.
    use crate::state::market_helpers::atoms_to_shares_at_snapshot;
    let collateral_snapshot_fp48: u128 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        header.borrower_collateral_share_price_snapshot_fp48
    };
    if withdrawn_atoms > 0 {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let collateral_shares_at_snapshot: u128 =
            atoms_to_shares_at_snapshot(withdrawn_atoms, collateral_snapshot_fp48);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        seat.collateral_encumbered_shares = seat
            .collateral_encumbered_shares
            .saturating_sub(collateral_shares_at_snapshot);
        if is_full_repay {
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
        }
    }

    // Liquidation protocol fee (matched to the `liquidate_loan`
    // accrual path so both settlement events feed the same protocol
    // accumulator). Reroute a configurable fraction of the LP's
    // recovery onto `accumulated_protocol_fee_atoms`; the existing
    // `protocol_fee_claim` ix collects it.
    let liquidation_protocol_atoms: u64 = if liquidation_protocol_bps > 0 {
        ((actual_repay_atoms as u128)
            .checked_mul(liquidation_protocol_bps as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / 10_000u128)
            .min(u64::MAX as u128) as u64
    } else {
        0
    };

    // Update the loan body.
    // Full repay: clamp outstanding/collateral to 0 and flip to Repaid.
    // Partial: subtract the paid/seized amounts; loan stays Active.
    // `accrue_loan` already grew lender_claimable_atoms from net_principal
    // to net_principal + accrued interest. The claim ix reads it as-is —
    // adding actual_repay_atoms here would double-count and let the
    // lender drain ~2× principal from the integration account.
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        // Reroute liquidation protocol fee from lender → protocol.
        if liquidation_protocol_atoms > 0 {
            let take = liquidation_protocol_atoms.min(header.lender_claimable_atoms);
            header.lender_claimable_atoms = header.lender_claimable_atoms.saturating_sub(take);
            header.accumulated_protocol_fee_atoms = header
                .accumulated_protocol_fee_atoms
                .checked_add(take)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        if is_full_repay {
            header.outstanding_debt_atoms = 0;
            header.collateral_atoms = 0;
            header.state = LoanState::Repaid as u8;
        } else {
            header.outstanding_debt_atoms = outstanding_live_atoms
                .checked_sub(actual_repay_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            header.collateral_atoms = collateral_atoms
                .checked_sub(seized_collateral_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    // Post-full-repay sweep: stale `SecondaryLoanSale` bids for this loan
    // must drop the moment outstanding hits 0.
    if is_full_repay {
        let loan_pda_key = *loan.info.key;
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let swept = crate::state::market_helpers::sweep_stale_secondary_bids_for_loan(
            da.fixed,
            da.dynamic,
            &loan_pda_key,
        )?;
        if swept > 0 {
            emit_stack(SecondaryStaleBidDroppedLog {
                market: market_key,
                loan_pda: loan_pda_key,
                bid_sequence: 0,
                seller_seat_index: hypertree::NIL,
                swept_by: 1, // 1 = settle_matured_loan sweep
                _padding: [0; 3],
            })?;
        }
    }

    emit_stack(LoanLiquidatedLog {
        market: market_key,
        loan: *loan.info.key,
        liquidator: *payer.info.key,
        debt_paid_atoms: actual_repay_atoms,
        collateral_seized_atoms: seized_collateral_atoms,
        liquidation_kind: 0, // matured
        is_partial: if is_full_repay { 0 } else { 1 },
        _padding: [0; 14],
    })?;

    Ok(())
}

/// SPL-transfer atoms from user's ATA to a market vault. User signs.
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

/// Signed SPL transfer: vault → recipient, signed by market_signer.
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
