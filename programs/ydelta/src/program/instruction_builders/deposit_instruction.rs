//! Builds the `YdeltaInstruction::Deposit` instruction: moves atoms from a
//! trader's wallet ATA into the signer's market seat (debt-side or
//! collateral-side, selected by the token-account mint).

use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::deposit::DepositParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address,
};

/// Builds the `Deposit` instruction for `market`. `payer` (signer) owns
/// the seat. `mint` is the side being deposited (debt vs collateral, picked
/// against `debt_mint`); `bank` + `liquidity_vault` are the matching
/// marginfi accounts. `amount_atoms` is the deposit in token atoms;
/// `trader_index_hint` is an optional seat-tree index to skip the lookup.
#[allow(clippy::too_many_arguments)]
pub fn deposit_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    debt_mint: &Pubkey,
    trader_token: &Pubkey,
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    bank: &Pubkey,
    liquidity_vault: &Pubkey,
    marginfi_program: &Pubkey,
    amount_atoms: u64,
    trader_index_hint: Option<DataIndex>,
) -> Instruction {
    let marginfi_account = if mint == debt_mint {
        get_lender_integration_account_address(market).0
    } else {
        get_borrower_integration_account_address(market).0
    };
    let market_signer = get_market_signer_address(market).0;
    let (vault, _) = get_vault_address(market, mint);
    let mut data = YdeltaInstruction::Deposit.to_vec();
    DepositParams {
        amount_atoms,
        trader_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new(*trader_token, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new(marginfi_account, false),
            AccountMeta::new(*bank, false),
            AccountMeta::new(*liquidity_vault, false),
            AccountMeta::new_readonly(market_signer, false),
            AccountMeta::new_readonly(*marginfi_program, false),
            AccountMeta::new(user_account_pda(payer).0, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
