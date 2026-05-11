use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::create_risk_profile::CreateRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a vault-admin `CreateRiskProfile` instruction.
pub fn create_risk_profile_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
    curator: &Pubkey,
    max_ltv_bps: u16,
    max_term_seconds: u32,
    allowed_market_max: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);

    let mut data = YdeltaInstruction::CreateRiskProfile.to_vec();
    CreateRiskProfileParams {
        profile_id,
        curator: *curator,
        max_ltv_bps,
        max_term_seconds,
        allowed_market_max,
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
