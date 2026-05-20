//! Instruction builders for `CheckLtvLiquidatable` and
//! `CheckMaturityLiquidatable` — read-only liquidatability gates
//! designed for `simulateTransaction` callers.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::validation::get_borrower_integration_account_address;

/// Build a `CheckLtvLiquidatable` ix. Caller submits via
/// `simulateTransaction`; the loan is liquidatable iff the simulated tx
/// returns `Ok`. On failure the program error encodes which gate
/// rejected (`LoanStillSolvent`, `OracleStale`, `InvalidArgument` for
/// settled loans, etc.).
///
/// `debt_oracles` and `collateral_oracles` are variadic per the bank's
/// `OracleSetup` (see `MarginfiOracleAis`).
#[allow(clippy::too_many_arguments)]
pub fn check_ltv_liquidatable_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    sequence: u64,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    marginfi_program: &Pubkey,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let borrower_marginfi = get_borrower_integration_account_address(market).0;

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(borrower_marginfi, false),
        AccountMeta::new_readonly(*debt_bank, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.push(AccountMeta::new_readonly(*collateral_bank, false));
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.push(AccountMeta::new_readonly(*marginfi_program, false));

    Instruction {
        program_id: crate::id(),
        accounts,
        data: YdeltaInstruction::CheckLtvLiquidatable.to_vec(),
    }
}

/// Build a `CheckMaturityLiquidatable` ix. The loan is past-grace-and-
/// settleable iff the simulated tx returns `Ok`. No collateral side
/// needed — the maturity gate is time-only.
pub fn check_maturity_liquidatable_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    sequence: u64,
    debt_bank: &Pubkey,
    marginfi_program: &Pubkey,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let borrower_marginfi = get_borrower_integration_account_address(market).0;

    let accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(borrower_marginfi, false),
        AccountMeta::new_readonly(*debt_bank, false),
        AccountMeta::new_readonly(*marginfi_program, false),
    ];

    Instruction {
        program_id: crate::id(),
        accounts,
        data: YdeltaInstruction::CheckMaturityLiquidatable.to_vec(),
    }
}
