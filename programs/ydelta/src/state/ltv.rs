//! Match-time LTV math in yDelta's fp48 arithmetic.

use solana_program::{
    account_info::AccountInfo, clock::Clock, program_error::ProgramError, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::math::{div_scale, from_scaled_floor, mul_scale, to_scaled};
use crate::program::YdeltaError;
use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
use crate::protocol::LendingProtocol;
use crate::require;
use crate::state::loan::{LoanFixed, LoanType};

/// Compute the minimum collateral atoms a match must carry to satisfy
/// the bank's `liability_weight_init` plus an `ltv_buffer_bps` safety
/// margin at the current oracle prices.
pub fn get_required_quote_collateral_to_back_debt(
    debt_atoms_traded: u64,
    debt_price_fp48: u128,
    coll_price_fp48: u128,
    liability_weight_init_fp48: u128,
    coll_asset_weight_init_fp48: u128,
    ltv_buffer_bps: u16,
) -> Result<u64, ProgramError> {
    if coll_price_fp48 == 0 || coll_asset_weight_init_fp48 == 0 {
        // Degenerate denominator — refuse rather than divide by zero.
        // The caller's `>=` comparison will then reject any match.
        return Ok(u64::MAX);
    }

    // numerator = debt_atoms × debt_price × liab_weight (fp48).
    let debt_atoms_fp48 = to_scaled(debt_atoms_traded as u128)?;
    let num1 = mul_scale(debt_atoms_fp48, debt_price_fp48)?;
    let num2 = mul_scale(num1, liability_weight_init_fp48)?;

    // apply ltv_buffer multiplier = 1 + buffer / 10_000.
    // Compute as numerator × (10_000 + buffer) / 10_000 in fp48.
    let buffered = if ltv_buffer_bps > 0 {
        let buf_factor = 10_000u128
            .checked_add(ltv_buffer_bps as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let scaled = num2
            .checked_mul(buf_factor)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        scaled / 10_000u128
    } else {
        num2
    };

    // denominator = coll_price × coll_asset_weight (fp48).
    let denom = mul_scale(coll_price_fp48, coll_asset_weight_init_fp48)?;

    // divide → atoms (fp48), then floor to u64.
    let result_fp48 = div_scale(buffered, denom)?;
    let atoms_u128 = from_scaled_floor(result_fp48);
    if atoms_u128 > u64::MAX as u128 {
        // Match would require more atoms than u64 can represent — by
        // any reasonable LTV that's a non-trade. Saturate so the
        // caller rejects.
        return Ok(u64::MAX);
    }
    Ok(atoms_u128 as u64)
}

// ─────────────────── Live outstanding-debt read ───────────────────
//
// Fixed loans accrue dual-rate interest on `LoanFixed.outstanding_debt_atoms`
// at every `accrue_loan` call, so the on-disk field is the canonical debt
// at any moment after accrual. P2Pool loans are different: `accrue_loan`
// is a no-op for them (loan.rs:443-446), and the canonical debt is the
// borrower's marginfi liability_shares × the bank's live
// `liability_share_value`. The LTV gate, the repay-amount math, and the
// simulation ixs all need a single source of truth — this helper.

/// Read the borrower marginfi-account's `liability_shares` for `debt_bank`.
/// Zero when the account has no balance entry for that bank yet.
pub fn read_borrower_liability_shares(
    borrower_marginfi_ai: &AccountInfo,
    debt_bank_pk: &Pubkey,
) -> Result<u128, ProgramError> {
    let data = borrower_marginfi_ai.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| YdeltaError::IncorrectAccount)?;
    Ok(mfi
        .find_balance(debt_bank_pk)
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares))
        .unwrap_or(0))
}

