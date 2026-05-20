use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::process_matched_loan::ProcessMatchedLoanParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;

/// Extra accounts needed when the lender seat is owned by a risk profile.
pub struct VaultSettleAddrs {
    pub global_vault: Pubkey,
    pub global_vault_signer: Pubkey,
    pub global_vault_staging: Pubkey,
    pub global_vault_integration_account: Pubkey,
    pub market_debt_vault: Pubkey,
    pub market_lender_integration_account: Pubkey,
    pub market_signer: Pubkey,
    pub debt_liquidity_vault: Pubkey,
    pub debt_bank_liquidity_vault_authority: Pubkey,
    pub debt_oracles: Vec<Pubkey>,
    pub debt_mint: Pubkey,
    pub token_program: Pubkey,
    pub marginfi_group: Pubkey,
    pub marginfi_program: Pubkey,
}

/// Build a permissionless `ProcessMatchedLoan` instruction for a primary match.
#[allow(clippy::too_many_arguments)]
pub fn process_matched_loan_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    debt_bank: &Pubkey,
    marginfi_program: &Pubkey,
    sequence: u64,
    matched_loan_index_hint: Option<DataIndex>,
    vault_settle: Option<VaultSettleAddrs>,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let mut data = YdeltaInstruction::ProcessMatchedLoan.to_vec();
    ProcessMatchedLoanParams {
        matched_loan_sequence: sequence,
        matched_loan_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    if let Some(va) = vault_settle {
        accounts.push(AccountMeta::new(va.global_vault, false));
        accounts.push(AccountMeta::new_readonly(va.global_vault_signer, false));
        accounts.push(AccountMeta::new(va.global_vault_staging, false));
        accounts.push(AccountMeta::new(va.global_vault_integration_account, false));
        accounts.push(AccountMeta::new(va.market_debt_vault, false));
        accounts.push(AccountMeta::new(
            va.market_lender_integration_account,
            false,
        ));
        accounts.push(AccountMeta::new_readonly(va.market_signer, false));
        accounts.push(AccountMeta::new(va.debt_liquidity_vault, false));
        accounts.push(AccountMeta::new_readonly(
            va.debt_bank_liquidity_vault_authority,
            false,
        ));
        for o in &va.debt_oracles {
            accounts.push(AccountMeta::new_readonly(*o, false));
        }
        accounts.push(AccountMeta::new_readonly(va.debt_mint, false));
        accounts.push(AccountMeta::new_readonly(va.token_program, false));
        accounts.push(AccountMeta::new_readonly(va.marginfi_group, false));
        accounts.push(AccountMeta::new_readonly(va.marginfi_program, false));
    }
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
