use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
    global_vault_staging_pda,
};

/// Build a `CreateVault` ix. Allocates the per-mint `GlobalVault`
/// PDA, the per-vault staging SPL token account, and initializes the
/// marginfi `integration_account`. The signer becomes
/// `global_vault_admin`.
///
/// One-shot — only succeeds the first time per `mint`. Subsequent
/// calls hit the "vault already exists" check in the loader.
#[allow(clippy::too_many_arguments)]
pub fn create_vault_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    marginfi_group: &Pubkey,
    lending_pool: &Pubkey,
    marginfi_program: &Pubkey,
    token_program: &Pubkey,
    token_program_22: &Pubkey,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let (global_vault_signer, _) = global_vault_signer_pda(&vault);
    let (integration_account, _) = global_vault_integration_account_pda(&vault);
    let (global_vault_staging, _) = global_vault_staging_pda(&vault);

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(global_vault_signer, false),
            AccountMeta::new(integration_account, false),
            AccountMeta::new(global_vault_staging, false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new_readonly(*token_program_22, false),
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new_readonly(*lending_pool, false),
            AccountMeta::new_readonly(*marginfi_program, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: YdeltaInstruction::CreateVault.to_vec(),
    }
}
