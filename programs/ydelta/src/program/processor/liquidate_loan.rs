//! Permissionless LTV-gated liquidation. When the available collateral
//! cannot cover `debt_value_in_collateral + bonus`, the liquidator takes
//! all collateral and a `BadDebtLog` is emitted with the gap.

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

pub fn process_liquidate_loan(
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
        cranker_refund,
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

    // Accrue (no-op for P2Pool) and read settlement parameters from the
    // loan body. Live outstanding for P2Pool comes from marginfi below
    // — `outstanding_debt_atoms` on the body is decorative there.
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

    // Live outstanding read.
    //   Fixed: == body_outstanding_atoms (already accrued).
    //   P2Pool: liability_shares × liability_share_value (live), since
    //   accrue_loan is a no-op for P2Pool and marginfi has been
    //   compounding the debt at its variable APR.
    let outstanding_live_atoms: u64 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        crate::state::ltv::loan_live_outstanding_atoms(
            header,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?
    };

    // Oracle-priced LTV gate. The position is liquidatable iff its
    // collateral would fail marginfi's maintenance solvency check at
    // current prices. Shared helper so the simulation ix
    // (`CheckLtvLiquidatable`) runs the identical gate.
    //
    // Partial liquidations are bounded by design. Every
    // `liquidate_loan` call is its own transaction that re-loads the
    // loan from chain and re-runs THIS gate against the post-partial
    // `outstanding_live_atoms` / `collateral_atoms` — so a keeper can
    // only take a further bonus-bearing slice while the loan is STILL in
    // LTV breach.
    //
    // Keeper-bonus bound. The bonus is a fixed
    // bps of debt-value-seized, but the summed bonus across a sequence
    // of partials is NOT exactly equal to a single full liquidation's
    // bonus: it is path-dependent. Interest accrues between partials
    // (each slice re-reads `outstanding_live_atoms`), and each slice
    // re-reads the debt/collateral oracles, so the per-slice
    // debt-value-in-collateral is computed at a different price and a
    // slightly larger outstanding than a one-shot full liquidation
    // would see. The summed bonus can therefore slightly EXCEED a
    // single-full-liquidation bonus. It remains bounded — the
    // min-partial-repay floor below caps the number of bonus events,
    // and each event's bonus is a fixed bps of that slice's
    // debt-value — so there is no unbounded-extraction path; the bound
    // is "≈ one full liquidation, up to the cross-partial accrual and
    // oracle-drift residual," not an exact equality.
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
    // Re-read for the bonus / collateral-split computation below — the
    // gate consumed the slices but we need the prices again. Cheap
    // (cached oracle accounts).
    let debt_price_fp48: u128 = MarginfiV18Adapter.oracle_price(&debt_oracle_args)?;
    let collateral_price_fp48: u128 = MarginfiV18Adapter.oracle_price(&collateral_oracle_args)?;

    // Resolve partial vs full repay amount.
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
    // Minimum-repay threshold to mitigate 1-atom-at-a-time grief:
    // partial liquidations must repay at least 1% of the loan's
    // outstanding debt (or be a full repay). Without this gate a
    // liquidator can drain collateral via the bonus over many tiny
    // calls. Cap is generous enough that legitimate keepers running
    // tight liquidations aren't blocked.
    let is_full_repay: bool = actual_repay_atoms == outstanding_live_atoms;
    if !is_full_repay {
        // Drip-grief floor. `outstanding / 100` underflows to 0 for
        // sub-100-atom residuals; without an absolute floor a
        // liquidator could call with `repay_atoms = 1` against tiny
        // residuals, paying the keeper bonus on each call while never
        // closing the loan. Force tiny residuals into the full-repay
        // branch so the keeper takes their bonus exactly once.
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

    // Compute bonus, surplus, and bad debt at the bare exchange rate.
    // Bare debt-value-in-collateral-atoms uses unit weights so we
    // measure the actual swap value at oracle prices for the chosen
    // `actual_repay_atoms`. The liquidator seizes that value plus a
    // keeper bonus in collateral atoms; bad-debt arises iff total
    // available collateral can't cover even this slice.
    //
    // This is intended liquidation economics. The
    // collateral is valued at the bare oracle price (unit asset
    // weights, `ltv_buffer_bps = 0`) — i.e. oracle-par — because the
    // liquidator is repaying real debt 1:1 and seizing real collateral;
    // the keeper bonus is their incentive. Maintenance/init weights and
    // the LTV buffer belong to the *solvency gate* (`assert_ltv_breach`
    // above), which decides WHEN a loan may be liquidated — not to the
    // seizure valuation, which decides HOW MUCH collateral the repaid
    // debt buys. Applying a maintenance haircut here would let the
    // liquidator seize MORE collateral per atom repaid (a haircut < 1×
    // raises `required` collateral), accelerating bad debt rather than
    // preventing it. The bad-debt accounting below (`bad_debt_gap_atoms`
    // = `total_seize_target − collateral_atoms`) is therefore correct:
    // it measures the shortfall in oracle-par terms.
    const FP48_ONE: u128 = 1u128 << 48;
    let repay_value_in_collateral_atoms = get_required_quote_collateral_to_back_debt(
        actual_repay_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        FP48_ONE,
        FP48_ONE,
        /*ltv_buffer_bps=*/ 0,
        debt_mint_decimals,
        collateral_mint_decimals,
    )?;
    let CollateralSplit {
        liquidator_seizes_atoms,
        // `surplus_atoms` (planned residual on the borrower's marginfi)
        // is replaced downstream by `collateral_atoms - withdrawn_atoms`
        // to absorb the ±1 atom drift on the marginfi withdraw CPI.
        surplus_atoms: _,
        bad_debt_gap_atoms,
        ..
    } = compute_collateral_split(repay_value_in_collateral_atoms, collateral_atoms, bonus_bps)?;

    // Liquidator must hold enough debt-mint atoms.
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
    // Fixed: liquidator's atoms top up the lender side. `marginfi.deposit
    // market_debt_vault → lender_marginfi_account` accrues asset shares
    // the lender (a vault risk profile) drains via
    // `claim_repayment_for_risk_profile`.
    //
    // P2Pool: there is no human lender. The canonical debt is the
    // borrower's marginfi liability_shares; the liquidator's atoms must
    // retire those shares directly via `marginfi.repay_atoms
    // market_debt_vault → borrower_marginfi_account`. Without this
    // branch the residual liability would stay parked on the borrower's
    // marginfi-account, locked against their collateral via marginfi's
    // own solvency check, even after the yDelta loan body shows
    // `outstanding == 0`.
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
            let _shares_burned: u128 = if is_full_repay {
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

    // Withdraw `liquidator_seizes_atoms` collateral
    //     → staging → liquidator. (Surplus stays on borrower marginfi.)
    let liquidator_seizes_shares: u128 = MarginfiV18Adapter
        .amount_to_asset_shares(&[collateral_bank.info.clone()], liquidator_seizes_atoms)?;
    // Health-check remaining accounts must mirror the borrower's
    // marginfi active-balance slots in order. OB-only Fixed loans
    // hold a single collateral asset balance; P2Pool loans add a
    // debt liability balance (collateral asset slot first per
    // insertion order).
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
    let withdrawn_atoms = MarginfiV18Adapter.withdraw(
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

    // Update the borrower seat.
    // Full repay: release the entire original collateral encumbrance,
    // credit surplus to withdrawable, decrement open_borrow_count.
    // Partial: release only the actually-withdrawn share-equivalent;
    // surplus is not credited (remaining collateral still encumbers
    // residual debt), and open_borrow_count stays.
    //
    // Share calc uses the loan's place-time snapshot (byte-symmetric
    // with the original encumber) and `withdrawn_atoms` rather than
    // pre-CPI inputs — marginfi can return ±1 atom drift, and using
    // the CPI return value keeps the seat ledger in sync with
    // marginfi's authoritative book.
    let collateral_snapshot_fp48: u128 = {
        let loan_data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
        header.borrower_collateral_share_price_snapshot_fp48
    };
    // Surplus = collateral_atoms - withdrawn_atoms on full repay.
    // Computing surplus from withdrawn_atoms (rather than the planned
    // liquidator_seizes_atoms) keeps the borrower's withdrawable
    // credit consistent with what physically stayed on borrower's
    // marginfi after the CPI.
    let surplus_atoms_actual: u64 = if did_full_repay {
        collateral_atoms.saturating_sub(withdrawn_atoms)
    } else {
        0
    };
    let surplus_shares: u128 = if surplus_atoms_actual > 0 {
        atoms_to_shares_at_snapshot(surplus_atoms_actual, collateral_snapshot_fp48)
    } else {
        0
    };
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
        // Full repay: release everything that was originally encumbered
        // (computed from the original `collateral_atoms` at snapshot,
        // matching place-time encumber atom-for-atom). Partial:
        // release only what marginfi physically withdrew.
        let release_shares: u128 = if did_full_repay {
            atoms_to_shares_at_snapshot(collateral_atoms, collateral_snapshot_fp48)
        } else {
            atoms_to_shares_at_snapshot(withdrawn_atoms, collateral_snapshot_fp48)
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

    // Liquidation protocol fee (debt-mint, accrued onto the loan
    // body). Marginfi-style: a configurable fraction of every
    // liquidation event is owed to the protocol.
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
    //
    // The liquidator's FULL `actual_repay_atoms` were deposited into
    // `lender_marginfi_account` by the Fixed-branch CPI above (P2Pool
    // retires the borrower liability instead — no protocol fee there).
    //
    // For Fixed loans `apply_partial_resolution` retires the
    // `actual_repay_atoms` slice and shrinks the three conservation
    // fields proportionally, preserving the identity `outstanding ==
    // lender_claimable + protocol_fee + curator_fee`.
    //
    // The liquidation protocol fee is then a HAIRCUT on the lender's
    // recovery — there is no genuine unattributed surplus on a
    // liquidation (`actual_repay` exactly retires accrued debt). It is
    // taken strictly from `lender_claimable_atoms` (capped at it) and
    // moved onto `accumulated_protocol_fee_atoms`. That is a pure
    // transfer between two terms of the conservation sum, so the
    // identity is preserved and the accumulator stays backed: every
    // atom is physically present in `lender_marginfi_account` (the
    // claim ix sweeps `protocol_fee_atoms` onto
    // `market.accumulated_protocol_fee_shares`, drained by
    // `protocol_fee_claim` out of that SAME account).
    //
    // On a bad-debt close (`bad_debt_gap_atoms > 0`) the curator —
    // a fee-taker, not a senior creditor — must not collect a
    // management fee accrued on interest the lender never received. Its
    // accumulated curator fee is clawed back into `lender_claimable`
    // before close so the LP recovers ahead of the curator.
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
            // Fixed: retire the debt slice across all four fields.
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
            // Curator clawback on bad debt.
            if bad_debt_gap_atoms > 0 && header.accumulated_curator_fee_atoms > 0 {
                let clawback: u64 = header.accumulated_curator_fee_atoms;
                header.accumulated_curator_fee_atoms = 0;
                header.lender_claimable_atoms = header
                    .lender_claimable_atoms
                    .checked_add(clawback)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
            // Decrement collateral by the ACTUAL `withdrawn_atoms`
            // returned by the marginfi withdraw CPI (±1 atom drift vs
            // the planned `liquidator_seizes_atoms`).
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
        collateral_seized_atoms: liquidator_seizes_atoms,
        liquidation_kind: 1, // LTV-breach
        is_partial: if did_full_repay { 0 } else { 1 },
        _padding: [0; 14],
    })?;
    if bad_debt_gap_atoms > 0 {
        // Bad-debt gap is in collateral terms: the liquidator paid
        // `actual_repay_atoms` of debt but the loan's collateral
        // couldn't fully cover the corresponding swap + bonus. Residual
        // debt (if partial) stays on the loan — see `outstanding_debt_atoms`.
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

    if loan_type == LoanType::P2Pool && did_full_repay {
        super::shared::close_account_and_refund(loan.info, cranker_refund)?;
    }

    Ok(())
}

/// Split `collateral_atoms` between the liquidator (paid debt +
/// bonus) and the borrower (surplus). Bad-debt path triggers when
/// collateral falls short of `debt_value + bonus`.
pub struct CollateralSplit {
    pub liquidator_seizes_atoms: u64,
    pub surplus_atoms: u64,
    pub bad_debt_gap_atoms: u64,
    pub bonus_atoms: u64,
}

pub fn compute_collateral_split(
    debt_value_in_collateral_atoms: u64,
    collateral_atoms: u64,
    bonus_bps: u16,
) -> Result<CollateralSplit, ProgramError> {
    require!(
        bonus_bps <= 10_000,
        YdeltaError::InvalidArgument,
        "bonus_bps {} exceeds 10_000",
        bonus_bps
    )?;
    let bonus_atoms: u64 = (debt_value_in_collateral_atoms as u128)
        .checked_mul(bonus_bps as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?
        .checked_div(10_000)
        .ok_or(ProgramError::ArithmeticOverflow)?
        .min(u64::MAX as u128) as u64;
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
        // 750 bps bonus on 80 → 6 bonus atoms.
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
        // Debt-in-collateral = 100, bonus = 0, but only 80 collateral.
        let s = compute_collateral_split(100, 80, 0).unwrap();
        assert_eq!(s.liquidator_seizes_atoms, 80);
        assert_eq!(s.surplus_atoms, 0);
        assert_eq!(s.bad_debt_gap_atoms, 20);
    }

    #[test]
    fn under_collateralized_with_bonus_increases_gap() {
        // Debt 100, bonus 10 (1000 bps) → target 110, collateral 90.
        let s = compute_collateral_split(100, 90, 1_000).unwrap();
        assert_eq!(s.bonus_atoms, 10);
        assert_eq!(s.liquidator_seizes_atoms, 90);
        assert_eq!(s.surplus_atoms, 0);
        assert_eq!(s.bad_debt_gap_atoms, 20);
    }

    #[test]
    fn rejects_bonus_bps_above_full_scale() {
        // bonus_bps > 10_000 is a corrupt config — reject before the
        // multiply produces a > 1× bonus.
        assert!(compute_collateral_split(100, 100, 10_001).is_err());
        assert!(compute_collateral_split(100, 100, u16::MAX).is_err());
    }

    #[test]
    fn accepts_bonus_bps_at_full_scale() {
        // 10_000 is the boundary — equivalent to 100% bonus, allowed.
        let s = compute_collateral_split(50, 200, 10_000).unwrap();
        assert_eq!(s.bonus_atoms, 50);
        assert_eq!(s.liquidator_seizes_atoms, 100);
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
