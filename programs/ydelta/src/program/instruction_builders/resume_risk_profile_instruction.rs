//! Builds the `YdeltaInstruction::ResumeRiskProfile` instruction: vault-admin
//! escape hatch that flips `profile.is_sunset = 0`, reversing a prior
//! `SunsetRiskProfile`.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::resume_risk_profile::ResumeRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `ResumeRiskProfile` instruction for the vault keyed by
/// `mint`. `payer` (signer) must be the vault admin; targets `profile_id`.
pub fn resume_risk_profile_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::ResumeRiskProfile.to_vec();
    ResumeRiskProfileParams { profile_id }
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
