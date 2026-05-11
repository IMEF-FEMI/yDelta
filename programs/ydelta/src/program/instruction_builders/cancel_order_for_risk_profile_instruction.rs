use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::cancel_order_for_risk_profile::CancelOrderForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a `CancelOrderForRiskProfile` ix. Curator-gated. Removes both the
/// market-side `RestingOrder` and the vault-side `RiskProfileOrderRef`.
pub fn cancel_order_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::CancelOrderForRiskProfile.to_vec();
    CancelOrderForRiskProfileParams { profile_id }
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
