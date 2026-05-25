//! Builds the `YdeltaInstruction::SunsetRiskProfile` instruction: vault-admin
//! flips `profile.is_sunset = 1`, disabling new deposits / orders / matches
//! while leaving withdrawals and curator cleanups enabled.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::sunset_risk_profile::SunsetRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `SunsetRiskProfile` instruction for the vault keyed by
/// `mint`. `payer` (signer) must be the vault admin; targets `profile_id`.
pub fn sunset_risk_profile_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::SunsetRiskProfile.to_vec();
    SunsetRiskProfileParams { profile_id }
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
