use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::validation::get_borrower_integration_account_address;

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
