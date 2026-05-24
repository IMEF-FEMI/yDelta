use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::repay::RepayParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::state::user_account::user_account_pda;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address,
};

#[allow(clippy::too_many_arguments)]
pub fn repay_instruction(
    market: &Pubkey,
    borrower: &Pubkey,
    sequence: u64,
    debt_mint: &Pubkey,
    borrower_token: &Pubkey,
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    debt_liquidity_vault: &Pubkey,
    collateral_bank: &Pubkey,
    marginfi_program: &Pubkey,
    repay_atoms: u64,
    full_repay: bool,
    borrower_seat_index_hint: Option<DataIndex>,
    cranker_refund: &Pubkey,
    // REQUIRED for Fixed loans (the processor uses it on full repay to
    // apply per-loan risk-profile decrements + bump pending_claim atoms).
    // MUST be `None` for P2Pool repays — the loader only consumes this
    // slot when the loan PDA reads as LoanType::Fixed.
    global_vault: Option<&Pubkey>,
) -> Instruction {
    let marginfi_account = get_lender_integration_account_address(market).0;
    let borrower_marginfi_account = get_borrower_integration_account_address(market).0;
    let market_signer = get_market_signer_address(market).0;
    let (loan, _) = loan_pda(market, sequence);
    let (vault, _) = get_vault_address(market, debt_mint);

    let mut data = YdeltaInstruction::Repay.to_vec();
    RepayParams {
        repay_atoms,
        full_repay,
        borrower_seat_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
        AccountMeta::new(*borrower, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(*borrower_token, false),
        AccountMeta::new(vault, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*debt_mint, false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*collateral_bank, false),
        AccountMeta::new(borrower_marginfi_account, false),
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new(user_account_pda(borrower).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new(*cranker_refund, false),
    ];
    if let Some(gv) = global_vault {
        accounts.push(AccountMeta::new(*gv, false));
    }
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
