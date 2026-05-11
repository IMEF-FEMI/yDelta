use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::claim_seat_for_risk_profile::ClaimSeatForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a vault-admin `ClaimSeatForRiskProfile` instruction.
pub fn claim_seat_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
    max_exposure_atoms: u64,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);

    let mut data = YdeltaInstruction::ClaimSeatForRiskProfile.to_vec();
    ClaimSeatForRiskProfileParams {
        profile_id,
        max_exposure_atoms,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
