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
//! Both ixs are genuinely READ-ONLY. They accrue interest into an
//! owned stack copy of the loan header and never borrow the loan
//! account data mutably — submitting either as a real (non-simulation)
//! transaction commits no accrual state to the loan account.
//!
//! Caller usage:
//!     let sim = rpc.simulate_transaction(&tx, ...).await?;
//!     match sim.value.err {
//!         None => /* loan is liquidatable; submit the real ix */,
//!         Some(custom_err) => /* not liquidatable; surface to UI */,
//!     }

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
    let (grace_period_seconds, debt_mint_decimals, collateral_mint_decimals): (u32, u8, u8) = {
        let m = market.get_fixed()?;
        (
            m.fee_config.grace_period_seconds,
            m.debt_mint_decimals,
            m.collateral_mint_decimals,
        )
    };

    // Reject already-Repaid loans, accrue, then read live outstanding and
    // the collateral cap.
    //
    // This is a read-only simulation ix. We accrue into an OWNED copy of
    // the loan header (`LoanFixed` is `Pod`/`Copy`) and never borrow the
    // account data mutably — so if this ix is ever submitted as a real
    // (non-simulation) transaction it commits NO accrual state to the loan
    // account.
    let (collateral_atoms, outstanding_live_atoms): (u64, u64) = {
        let loan_data = loan.info.try_borrow_data()?;
        let mut header: LoanFixed =
            *bytemuck::from_bytes::<LoanFixed>(&loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "loan already in Repaid state"
        )?;
        accrue_loan(&mut header, now, grace_period_seconds)?;
        let outstanding = crate::state::ltv::loan_live_outstanding_atoms(
            &header,
            borrower_marginfi_account.info,
            debt_bank.info,
        )?;
        (header.collateral_atoms, outstanding)
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
    // verify live outstanding > 0.
    //
    // Read-only simulation ix — accrue into an OWNED copy of the loan
    // header and never borrow the account data mutably, so this ix
    // commits no accrual state even if submitted as a real tx.
    let outstanding_live_atoms: u64 = {
        let loan_data = loan.info.try_borrow_data()?;
        let mut header: LoanFixed =
            *bytemuck::from_bytes::<LoanFixed>(&loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "loan already in Repaid state"
        )?;
        accrue_loan(&mut header, now, grace_period_seconds)?;
        crate::state::ltv::assert_past_grace_period(&header, grace_period_seconds, now)?;
        crate::state::ltv::loan_live_outstanding_atoms(
            &header,
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
