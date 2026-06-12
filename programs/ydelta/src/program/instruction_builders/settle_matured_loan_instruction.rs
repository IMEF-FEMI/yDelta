//! Builds the `YdeltaInstruction::SettleMaturedLoan` instruction:
//! permissionless keeper repays up to `repay_atoms_max` of a loan that's
//! past `matures_at + grace` and seizes proportional collateral; full
//! repay closes the loan PDA.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address,
};

/// Builds the `SettleMaturedLoan` instruction for loan `(market, sequence)`.
/// `payer` (signer) is the settler; `liquidator_debt_token` funds the repay
/// and `liquidator_collateral_token` receives collateral. `repay_atoms_max`
/// caps the repay in debt-token atoms. `global_vault` must be `Some` for
/// Fixed loans and `None` for P2Pool loans.
#[allow(clippy::too_many_arguments)]
pub fn settle_matured_loan_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    sequence: u64,
    debt_mint: &Pubkey,
    collateral_mint: &Pubkey,
    liquidator_debt_token: &Pubkey,
    liquidator_collateral_token: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_liquidity_vault: &Pubkey,
    collateral_liquidity_vault: &Pubkey,
    collateral_bank_lva: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    marginfi_program: &Pubkey,
    repay_atoms_max: u64,
    cranker_refund: &Pubkey,
    // REQUIRED for Fixed loans (full-settle close-out updates the lender
    // vault's sub-vault + bumps pending_claim). MUST be `None` for
    // P2Pool settlements — the loader only consumes this slot for Fixed.
    global_vault: Option<&Pubkey>,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let (market_debt_vault, _) = get_vault_address(market, debt_mint);
    let (market_collateral_vault, _) = get_vault_address(market, collateral_mint);
    let (market_signer, _) = get_market_signer_address(market);
    let (lender_marginfi, _) = get_lender_integration_account_address(market);
    let (borrower_marginfi, _) = get_borrower_integration_account_address(market);

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(*liquidator_debt_token, false),
        AccountMeta::new(*liquidator_collateral_token, false),
        AccountMeta::new(market_debt_vault, false),
        AccountMeta::new(market_collateral_vault, false),
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new(lender_marginfi, false),
        AccountMeta::new(borrower_marginfi, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*collateral_bank, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new(*collateral_liquidity_vault, false),
        AccountMeta::new_readonly(*collateral_bank_lva, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.extend([
        AccountMeta::new_readonly(*debt_mint, false),
        AccountMeta::new_readonly(*collateral_mint, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new(*cranker_refund, false),
    ]);
    if let Some(gv) = global_vault {
        accounts.push(AccountMeta::new(*gv, false));
    }
    Instruction {
        program_id: crate::id(),
        accounts,
        data: {
            let mut d = YdeltaInstruction::SettleMaturedLoan.to_vec();
            d.extend_from_slice(&repay_atoms_max.to_le_bytes());
            d
        },
    }
}
