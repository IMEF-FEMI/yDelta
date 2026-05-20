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

pub fn process_settle_matured_loan(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    // Optional 8-byte tail: `repay_atoms_max` (LE u64). Empty data means
    // "repay full outstanding". Reject a 1-7 byte tail rather than
    // silently treating a malformed instruction as a full repay.
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

    // Drip-grief floor — mirror `liquidate_loan`. Without it a keeper
    // could call with `repay_atoms_max = 1` repeatedly: every call emits
    // events and charges `liquidation_protocol_bps` while the borrower's
    // collateral stays locked and the loan never closes. Force sub-floor
    // residuals into the full-repay branch so settlement happens once.
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

    // Pro-rata collateral seize: matured-loan settlement uses no oracle
    // and no bonus, so the liquidator gets the same fraction of
    // collateral as the fraction of debt they paid. Floor on division
    // leaves at most
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

    // Atoms staged into `market_debt_vault`.
    //
    // For a Fixed settle, or a P2Pool *partial* settle, this is exactly
    // `actual_repay_atoms` — the slice the keeper commits to.
    //
    // For a P2Pool *full* settle the keeper must drive the borrower's
    // marginfi liability to exactly zero, which requires the
    // `repay_all = true` CPI (see the P2Pool match arm below).
    // `repay_all` retires the WHOLE live liability — and marginfi
    // lazily accrues `liability_share_value` upward ON the repay CPI's
    // entry, so the live liability at CPI time exceeds the pre-CPI
    // `outstanding_live_atoms` read. We therefore over-stage a small
    // accrual headroom so the `repay_all` SPL transfer is always
    // covered; marginfi pulls exactly the live liability and the
    // unconsumed headroom is swept back to the keeper after the CPI
    // (see `sweep_back_atoms` below). A plain atom-capped `repay_atoms`
    // cannot be used for the full close: under-staging leaves a residual
    // liability (loan can't close), and over-staging is rejected by
    // marginfi as a deposit on a repay-only operation (`OperationRepayOnly`).
    let p2pool_full_close: bool = loan_type == LoanType::P2Pool && is_full_repay;
    // Headroom: 2% of the slice plus a 64-atom absolute floor. Generously
    // covers the post-maturity accrual delta on any realistic loan; the
    // unconsumed remainder is refunded, so an over-estimate costs nothing.
    let staged_atoms: u64 = if p2pool_full_close {
        actual_repay_atoms
            .saturating_add(actual_repay_atoms / 50)
            .saturating_add(64)
    } else {
        actual_repay_atoms
    };

    // Liquidator must hold enough debt-mint atoms for the staged amount.
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

    // Transfer liquidator debt atoms into the market staging vault.
    transfer_user_to_vault(
        token_program.info,
        liquidator_debt_token.info,
        market_debt_vault.info,
        debt_mint.info,
        payer.info,
        staged_atoms,
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
            // CPI dispatch keyed on partial vs full settle.
            //
            // **Partial** settle: atom-capped `repay_atoms` (no
            // `repay_all`). The keeper repays exactly `actual_repay_atoms`
            // (`< outstanding_live_atoms`, so strictly below the live
            // liability); marginfi retires that slice and a residual
            // liability legitimately remains. The loan stays `Active` for
            // a follow-up settle.
            //
            // **Full** settle: `repay_atoms_full` (`repay_all = true`).
            // marginfi lazily accrues `liability_share_value` upward on
            // the repay CPI's entry, so the live liability at CPI time
            // exceeds the pre-CPI `outstanding_live_atoms` read. An
            // atom-capped `repay_atoms` of the (stale) read therefore
            // CANNOT zero the liability — it under-repays and strands a
            // residual; and over-staging is rejected by marginfi as a
            // deposit on a repay-only op (`OperationRepayOnly`). Only
            // `repay_all = true` retires the WHOLE live liability to
            // exactly zero. `staged_atoms` carries an accrual headroom
            // above `actual_repay_atoms` so the `repay_all` SPL transfer
            // is always covered; marginfi pulls exactly the live
            // liability and the unconsumed headroom is swept back to the
            // keeper below.
            let _shares_burned: u128 = if is_full_repay {
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

            // Sweep the unconsumed staging headroom back to the keeper.
            // `repay_all` pulls exactly the live liability from
            // `market_debt_vault`; any `staged_atoms − live_liability`
            // remainder is the keeper's own over-stake and is refunded so
            // settling a loan never silently costs the keeper atoms.
            let vault_remainder: u64 = {
                let d = market_debt_vault.info.try_borrow_data()?;
                u64::from_le_bytes(d[64..72].try_into().expect("slice is 8 bytes"))
            };
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
    let post_repay_liability_shares: Option<u128> = if loan_type == LoanType::P2Pool {
        Some(crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?)
    } else {
        None
    };
    let did_full_repay: bool = match post_repay_liability_shares {
        Some(shares) => shares == 0,
        None => is_full_repay,
    };
    let post_repay_outstanding_atoms: Option<u64> = match post_repay_liability_shares {
        Some(0) => Some(0),
        Some(shares) => Some(crate::state::ltv::liability_shares_to_atoms_ceil(
            debt_bank.info,
            shares,
        )?),
        None => None,
    };

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
        if did_full_repay {
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
        }
    }

    // Liquidation protocol fee (matched to the `liquidate_loan`
    // accrual path so both settlement events feed the same protocol
    // accumulator). Reroute a configurable fraction of the LP's
    // recovery onto `accumulated_protocol_fee_atoms`; the existing
    // `protocol_fee_claim` ix collects it.
    let liquidation_protocol_atoms: u64 =
        if loan_type == LoanType::Fixed && liquidation_protocol_bps > 0 {
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
    // Partial: retire the paid/seized slice; loan stays Active.
    //
    // The FULL `actual_repay_atoms` were deposited into
    // `lender_marginfi_account` by the Fixed-branch CPI above.
    //
    // For Fixed loans `apply_partial_resolution` retires the
    // `actual_repay_atoms` slice (shrinks `outstanding`, grows the
    // cumulative `principal_retired_atoms`) so the conservation
    // identity `outstanding + retired == lender_claimable + protocol_fee
    // + curator_fee` holds. The liquidation protocol fee is then a
    // HAIRCUT on the lender's recovery — there is no genuine
    // unattributed surplus on a settlement — taken strictly from
    // `lender_claimable_atoms` (capped at it) and moved onto
    // `accumulated_protocol_fee_atoms`. That is a pure transfer between
    // two claimable buckets, so the identity is preserved and every
    // protocol-fee atom stays physically backed in
    // `lender_marginfi_account` (drained by `protocol_fee_claim`).
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        if loan_type == LoanType::P2Pool {
            header.borrower_marginfi_borrow_shares = post_repay_liability_shares.unwrap_or(0);
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
            // Fixed: retire the debt slice.
            apply_partial_resolution(header, actual_repay_atoms)?;
            // Liquidation protocol fee — haircut on lender recovery.
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
            // Conservation identity must hold after every resolution.
            assert_loan_conservation(header)?;
        }
    }

    emit_stack(LoanLiquidatedLog {
        market: market_key,
        loan: *loan.info.key,
        liquidator: *payer.info.key,
        debt_paid_atoms: actual_repay_atoms,
        collateral_seized_atoms: seized_collateral_atoms,
        liquidation_kind: 0, // matured
        is_partial: if did_full_repay { 0 } else { 1 },
        _padding: [0; 14],
    })?;

    if loan_type == LoanType::P2Pool && did_full_repay {
        super::shared::close_account_and_refund(loan.info, cranker_refund)?;
    }

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
