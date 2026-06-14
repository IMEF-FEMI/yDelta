//! Builds the `YdeltaInstruction::GlobalVaultDeposit` instruction: lender
//! moves atoms from their wallet ATA into a sub-vault, minting sub_vault
//! shares against the vault NAV.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::global_vault_deposit::GlobalVaultDepositParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;
use crate::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
    global_vault_staging_pda,
};

/// Builds the `GlobalVaultDeposit` instruction for the vault keyed by
/// `mint`. `depositor` (signer) is the lender; `depositor_token` is their
/// wallet ATA. `lending_pool` + `liquidity_vault` are the marginfi bank
/// accounts the deposit flows into. `sub_vault_id` selects the target
/// `SubVault`; `amount_atoms` is the deposit in token atoms.
#[allow(clippy::too_many_arguments)]
pub fn global_vault_deposit_instruction(
    bank: &Pubkey,
    mint: &Pubkey,
    depositor: &Pubkey,
    depositor_token: &Pubkey,
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    lending_pool: &Pubkey,
    liquidity_vault: &Pubkey,
    marginfi_program: &Pubkey,
    sub_vault_id: u16,
    amount_atoms: u64,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let (global_vault_signer, _) = global_vault_signer_pda(&vault);
    let (global_vault_staging, _) = global_vault_staging_pda(&vault);
    let (integration_account, _) = global_vault_integration_account_pda(&vault);
    let (user_account, _) = user_account_pda(depositor);

    let mut data = YdeltaInstruction::GlobalVaultDeposit.to_vec();
    GlobalVaultDepositParams {
        amount_atoms,
        sub_vault_id,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*depositor, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(global_vault_signer, false),
            AccountMeta::new(global_vault_staging, false),
            AccountMeta::new(*depositor_token, false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new(integration_account, false),
            AccountMeta::new(*lending_pool, false),
            AccountMeta::new(*liquidity_vault, false),
            AccountMeta::new_readonly(*marginfi_program, false),
            AccountMeta::new(user_account, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
