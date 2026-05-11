use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::state::vault::{
    global_vault_integration_account_pda, global_vault_signer_pda, global_vault_staging_pda,
};
use crate::validation::pdas::get_market_signer_address;
use crate::validation::token_checkers::get_vault_address;

/// Build a permissionless `ClaimRepaymentForRiskProfile` instruction.
///
/// The vault-settle loader (`load_vault_settle_accounts`) reads exactly ONE
/// oracle (`bank.config.primary_oracle()`), so this builder takes a single
/// `bank_oracle` rather than a variadic slice. Passing more than one oracle
/// would misalign the trailing `mint, token_program, marginfi_group,
/// marginfi_program` accounts and is a footgun the prior API allowed.
#[allow(clippy::too_many_arguments)]
pub fn claim_repayment_for_risk_profile_instruction(
    payer: &Pubkey,
    market: &Pubkey,
    sequence: u64,
    global_vault: &Pubkey,
    debt_mint: &Pubkey,
    debt_bank: &Pubkey,
    debt_liquidity_vault: &Pubkey,
    debt_bank_lva: &Pubkey,
    bank_oracle: &Pubkey,
    lender_marginfi_account: &Pubkey,
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    marginfi_program: &Pubkey,
    cranker_refund: Option<&Pubkey>,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let (market_signer, _) = get_market_signer_address(market);
    let (market_debt_vault, _) = get_vault_address(market, debt_mint);
    let (global_vault_signer, _) = global_vault_signer_pda(global_vault);
    let (global_vault_staging, _) = global_vault_staging_pda(global_vault);
    let (global_vault_integration_account, _) = global_vault_integration_account_pda(global_vault);

    let data = YdeltaInstruction::ClaimRepaymentForRiskProfile.to_vec();

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new(*global_vault, false),
        AccountMeta::new_readonly(global_vault_signer, false),
        AccountMeta::new(global_vault_staging, false),
        AccountMeta::new(global_vault_integration_account, false),
        AccountMeta::new(market_debt_vault, false),
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new(*lender_marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*debt_bank_lva, false),
        AccountMeta::new_readonly(*bank_oracle, false),
        AccountMeta::new_readonly(*debt_mint, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new_readonly(*marginfi_program, false),
    ];
    if let Some(refund) = cranker_refund {
        accounts.push(AccountMeta::new(*refund, false));
    }

    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
