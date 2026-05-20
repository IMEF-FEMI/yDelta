//! Match-time LTV math in yDelta's fp48 arithmetic.

use solana_program::{
    account_info::AccountInfo, clock::Clock, program_error::ProgramError, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::math::{div_scale, from_scaled_ceil, mul_scale, to_scaled};
use crate::program::YdeltaError;
use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
use crate::protocol::LendingProtocol;
use crate::require;
use crate::state::loan::{LoanFixed, LoanType};

/// `10^exp` as a `u128`. Uses `checked_mul` and surfaces an overflow as
/// an error rather than `saturating_mul`. A `pow10`-style helper —
/// kept local to this module to mirror the one in `protocol/oracles.rs`
/// (which is private to that module).
fn pow10_u128(exp: u32) -> Result<u128, ProgramError> {
    let mut acc: u128 = 1;
    for _ in 0..exp {
        acc = acc
            .checked_mul(10)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }
    Ok(acc)
}

/// Compute the minimum collateral atoms a match must carry to satisfy
/// the bank's `liability_weight_init` plus an `ltv_buffer_bps` safety
/// margin at the current oracle prices.
///
/// Decimal normalization: oracle prices are USD per WHOLE token,
/// not per atom. The raw price ratio therefore yields a result carrying
/// an implicit `10^(debt_decimals − collateral_decimals)` factor. To
/// recover a true collateral-atom count the result must be multiplied
/// by `10^(collateral_decimals − debt_decimals)`. The factor is applied
/// in the fp48 domain BEFORE `from_scaled_ceil` so the conservative
/// round-UP is preserved. Equal decimals → factor 1 → behaviour
/// unchanged.
pub fn get_required_quote_collateral_to_back_debt(
    debt_atoms_traded: u64,
    debt_price_fp48: u128,
    coll_price_fp48: u128,
    liability_weight_init_fp48: u128,
    coll_asset_weight_init_fp48: u128,
    ltv_buffer_bps: u16,
    debt_mint_decimals: u8,
    collateral_mint_decimals: u8,
) -> Result<u64, ProgramError> {
    if coll_price_fp48 == 0 || coll_asset_weight_init_fp48 == 0 {
        // Degenerate denominator — refuse rather than divide by zero.
        // This saturates `required` to `u64::MAX`, which is correct
        // fail-closed behaviour ONLY for the match gate: its
        // `collateral >= required` comparison then rejects the trade.
        // The liquidation gate uses the OPPOSITE comparison
        // (`collateral < required`), so it must NOT reach this branch —
        // `assert_ltv_breach` hard-errors with `OracleDegenerate`
        // before calling this helper on a zero price/weight.
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

    // divide → atoms (fp48), then round UP to u64. Required
    // collateral must round AGAINST the borrower / FOR the protocol —
    // flooring lets a borrower post ~1 atom less than truly required
    // (a too-loose match gate) and makes the liquidation bar ~1 atom
    // too low. Ceiling is the conservative direction for both the match
    // gate (`collateral >= required`) and the liquidation gate
    // (`collateral < required`): a fractional atom of required
    // collateral always rounds to a whole atom the borrower must hold.
    let result_fp48 = div_scale(buffered, denom)?;

    // ── Decimal normalization ──
    // Oracle prices are USD per WHOLE token. The bare ratio above
    // carries an implicit factor of 10^(debt_decimals − coll_decimals).
    // Correct it by multiplying the result by 10^(coll_dec − debt_dec):
    //   - coll_dec >= debt_dec → multiply by 10^(coll_dec − debt_dec).
    //   - debt_dec >  coll_dec → divide   by 10^(debt_dec − coll_dec).
    // Applied in the fp48 domain so the conservative ceil below still
    // rounds the true (normalized) required collateral UP.
    let normalized_fp48: u128 = if collateral_mint_decimals >= debt_mint_decimals {
        let factor = pow10_u128((collateral_mint_decimals - debt_mint_decimals) as u32)?;
        result_fp48
            .checked_mul(factor)
            .ok_or(ProgramError::ArithmeticOverflow)?
    } else {
        let factor = pow10_u128((debt_mint_decimals - collateral_mint_decimals) as u32)?;
        // `factor` is a positive power of ten — never zero — so this
        // division cannot fault on a zero divisor. Integer division here
        // truncates the fp48 fractional bits toward zero; the ceil below
        // still rounds any surviving fractional atom UP.
        result_fp48
            .checked_div(factor)
            .ok_or(ProgramError::ArithmeticOverflow)?
    };

    let atoms_u128 = from_scaled_ceil(normalized_fp48)?;
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
    let atoms_u128 = crate::math::from_scaled_ceil(atoms_fp48)?;
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

    let debt_price_fp48: u128 = MarginfiV18Adapter.oracle_price(debt_oracle_args)?;
    let collateral_price_fp48: u128 = MarginfiV18Adapter.oracle_price(collateral_oracle_args)?;
    let (_debt_asset_weight_maint, debt_liability_weight_maint) =
        MarginfiV18Adapter.maint_weight(&[debt_bank_ai.clone()])?;
    let (collateral_asset_weight_maint, _) =
        MarginfiV18Adapter.maint_weight(&[collateral_bank_ai.clone()])?;

    // A degenerate oracle/weight (zero collateral price or zero
    // collateral asset weight) makes `get_required_quote_collateral_to_back_debt`
    // saturate to `u64::MAX`. For the *match* gate that saturation is
    // correct fail-closed behaviour — `collateral >= u64::MAX` rejects
    // the match. But the LIQUIDATION gate's comparison is the opposite
    // direction (`collateral < required`): with `required = u64::MAX`
    // EVERY solvent loan reads as breached, letting a keeper liquidate
    // fully-collateralised loans on a momentary zero/garbage feed.
    // The two gates' fail-closed directions are opposite, so the
    // liquidation gate must HARD-ERROR on the degenerate case rather
    // than treat `u64::MAX` as "breached".
    require!(
        collateral_price_fp48 != 0 && collateral_asset_weight_maint != 0,
        YdeltaError::OracleDegenerate,
        "degenerate collateral oracle (price={}, weight={}) — refusing to \
         evaluate the liquidation gate; cannot prove a breach",
        collateral_price_fp48,
        collateral_asset_weight_maint
    )?;

    let required_collateral_atoms = get_required_quote_collateral_to_back_debt(
        outstanding_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        debt_liability_weight_maint,
        collateral_asset_weight_maint,
        /*ltv_buffer_bps=*/ 0,
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

    /// Equal-decimals wrapper: the normalization factor is 1, so these
    /// calls behave as if no decimal normalization applied. Every test
    /// below that does NOT exercise decimal normalization uses this.
    fn req(
        debt_atoms: u64,
        debt_price_fp48: u128,
        coll_price_fp48: u128,
        liab_weight_fp48: u128,
        coll_weight_fp48: u128,
        ltv_buffer_bps: u16,
    ) -> Result<u64, ProgramError> {
        get_required_quote_collateral_to_back_debt(
            debt_atoms,
            debt_price_fp48,
            coll_price_fp48,
            liab_weight_fp48,
            coll_weight_fp48,
            ltv_buffer_bps,
            /*debt_decimals=*/ 6,
            /*collateral_decimals=*/ 6,
        )
    }

    #[test]
    fn unit_prices_unit_weights_no_buffer() {
        // Debt and collateral at $1 each, both weights 1.0, no buffer,
        // equal decimals → required = debt_atoms (atoms map 1:1).
        let r = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, 1_000_000);
    }

    #[test]
    fn liability_weight_inflates_required() {
        // Liab weight 1.25 → require 25% more collateral.
        let r = req(1_000_000, fp48(1), fp48(1), fp48_div(125, 100), fp48(1), 0).unwrap();
        assert_eq!(r, 1_250_000);
    }

    #[test]
    fn collateral_weight_below_one_inflates_required() {
        // Collateral weight 0.8 → require 1/0.8 = 1.25× more. 0.8
        // has no exact fp48 representation, so `1_000_000 / 0.8_approx`
        // lands a hair above 1_250_000 and the (correct, conservative)
        // ceiling rounds it up to 1_250_001 — one atom MORE collateral
        // required, never less. The bare-floor result would have been
        // 1_250_000.
        let r = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48_div(8, 10), 0).unwrap();
        assert_eq!(r, 1_250_001);
    }

    #[test]
    fn ltv_buffer_adds_top_up() {
        // 2% buffer → require 2% more collateral on top of the bare math.
        let bare = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        let buffered = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 200).unwrap();
        assert_eq!(bare, 1_000_000);
        assert_eq!(buffered, 1_020_000);
    }

    #[test]
    fn debt_more_expensive_than_collateral_inflates_required() {
        // Debt $100, collateral $1 → require 100× as much collateral.
        let r = req(1_000, fp48(100), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, 100_000);
    }

    /// A non-integer required-collateral result must
    /// round UP, not down. Debt $1, collateral price 3.0 → required =
    /// debt_atoms / 3. For `debt_atoms = 1`, the exact result is
    /// 0.333… atoms; flooring yields 0 (the borrower posts NO
    /// collateral), ceiling yields 1 (the conservative bar).
    #[test]
    fn required_collateral_rounds_up_not_down() {
        let r = req(1, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(
            r, 1,
            "fractional required-collateral must ceil to 1, not floor to 0"
        );

        // A larger fractional case: 10 debt atoms / 3 = 3.333… → ceil 4.
        let r2 = req(10, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r2, 4, "10/3 required collateral must ceil to 4");

        // Exact-integer ratios are unaffected — ceil == floor when there
        // is no fractional part.
        let exact = req(9, fp48(1), fp48(3), fp48(1), fp48(1), 0).unwrap();
        assert_eq!(exact, 3, "9/3 is exact — ceil leaves it at 3");
    }

    #[test]
    fn zero_collateral_price_returns_u64_max() {
        let r = req(1_000, fp48(1), 0, fp48(1), fp48(1), 0).unwrap();
        assert_eq!(r, u64::MAX);
    }

    #[test]
    fn zero_collateral_weight_returns_u64_max() {
        let r = req(1_000, fp48(1), fp48(1), fp48(1), 0, 0).unwrap();
        assert_eq!(r, u64::MAX);
    }

    // ─────────────── Decimal-normalization tests ───────────────

    /// A USDC(6-dec) debt / SOL(9-dec) collateral market. SOL ≈ $100,
    /// USDC = $1, unit weights, no buffer. Repaying $1 of USDC debt
    /// (1_000_000 atoms) needs ~$1 of SOL = 0.01 SOL = 10_000_000
    /// lamports.
    ///
    /// Passing equal decimals (skipping normalization) returns ~10_000
    /// lamports ($0.001), 1000× too small. With the correct decimals the
    /// helper returns the true 10_000_000 lamports.
    #[test]
    fn required_collateral_normalizes_for_a_six_vs_nine_decimal_market() {
        let debt_atoms: u64 = 1_000_000; // $1 USDC at 6 decimals
        let usdc_price = fp48(1); // $1 per whole USDC
        let sol_price = fp48(100); // $100 per whole SOL

        // Equal decimals → no normalization.
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
        // $1 / $100 = 0.01 — at the WRONG (per-whole-token) scale that
        // is 10_000 "atoms".
        assert_eq!(unnormalized, 10_000, "unnormalized formula output");

        // Debt 6 dec, collateral 9 dec → multiply by 10^(9-6) = 1000.
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

    /// The reverse decimal direction (debt > collateral decimals) must
    /// also be correct: a SOL(9-dec) debt / USDC(6-dec) collateral
    /// market divides by 10^(9-6) = 1000.
    #[test]
    fn required_collateral_divides_for_a_nine_vs_six_decimal_market() {
        // 1 SOL of debt = 1_000_000_000 lamports, SOL $100, USDC $1.
        // Repaying 1 SOL ($100) needs $100 of USDC = 100_000_000 atoms.
        let r = get_required_quote_collateral_to_back_debt(
            1_000_000_000,
            fp48(100), // debt: $100/SOL
            fp48(1),   // collateral: $1/USDC
            fp48(1),
            fp48(1),
            0,
            /*debt_decimals=*/ 9,
            /*collateral_decimals=*/ 6,
        )
        .unwrap();
        assert_eq!(
            r, 100_000_000,
            "1 SOL debt must require $100 = 100_000_000 USDC atoms"
        );
    }

    /// Equal decimals → the normalization factor is exactly 1, so the
    /// helper does not rescale the result.
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

    /// Normalization is applied BEFORE the ceil, so the conservative
    /// round-UP still holds on the normalized value. Debt 6 dec /
    /// collateral 9 dec, a fractional pre-normalization result must ceil
    /// the *true* required collateral up, never down.
    #[test]
    fn normalization_preserves_ceil_rounding() {
        // 1 debt atom, collateral price 3.0, debt 6 dec / coll 9 dec.
        // Bare ratio = 1/3 atom; ×1000 = 333.333… → must ceil to 334.
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

    /// A degenerate oracle (zero collateral price or
    /// zero collateral asset weight) saturates `required` to `u64::MAX`.
    /// The match gate's `collateral >= required` comparison then
    /// fail-closes (rejects the trade) — correct. But the LIQUIDATION
    /// gate compares `collateral < required`; with `required = u64::MAX`
    /// EVERY solvent loan would read as breached. This test pins the
    /// fact that `u64::MAX` is NOT a safe "breached" sentinel for the
    /// liquidation direction — `assert_ltv_breach` must hard-error on
    /// the degenerate inputs (price==0 || weight==0) BEFORE this helper
    /// is reached, which it does via the `OracleDegenerate` require!.
    #[test]
    fn degenerate_oracle_required_is_not_a_safe_liquidation_sentinel() {
        // A fully solvent loan: 1_000_000 collateral atoms backing
        // 1_000_000 debt atoms, 1:1 prices, unit weights, equal decimals.
        let solvent_collateral: u64 = 1_000_000;

        // Healthy oracle: required is finite and the loan is solvent
        // (collateral >= required), so the liquidation comparison
        // `collateral < required` is false → NOT liquidatable. Good.
        let healthy_required = req(1_000_000, fp48(1), fp48(1), fp48(1), fp48(1), 0).unwrap();
        assert!(solvent_collateral >= healthy_required);
        assert!(
            !(solvent_collateral < healthy_required),
            "healthy oracle: solvent loan must not be liquidatable"
        );

        // Degenerate oracle (zero collateral weight): required saturates
        // to u64::MAX. If the liquidation gate naively reused this
        // value, `collateral < u64::MAX` would be TRUE → the solvent
        // loan would be (wrongly) liquidatable. assert_ltv_breach
        // therefore hard-errors on the degenerate inputs instead.
        let degenerate_required = req(
            1_000_000,
            fp48(1),
            fp48(1),
            fp48(1),
            /*coll_asset_weight=*/ 0,
            0,
        )
        .unwrap();
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
