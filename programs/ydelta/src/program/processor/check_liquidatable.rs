//! Read-only liquidatability gates designed for `simulateTransaction`.
//!
//! Two ixs:
//!   - `CheckLtvLiquidatable` (tag 40): runs the same maintenance-tier
//!     LTV gate `liquidate_loan` runs. Returns `Ok(())` iff the loan
//!     would be eligible; errors `LoanStillSolvent` (or
//!     `OracleStale` / `InvalidArgument`) otherwise.
//!   - `CheckMaturityLiquidatable` (tag 41): runs the same time gate
//!     `settle_matured_loan` runs. Returns `Ok(())` iff
//!     `now > matures_at + grace` AND live outstanding > 0; errors
//!     `LoanNotMatured` (or `InvalidArgument`) otherwise.
//!
//! Both share the helpers in `state/ltv.rs` (`assert_ltv_breach`,
//! `assert_past_grace_period`, `loan_live_outstanding_atoms`) with the
//! real ixs, so a successful simulation guarantees the real ix would
//! also pass that gate (modulo CPI side-effects).
//!
//! Caller usage:
//!     let sim = rpc.simulate_transaction(&tx, ...).await?;
//!     match sim.value.err {
//!         None => /* loan is liquidatable; submit the real ix */,
//!         Some(custom_err) => /* not liquidatable; surface to UI */,
//!     }

use std::cell::RefMut;

use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::program::YdeltaError;
use crate::require;
use crate::state::loan::{accrue_loan, LoanFixed, LoanState, LOAN_FIXED_SIZE};
use crate::validation::loaders::{CheckLtvLiquidatableContext, CheckMaturityLiquidatableContext};

pub fn process_check_ltv_liquidatable(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let ctx = CheckLtvLiquidatableContext::load(accounts)?;
    let CheckLtvLiquidatableContext {
        payer: _,
        market,
        loan,
        borrower_marginfi_account,
        debt_bank,
        debt_oracle_ais,
        collateral_bank,
        collateral_oracle_ais,
        marginfi_program: _,
    } = ctx;

    let now: i64 = Clock::get()?.unix_timestamp;
    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    // Mirror `liquidate_loan`'s preflight: reject already-Repaid loans,
    // accrue (no-op for P2Pool), then read live outstanding and the
    // collateral cap.
    let (collateral_atoms,) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "loan already in Repaid state"
        )?;
        accrue_loan(header, now, grace_period_seconds)?;
        (header.collateral_atoms,)
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
    )
}

pub fn process_check_maturity_liquidatable(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let ctx = CheckMaturityLiquidatableContext::load(accounts)?;
    let CheckMaturityLiquidatableContext {
        payer: _,
        market,
        loan,
        borrower_marginfi_account,
        debt_bank,
        marginfi_program: _,
    } = ctx;

    let now: i64 = Clock::get()?.unix_timestamp;
    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    // Reject already-Repaid loans, accrue, run the time gate, then
    // verify live outstanding > 0. Same preflight ordering as
    // `settle_matured_loan` so a passing sim → passing real ix.
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "loan already in Repaid state"
        )?;
        accrue_loan(header, now, grace_period_seconds)?;
    }
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
        "live outstanding is 0 — loan already settled"
    )?;
    Ok(())
}
