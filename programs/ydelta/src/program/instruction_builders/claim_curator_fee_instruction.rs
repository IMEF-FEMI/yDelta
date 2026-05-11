use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::claim_curator_fee::ClaimCuratorFeeParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
    global_vault_staging_pda,
};

/// Build a `ClaimCuratorFee` ix. Curator-gated — `payer` must equal
/// `profile.curator`. Withdraws `accumulated_curator_fee_atoms` from
/// the vault's marginfi account out to the curator's wallet ATA.
#[allow(clippy::too_many_arguments)]
pub fn claim_curator_fee_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    curator_token: &Pubkey,
    debt_bank: &Pubkey,
    liquidity_vault: &Pubkey,
    bank_liquidity_vault_authority: &Pubkey,
    bank_oracle: &Pubkey,
    token_program: &Pubkey,
    marginfi_program: &Pubkey,
    marginfi_group: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let (global_vault_signer, _) = global_vault_signer_pda(&vault);
    let (global_vault_staging, _) = global_vault_staging_pda(&vault);
    let (vault_integration, _) = global_vault_integration_account_pda(&vault);

    let mut data = YdeltaInstruction::ClaimCuratorFee.to_vec();
    ClaimCuratorFeeParams { profile_id }
        .serialize(&mut data)
        .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(global_vault_signer, false),
            AccountMeta::new(global_vault_staging, false),
            AccountMeta::new(vault_integration, false),
            AccountMeta::new(*curator_token, false),
            AccountMeta::new(*debt_bank, false),
            AccountMeta::new(*liquidity_vault, false),
            AccountMeta::new_readonly(*bank_liquidity_vault_authority, false),
            AccountMeta::new_readonly(*bank_oracle, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*token_program, false),
            AccountMeta::new_readonly(*marginfi_program, false),
            AccountMeta::new_readonly(*marginfi_group, false),
        ],
        data,
    }
}
