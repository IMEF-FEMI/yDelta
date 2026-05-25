//! Builds the `YdeltaInstruction::ProcessMatchedLoan` instruction: cranker
//! promotes a queued `MatchedLoan` into a `LoanFixed` PDA, optionally
//! running the vault-side atom migration when `vault_settle` is provided.

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

/// Bundle of vault-side accounts the processor needs when the matched loan
/// is vault-funded. Pass `Some(VaultSettleAddrs { .. })` to
/// `process_matched_loan_instruction` for vault matches; pass `None` for
/// pure P2Pool promotions.
pub struct VaultSettleAddrs {
    /// `GlobalVaultFixed` PDA for the debt mint.
    pub global_vault: Pubkey,
    /// Vault signer PDA (marginfi CPI + staging ATA authority).
    pub global_vault_signer: Pubkey,
    /// Vault's debt-mint staging ATA.
    pub global_vault_staging: Pubkey,
    /// Vault's own marginfi integration account.
    pub global_vault_integration_account: Pubkey,
    /// Market's debt-side token vault.
    pub market_debt_vault: Pubkey,
    /// Market's lender-side marginfi integration account.
    pub market_lender_integration_account: Pubkey,
    /// Market signer PDA.
    pub market_signer: Pubkey,
    /// Debt bank's marginfi liquidity vault.
    pub debt_liquidity_vault: Pubkey,
    /// Debt bank's liquidity-vault authority PDA.
    pub debt_bank_liquidity_vault_authority: Pubkey,
    /// Oracle accounts for the debt bank, in marginfi-expected order.
    pub debt_oracles: Vec<Pubkey>,
    /// Debt token mint.
    pub debt_mint: Pubkey,
    /// SPL token program owning the debt mint.
    pub token_program: Pubkey,
    /// Marginfi group account.
    pub marginfi_group: Pubkey,
    /// Marginfi program id.
    pub marginfi_program: Pubkey,
}

/// Builds the `ProcessMatchedLoan` instruction. `payer` (signer) is the
/// cranker funding the `LoanFixed` rent. `sequence` selects the queued
/// match; `matched_loan_index_hint` skips the matched-loan tree lookup.
/// Pass `vault_settle = Some(..)` for vault-funded matches, `None` for
/// P2Pool promotions.
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
