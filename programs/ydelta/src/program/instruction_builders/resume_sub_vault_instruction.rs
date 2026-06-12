//! Builds the `YdeltaInstruction::ResumeSubVault` instruction: vault-admin
//! escape hatch that flips `profile.is_sunset = 0`, reversing a prior
//! `SunsetSubVault`.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::resume_sub_vault::ResumeSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `ResumeSubVault` instruction for the vault keyed by
/// `mint`. `payer` (signer) must be the vault admin; targets `sub_vault_id`.
pub fn resume_sub_vault_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::ResumeSubVault.to_vec();
    ResumeSubVaultParams { sub_vault_id }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}
