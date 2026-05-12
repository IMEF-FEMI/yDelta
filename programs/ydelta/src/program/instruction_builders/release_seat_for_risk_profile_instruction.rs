use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::release_seat_for_risk_profile::ReleaseSeatForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a curator-gated `ReleaseSeatForRiskProfile` instruction.
/// Same split-payer layout as the other risk-profile ixs:
/// `fee_payer` pays tx fee, `curator` signs as the profile's curator.
pub fn release_seat_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::ReleaseSeatForRiskProfile.to_vec();
    ReleaseSeatForRiskProfileParams { profile_id }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*fee_payer, true),
            AccountMeta::new_readonly(*curator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
