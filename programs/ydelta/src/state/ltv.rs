//! LTV math used at match time and liquidation time. Computes the
//! quote-collateral needed to back a borrow and gates liquidation on
//! `collateral < required_at_maint_weights`.

use solana_program::{
    account_info::AccountInfo, clock::Clock, program_error::ProgramError, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::math::{from_scaled_ceil, mul_scale};
use crate::program::YdeltaError;
use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
use crate::protocol::LendingProtocol;
use crate::require;
use crate::state::loan::{LoanFixed, LoanType};

fn pow10_u128(exp: u32) -> Result<u128, ProgramError> {
    let mut acc: u128 = 1;
    for _ in 0..exp {
        acc = acc
            .checked_mul(10)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(acc)
}

/// Computes the collateral (in collateral atoms) required to back
/// `debt_atoms_traded` at the supplied prices and weights, applying an
/// optional bps top-up buffer and normalizing across mint-decimal
/// differences. Returns `u64::MAX` (fail-closed) when any input is zero
/// so callers compare it as "always under-collateralized".
pub fn get_required_quote_collateral_to_back_debt(
    debt_atoms_traded: u64,
    debt_price_fp48: crate::math::Fp48,
    coll_price_fp48: crate::math::Fp48,
    liability_weight_init_fp48: crate::math::Fp48,
    coll_asset_weight_init_fp48: crate::math::Fp48,
    ltv_buffer_bps: u16,
    debt_mint_decimals: u8,
    collateral_mint_decimals: u8,
) -> Result<u64, ProgramError> {
    if coll_price_fp48.is_zero()
        || coll_asset_weight_init_fp48.is_zero()
        || debt_price_fp48.is_zero()
        || liability_weight_init_fp48.is_zero()
    {
        return Ok(u64::MAX);
    }

    // debt_atoms × debt_price × liability_weight, all fp48-typed.
    let debt_atoms_fp48 = crate::math::Fp48::from_atoms(debt_atoms_traded);
    let num1 = debt_atoms_fp48.checked_mul(debt_price_fp48)?;
    let num2 = num1.checked_mul(liability_weight_init_fp48)?;

    let buffered = if ltv_buffer_bps > 0 {
        let buf_factor = 10_000u128
            .checked_add(ltv_buffer_bps as u128)
            .ok_or(crate::program::YdeltaError::MathOverflow)?;
        // num2 fp48 × buf_factor / 10_000, preserving fp48 scale.
        crate::math::Fp48::from_raw(crate::math::mul_div(
            num2.raw(),
            buf_factor,
            10_000u128,
            false,
        )?)
    } else {
        num2
    };

    let denom = coll_price_fp48.checked_mul(coll_asset_weight_init_fp48)?;

    let result_fp48 = buffered.checked_div(denom)?;

    let normalized_fp48: u128 = if collateral_mint_decimals >= debt_mint_decimals {
        let factor = pow10_u128((collateral_mint_decimals - debt_mint_decimals) as u32)?;
        crate::math::mul_div(result_fp48.raw(), factor, 1, false)?
    } else {
        let factor = pow10_u128((debt_mint_decimals - collateral_mint_decimals) as u32)?;
        crate::math::mul_div(result_fp48.raw(), 1, factor, false)?
    };

    let atoms_u128 = from_scaled_ceil(normalized_fp48)?;
    if atoms_u128 > u64::MAX as u128 {
        return Ok(u64::MAX);
    }
    Ok(atoms_u128 as u64)
}

/// Reads the borrower's marginfi liability-share count against
/// `debt_bank_pk`. Returns 0 when the borrower has no balance in that
/// bank.
pub fn read_borrower_liability_shares(
    borrower_marginfi_ai: &AccountInfo,
    debt_bank_pk: &Pubkey,
) -> Result<u128, ProgramError> {
    let data = borrower_marginfi_ai.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| YdeltaError::IncorrectAccount)?;
    match mfi.find_balance(debt_bank_pk) {
        Some(b) => wrapped_i80f48_to_u128(b.liability_shares),
        None => Ok(0),
    }
}

/// Converts marginfi liability-shares to debt atoms using the bank's
/// current `liability_share_value`, rounding up so the protocol never
/// under-funds a repay.
pub fn liability_shares_to_atoms_ceil(
    debt_bank_ai: &AccountInfo,
    shares: u128,
) -> Result<u64, ProgramError> {
    let data = debt_bank_ai.try_borrow_data()?;
    let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
        .map_err(|_| YdeltaError::IncorrectAccount)?;
    let lsv = wrapped_i80f48_to_u128(bank.liability_share_value)?;
    let atoms_fp48 = mul_scale(shares, lsv)?;
    let atoms_u128 = crate::math::from_scaled_ceil(atoms_fp48)?;
    if atoms_u128 > u64::MAX as u128 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    Ok(atoms_u128 as u64)
}

/// Returns the loan's current outstanding debt in atoms. For fixed-term
/// loans this is the ledger value; for P2Pool loans it is derived live
/// from marginfi's share value scaled by the loan's recorded shares.
pub fn loan_live_outstanding_atoms(
    loan: &LoanFixed,
    borrower_marginfi_ai: &AccountInfo,
    debt_bank_ai: &AccountInfo,
) -> Result<u64, ProgramError> {
    match loan.loan_type()? {
        LoanType::Fixed => Ok(loan.outstanding_debt_atoms),
        LoanType::P2Pool => {
            let account_total =
                read_borrower_liability_shares(borrower_marginfi_ai, debt_bank_ai.key)?;
            let this_loan_shares = loan.borrower_marginfi_borrow_shares.min(account_total);
            liability_shares_to_atoms_ceil(debt_bank_ai, this_loan_shares)
        }
    }
}

/// Gates LTV-based liquidation. Reads oracle prices and maintenance
/// weights from the supplied account infos, computes required collateral
/// at maint weights, and errors with `LoanStillSolvent` when the loan's
/// `collateral >= required`. Rejects degenerate oracles up-front via
/// `OracleDegenerate` so a zero feed can never auto-liquidate a healthy
/// loan.
#[allow(clippy::too_many_arguments)]
pub fn assert_ltv_breach<'info>(
    outstanding_atoms: u64,
    collateral_atoms: u64,
    debt_bank_ai: &AccountInfo<'info>,
    debt_oracle_args: &[AccountInfo<'info>],
    collateral_bank_ai: &AccountInfo<'info>,
    collateral_oracle_args: &[AccountInfo<'info>],
    debt_mint_decimals: u8,
    collateral_mint_decimals: u8,
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

    // Adapter calls still return raw u128 fp48; wrap at the boundary.
    let debt_price_fp48 =
        crate::math::Fp48::from_raw(MarginfiV18Adapter.oracle_price(debt_oracle_args)?);
    let collateral_price_fp48 =
        crate::math::Fp48::from_raw(MarginfiV18Adapter.oracle_price(collateral_oracle_args)?);
    let (_debt_asset_weight_maint, debt_liability_weight_maint_raw) =
        MarginfiV18Adapter.maint_weight(&[debt_bank_ai.clone()])?;
    let debt_liability_weight_maint = crate::math::Fp48::from_raw(debt_liability_weight_maint_raw);
    let (collateral_asset_weight_maint_raw, _) =
        MarginfiV18Adapter.maint_weight(&[collateral_bank_ai.clone()])?;
    let collateral_asset_weight_maint = crate::math::Fp48::from_raw(collateral_asset_weight_maint_raw);

    require!(
        !collateral_price_fp48.is_zero()
            && !collateral_asset_weight_maint.is_zero()
            && !debt_price_fp48.is_zero()
            && !debt_liability_weight_maint.is_zero(),
        YdeltaError::OracleDegenerate,
        "degenerate oracle/weight (debt_price={}, liab_weight={}, \
         coll_price={}, coll_weight={}) — refusing to evaluate the \
         liquidation gate; cannot prove a breach",
        debt_price_fp48.raw(),
        debt_liability_weight_maint.raw(),
        collateral_price_fp48.raw(),
        collateral_asset_weight_maint.raw()
    )?;

    let required_collateral_atoms = get_required_quote_collateral_to_back_debt(
        outstanding_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        debt_liability_weight_maint,
        collateral_asset_weight_maint,
        0,
        debt_mint_decimals,
        collateral_mint_decimals,
    )?;
    require!(
        collateral_atoms < required_collateral_atoms,
        YdeltaError::LoanStillSolvent,
        "collateral {} >= required {} at maint weights — loan still solvent",
        collateral_atoms,
        required_collateral_atoms
    )
}

/// Errors with `LoanNotMatured` until `now_unix_ts` is strictly past
/// `loan.matures_at_unix + grace_period_seconds`. Used by the settle and
/// maturity-liquidate paths.
pub fn assert_past_grace_period(
    loan: &LoanFixed,
    grace_period_seconds: u32,
    now_unix_ts: i64,
) -> Result<(), ProgramError> {
    let grace_end = loan
        .matures_at_unix
        .checked_add(grace_period_seconds as i64)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    require!(
        now_unix_ts > grace_end,
        YdeltaError::LoanNotMatured,
        "now ({}) <= matures_at + grace ({})",
        now_unix_ts,
        grace_end
    )
}

/// Returns the current unix timestamp from the Clock sysvar.
pub fn now_unix_ts() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp48(whole: u64) -> crate::math::Fp48 {
        crate::math::Fp48::from_raw((whole as u128) << 48)
    }

    fn fp48_div(num: u64, den: u64) -> crate::math::Fp48 {
        crate::math::Fp48::from_raw(((num as u128) << 48) / (den as u128))
    }

    fn req(
        debt_atoms: u64,
        debt_price_fp48: crate::math::Fp48,
        coll_price_fp48: crate::math::Fp48,
        liab_weight_fp48: crate::math::Fp48,
        coll_weight_fp48: crate::math::Fp48,
        ltv_buffer_bps: u16,
    ) -> Result<u64, ProgramError> {
        get_required_quote_collateral_to_back_debt(
            debt_atoms,
            debt_price_fp48,
            coll_price_fp48,
            liab_weight_fp48,
            coll_weight_fp48,
            ltv_buffer_bps,
            6,
            6,
        )
    }

    #[test]
    fn unit_prices_unit_weights_no_buffer() {
        let r = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, 1_000_000);
    }

    #[test]
    fn liability_weight_inflates_required() {
        let r = req(1_000_000, fp48(1), fp48(1), fp48_div(125, 100), fp48(1), 0).unwrap();
        assert_eq!(r, 1_250_000);
    }

    #[test]
    fn collateral_weight_below_one_inflates_required() {
        let r = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48_div(8, 10), 0).unwrap();
        assert_eq!(r, 1_250_001);
    }

    #[test]
    fn ltv_buffer_adds_top_up() {
        let bare = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        let buffered = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 200).unwrap();
        assert_eq!(bare, 1_000_000);
        assert_eq!(buffered, 1_020_000);
    }

    #[test]
    fn debt_more_expensive_than_collateral_inflates_required() {
        let r = req(1_000, fp48(100), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, 100_000);
    }

    #[test]
    fn required_collateral_rounds_up_not_down() {
        let r = req(1, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(
            r, 1,
            "fractional required-collateral must ceil to 1, not floor to 0"
        );

        let r2 = req(10, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r2, 4, "10/3 required collateral must ceil to 4");

        let exact = req(9, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(exact, 3, "9/3 is exact — ceil leaves it at 3");
    }

    #[test]
    fn zero_collateral_price_returns_u64_max() {
        let r = req(1_000, fp48(1), crate::math::Fp48::ZERO, fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn zero_collateral_weight_returns_u64_max() {
        let r = req(1_000, fp48(1), fp48(1), fp48(1), crate::math::Fp48::ZERO, 0).unwrap();
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn zero_debt_price_returns_u64_max_not_zero() {
        let r = req(1_000_000, crate::math::Fp48::ZERO, fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(
            r,
            u64::MAX,
            "zero debt price MUST fail-closed (u64::MAX), NOT permissive (0)"
        );
    }

    #[test]
    fn zero_liability_weight_returns_u64_max_not_zero() {
        let r = req(1_000_000, fp48(1), fp48(1), crate::math::Fp48::ZERO, fp48(1), 0).unwrap();
        assert_eq!(
            r,
            u64::MAX,
            "zero liability weight MUST fail-closed (u64::MAX), NOT permissive (0)"
        );
    }

    #[test]
    fn all_zero_inputs_return_u64_max() {
        let r = req(
            1_000_000,
            crate::math::Fp48::ZERO,
            crate::math::Fp48::ZERO,
            crate::math::Fp48::ZERO,
            crate::math::Fp48::ZERO,
            0,
        )
        .unwrap();
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn required_collateral_normalizes_for_a_six_vs_nine_decimal_market() {
        let debt_atoms: u64 = 1_000_000;
        let usdc_price = fp48(1);
        let sol_price = fp48(100);

        let unnormalized = get_required_quote_collateral_to_back_debt(
            debt_atoms,
            usdc_price,
            sol_price,
            fp48(1),
            fp48(1),
            0,
            6,
            6,
        )
        .unwrap();

        assert_eq!(unnormalized, 10_000, "unnormalized formula output");

        let normalized = get_required_quote_collateral_to_back_debt(
            debt_atoms,
            usdc_price,
            sol_price,
            fp48(1),
            fp48(1),
            0,
            6,
            9,
        )
        .unwrap();
        assert_eq!(
            normalized, 10_000_000,
            "$1 of debt must require 0.01 SOL = 10_000_000 lamports"
        );
        assert_eq!(
            normalized,
            unnormalized * 1000,
            "normalized result must be exactly 1000× the unnormalized result for a 6/9 market"
        );
    }

    #[test]
    fn required_collateral_divides_for_a_nine_vs_six_decimal_market() {
        let r = get_required_quote_collateral_to_back_debt(
            1_000_000_000,
            fp48(100),
            fp48(1),
            fp48(1),
            fp48(1),
            0,
            9,
            6,
        )
        .unwrap();
        assert_eq!(
            r, 100_000_000,
            "1 SOL debt must require $100 = 100_000_000 USDC atoms"
        );
    }

    #[test]
    fn equal_decimals_normalization_factor_is_one() {
        for dec in [0u8, 6, 9, 18] {
            let r = get_required_quote_collateral_to_back_debt(
                1_000_000,
                fp48(1),
                fp48(1),
                fp48(1),
                fp48(1),
                0,
                dec,
                dec,
            )
            .unwrap();
            assert_eq!(r, 1_000_000, "equal decimals ({}) must not rescale", dec);
        }
    }

    #[test]
    fn normalization_preserves_ceil_rounding() {
        let r = get_required_quote_collateral_to_back_debt(
            1,
            fp48(1),
            fp48(3),
            fp48(1),
            fp48(1),
            0,
            6,
            9,
        )
        .unwrap();
        assert_eq!(
            r, 334,
            "normalized fractional required collateral must round UP"
        );
    }

    #[test]
    fn degenerate_oracle_required_is_not_a_safe_liquidation_sentinel() {
        let solvent_collateral: u64 = 1_000_000;

        let healthy_required = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert!(solvent_collateral >= healthy_required);
        assert!(
            !(solvent_collateral < healthy_required),
            "healthy oracle: solvent loan must not be liquidatable"
        );

        let degenerate_required =
            req(1_000_000, fp48(1), fp48(1), fp48(1), crate::math::Fp48::ZERO, 0).unwrap();
        assert_eq!(degenerate_required, u64::MAX);
        assert!(
            solvent_collateral < degenerate_required,
            "this is exactly the false-positive the degenerate-oracle \
             guard prevents: a u64::MAX 'required' makes every solvent \
             loan compare as breached — the gate must reject the oracle, \
             not the loan"
        );
    }
}