/// Convert marginfi liability shares to atoms at the bank's current
/// `liability_share_value` (fp48). Mirrors the adapter's `repay` ceil
/// path — the liability-side rounding direction is `ceil` so a debt of
/// `n.5` atoms reads as `n+1` rather than `n`. The caller's gate uses
/// this value to compare against collateral, so over-counting by ≤1
/// atom is safe and rounding-stable.
pub fn liability_shares_to_atoms_ceil(
    debt_bank_ai: &AccountInfo,
    shares: u128,
) -> Result<u64, ProgramError> {
    let data = debt_bank_ai.try_borrow_data()?;
    let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
        .map_err(|_| YdeltaError::IncorrectAccount)?;
    let lsv = wrapped_i80f48_to_u128(bank.liability_share_value);
    let atoms_fp48 = mul_scale(shares, lsv)?;
    let atoms_u128 = crate::math::from_scaled_ceil(atoms_fp48);
    if atoms_u128 > u64::MAX as u128 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    Ok(atoms_u128 as u64)
}

/// Loan-aware live outstanding-debt read.
///
/// Fixed: returns `loan.outstanding_debt_atoms` (caller MUST have run
/// `accrue_loan` first).
///
/// P2Pool: ignores `loan.outstanding_debt_atoms` (which is decorative —
/// stamped at promotion and never accrued) and reads
/// `borrower_marginfi.liability_shares × debt_bank.liability_share_value`
/// instead. This is the canonical debt: marginfi has been compounding it
/// at the variable borrow APR since the loan was opened, and the
/// liquidator / repay flow needs to settle against the live shares — not
/// the frozen principal snapshot.
pub fn loan_live_outstanding_atoms(
    loan: &LoanFixed,
    borrower_marginfi_ai: &AccountInfo,
    debt_bank_ai: &AccountInfo,
) -> Result<u64, ProgramError> {
    match loan.loan_type()? {
        LoanType::Fixed => Ok(loan.outstanding_debt_atoms),
        LoanType::P2Pool => {
            let shares = read_borrower_liability_shares(borrower_marginfi_ai, debt_bank_ai.key)?;
            liability_shares_to_atoms_ceil(debt_bank_ai, shares)
        }
    }
}

// ─────────────────── Liquidation gates ───────────────────
//
// Both gates are pure read-only checks: they error out with the matching
// `YdeltaError` if the loan is NOT eligible. The real ix paths
// (`liquidate_loan`, `settle_matured_loan`) and the simulation paths
// (step 4 — `CheckLtvLiquidatable`, `CheckMaturityLiquidatable`) all run
// the same checks via this single helper so a successful simulation
// guarantees a successful real call (modulo CPI side-effects).

