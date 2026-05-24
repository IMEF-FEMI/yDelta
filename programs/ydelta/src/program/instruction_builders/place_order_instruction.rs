use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::place_order::PlaceOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;
use crate::validation::token_checkers::get_vault_address;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address,
};

#[allow(clippy::too_many_arguments)]
pub fn place_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    debt_liquidity_vault: &Pubkey,
    debt_bank_liquidity_vault_authority: &Pubkey,
    borrower_debt_token: &Pubkey,
    debt_mint: &Pubkey,
    token_program: &Pubkey,
    marginfi_program: &Pubkey,
    rate_bps: u16,
    term_seconds: u32,
    principal_atoms: u64,
    collateral_atoms: u64,
    flags: u8,
    seat_index_hint: Option<DataIndex>,
) -> Instruction {
    let marginfi_account = get_borrower_integration_account_address(market).0;
    let lender_marginfi_account = get_lender_integration_account_address(market).0;
    let market_debt_vault = get_vault_address(market, debt_mint).0;
    let market_signer = get_market_signer_address(market).0;

    let mut data = YdeltaInstruction::PlaceOrder.to_vec();
    PlaceOrderParams {
        seat_index_hint,
        flags,
        rate_bps,
        term_seconds,
        principal_atoms,
        collateral_atoms,
    }
    .serialize(&mut data)
    .unwrap();

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*collateral_bank, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.extend([
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*debt_bank_liquidity_vault_authority, false),
        AccountMeta::new(*borrower_debt_token, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new(user_account_pda(payer).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new(lender_marginfi_account, false),
        AccountMeta::new(market_debt_vault, false),
    ]);

    let (vault_pk, _) = crate::state::vault::global_vault_pda(debt_mint);
    accounts.push(AccountMeta::new(vault_pk, false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
