use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::withdraw::WithdrawParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address,
};

/// Build a `Withdraw` instruction.
#[allow(clippy::too_many_arguments)]
pub fn withdraw_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    debt_mint: &Pubkey,
    trader_token: &Pubkey,
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    bank_being_withdrawn: &Pubkey,
    liquidity_vault: &Pubkey,
    bank_liquidity_vault_authority: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    marginfi_program: &Pubkey,
    amount_atoms: u64,
    trader_index_hint: Option<DataIndex>,
) -> Instruction {
    let _ = bank_being_withdrawn; // documented for callers; the side is
                                  // disambiguated from trader_token's mint.
                                  // Debt withdrawals come from the lender-side account; collateral
                                  // withdrawals from the borrower-side account.
    let marginfi_account = if mint == debt_mint {
        get_lender_integration_account_address(market).0
    } else {
        get_borrower_integration_account_address(market).0
    };
    let market_signer = get_market_signer_address(market).0;
    let (vault, _) = get_vault_address(market, mint);
    let mut data = YdeltaInstruction::Withdraw.to_vec();
    WithdrawParams {
        amount_atoms,
        trader_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(*trader_token, false),
        // The market's per-mint staging vault, owned by market_signer.
        AccountMeta::new(vault, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*mint, false),
        // Integration accounts.
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*collateral_bank, false),
        AccountMeta::new(*liquidity_vault, false),
        AccountMeta::new_readonly(*bank_liquidity_vault_authority, false),
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
        // Signer's UserAccount + system_program (for auto-create
        // on first interaction).
        AccountMeta::new(user_account_pda(payer).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ]);
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