/// Maintenance-tier LTV breach gate. Returns `Ok(())` iff the loan would
/// fail marginfi's maintenance solvency check at current oracle prices.
/// Errors with `YdeltaError::LoanStillSolvent` otherwise.
///
/// `outstanding_atoms` should be the live outstanding (from
/// `loan_live_outstanding_atoms`); `collateral_atoms` is the loan's
/// `collateral_atoms` field (stamped at match time and only decremented
/// on partial liquidations). Caller passes the variadic oracle slices for
/// each bank — see `MarginfiOracleAis::oracle_price_args`.
#[allow(clippy::too_many_arguments)]
pub fn assert_ltv_breach<'info>(
    outstanding_atoms: u64,
    collateral_atoms: u64,
    debt_bank_ai: &AccountInfo<'info>,
    debt_oracle_args: &[AccountInfo<'info>],
    collateral_bank_ai: &AccountInfo<'info>,
    collateral_oracle_args: &[AccountInfo<'info>],
) -> Result<(), ProgramError> {
    require!(
        outstanding_atoms > 0,
        YdeltaError::InvalidArgument,
        "outstanding_debt_atoms is 0 — already settled"
    )?;
    require!(
        collateral_atoms > 0,
        YdeltaError::LiquidationCollateralUnderflow,
        "loan has no collateral"
    )?;

    let debt_price_fp48: u128 = MarginfiV18Adapter.oracle_price(debt_oracle_args)?;
    let collateral_price_fp48: u128 = MarginfiV18Adapter.oracle_price(collateral_oracle_args)?;
    let (_debt_asset_weight_maint, debt_liability_weight_maint) =
        MarginfiV18Adapter.maint_weight(&[debt_bank_ai.clone()])?;
    let (collateral_asset_weight_maint, _) =
        MarginfiV18Adapter.maint_weight(&[collateral_bank_ai.clone()])?;

    let required_collateral_atoms = get_required_quote_collateral_to_back_debt(
        outstanding_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        debt_liability_weight_maint,
        collateral_asset_weight_maint,
        /*ltv_buffer_bps=*/ 0,
    )?;
    require!(
        collateral_atoms < required_collateral_atoms,
        YdeltaError::LoanStillSolvent,
        "collateral {} >= required {} at maint weights — loan still solvent",
        collateral_atoms,
        required_collateral_atoms
    )
}

/// Past-grace-period gate. Returns `Ok(())` iff the loan is past
/// `matures_at_unix + grace_period_seconds`; errors with
/// `YdeltaError::LoanNotMatured` otherwise.
pub fn assert_past_grace_period(
    loan: &LoanFixed,
    grace_period_seconds: u32,
    now_unix_ts: i64,
) -> Result<(), ProgramError> {
    let grace_end = loan
        .matures_at_unix
        .saturating_add(grace_period_seconds as i64);
    require!(
        now_unix_ts > grace_end,
        YdeltaError::LoanNotMatured,
        "now ({}) <= matures_at + grace ({})",
        now_unix_ts,
        grace_end
    )
}

/// `Clock::get()?.unix_timestamp` wrapper that mirrors how the ix
/// processors read it. Pulled into the helper module so simulation ixs
/// don't duplicate the boilerplate.
pub fn now_unix_ts() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp48(whole: u64) -> u128 {
        (whole as u128) << 48
    }

    fn fp48_div(num: u64, den: u64) -> u128 {
        ((num as u128) << 48) / (den as u128)
    }

    #[test]
    fn unit_prices_unit_weights_no_buffer() {
        // Debt and collateral at $1 each, both weights 1.0, no buffer:
        // required = debt_atoms (atoms map 1:1).
        let r = get_required_quote_collateral_to_back_debt(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48(1),
            fp48(1),
            0,
        )
        .unwrap();
        assert_eq!(r, 1_000_000);
    }

    #[test]
    fn liability_weight_inflates_required() {
        // Liab weight 1.25 → require 25% more collateral.
        let r = get_required_quote_collateral_to_back_debt(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48_div(125, 100),
            fp48(1),
            0,
        )
        .unwrap();
        assert_eq!(r, 1_250_000);
    }

    #[test]
    fn collateral_weight_below_one_inflates_required() {
        // Collateral weight 0.8 → require 1/0.8 = 1.25× more.
        let r = get_required_quote_collateral_to_back_debt(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48(1),
            fp48_div(8, 10),
            0,
        )
        .unwrap();
        assert_eq!(r, 1_250_000);
    }

    #[test]
    fn ltv_buffer_adds_top_up() {
        // 2% buffer → require 2% more collateral on top of the bare math.
        let bare = get_required_quote_collateral_to_back_debt(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48(1),
            fp48(1),
            0,
        )
        .unwrap();
        let buffered = get_required_quote_collateral_to_back_debt(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48(1),
            fp48(1),
            200,
        )
        .unwrap();
        assert_eq!(bare, 1_000_000);
        assert_eq!(buffered, 1_020_000);
    }

    #[test]
    fn debt_more_expensive_than_collateral_inflates_required() {
        // Debt $100, collateral $1 → require 100× as much collateral.
        let r = get_required_quote_collateral_to_back_debt(
            1_000,
            fp48(100),
            fp48(1),
            fp48(1),
            fp48(1),
            0,
        )
        .unwrap();
        assert_eq!(r, 100_000);
    }

    #[test]
    fn zero_collateral_price_returns_u64_max() {
        let r = get_required_quote_collateral_to_back_debt(1_000, fp48(1), 0, fp48(1), fp48(1), 0)
            .unwrap();
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn zero_collateral_weight_returns_u64_max() {
        let r = get_required_quote_collateral_to_back_debt(1_000, fp48(1), fp48(1), fp48(1), 0, 0)
            .unwrap();
        assert_eq!(r, u64::MAX);
    }
}
