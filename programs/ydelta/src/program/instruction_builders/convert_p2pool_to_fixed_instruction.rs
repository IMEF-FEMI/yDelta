use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::convert_p2pool_to_fixed::ConvertP2PoolToFixedParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::validation::{
    get_borrower_integration_account_address, get_market_signer_address, get_vault_address,
};

#[allow(clippy::too_many_arguments)]
pub fn convert_p2pool_to_fixed_instruction(
    market: &Pubkey,
    borrower: &Pubkey,
    loan_sequence: u64,
    debt_mint: &Pubkey,
    debt_bank: &Pubkey,
    debt_liquidity_vault: &Pubkey,
    debt_bank_lva: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_bank: &Pubkey,
    collateral_oracles: &[Pubkey],
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    marginfi_program: &Pubkey,
    max_acceptable_rate_bps: u16,

    cranker_refund: &Pubkey,
) -> Instruction {
    let borrower_marginfi_account = get_borrower_integration_account_address(market).0;
    let market_signer = get_market_signer_address(market).0;
    let (loan, _) = loan_pda(market, loan_sequence);
    let (market_debt_vault, _) = get_vault_address(market, debt_mint);

    let mut data = YdeltaInstruction::ConvertP2PoolToFixed.to_vec();
    ConvertP2PoolToFixedParams {
        max_acceptable_rate_bps,
    }
    .serialize(&mut data)
    .unwrap();

    let mut accounts = vec![
        AccountMeta::new(*borrower, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new_readonly(solana_program::system_program::id(), false),
        AccountMeta::new(borrower_marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*debt_bank_lva, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.push(AccountMeta::new_readonly(*collateral_bank, false));
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    let (global_vault, _) = crate::state::vault::global_vault_pda(debt_mint);
    let (global_vault_signer, _) = crate::state::vault::global_vault_signer_pda(&global_vault);
    let (global_vault_integration_account, _) =
        crate::state::vault::global_vault_integration_account_pda(&global_vault);
    accounts.extend([
        AccountMeta::new(market_debt_vault, false),
        AccountMeta::new_readonly(*debt_mint, false),
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new(global_vault, false),
        AccountMeta::new_readonly(global_vault_signer, false),
        AccountMeta::new(global_vault_integration_account, false),
    ]);

    accounts.push(AccountMeta::new(*cranker_refund, false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
