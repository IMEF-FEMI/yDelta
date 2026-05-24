use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::update_risk_profile::UpdateRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

pub fn update_risk_profile_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
    new_max_ltv_bps: Option<u16>,
    new_max_term_seconds: Option<u32>,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);

    let mut data = YdeltaInstruction::UpdateRiskProfile.to_vec();
    UpdateRiskProfileParams {
        profile_id,
        new_max_ltv_bps,
        new_max_term_seconds,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
