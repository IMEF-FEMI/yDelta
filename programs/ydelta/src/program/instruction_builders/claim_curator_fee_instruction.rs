//! Builds the `YdeltaInstruction::ClaimCuratorFee` instruction: curator
//! withdraws the sub-vault's accumulated curator-fee atoms via marginfi
//! withdraw into `curator_token`.

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

/// Builds the `ClaimCuratorFee` instruction for `sub_vault_id` on the vault
/// keyed by `bank`. `payer` (signer) must equal the sub-vault's curator;
/// fees are routed to `curator_token` via marginfi's `debt_bank` /
/// `liquidity_vault` / oracle path.
#[allow(clippy::too_many_arguments)]
pub fn claim_curator_fee_instruction(
    bank: &Pubkey,
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
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let (global_vault_signer, _) = global_vault_signer_pda(&vault);
    let (global_vault_staging, _) = global_vault_staging_pda(&vault);
    let (vault_integration, _) = global_vault_integration_account_pda(&vault);

    let mut data = YdeltaInstruction::ClaimCuratorFee.to_vec();
    ClaimCuratorFeeParams { sub_vault_id }
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
