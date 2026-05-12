use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::set_seat_max_exposure_for_risk_profile::SetSeatMaxExposureForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a curator-gated `SetSeatMaxExposureForRiskProfile` instruction.
///
/// Account shape mirrors `cancel_order_for_risk_profile` (the loader is
/// shared): split fee_payer + curator signers, then global_config,
/// vault, market, system_program. No rent expansion happens here; the
/// system program slot is kept only for layout symmetry across the
/// risk-profile ix family.
pub fn set_seat_max_exposure_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    profile_id: u8,
    max_exposure_atoms: u64,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::SetSeatMaxExposureForRiskProfile.to_vec();
    SetSeatMaxExposureForRiskProfileParams {
        profile_id,
        max_exposure_atoms,
    }
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
