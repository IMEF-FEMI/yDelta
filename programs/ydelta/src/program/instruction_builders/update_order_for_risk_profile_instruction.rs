use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::update_order_for_risk_profile::UpdateOrderForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build an `UpdateOrderForRiskProfile` ix. Curator-gated cancel-and-replace
/// — sequence number is renewed (back of price-time priority).
///
/// Split-payer layout: `fee_payer` covers tx fee + any rent for
/// vault/market dynamic-region expansion (rare on update — the
/// in-flight cancel returns the old block to the free list, which the
/// re-place reuses, so steady-state cost is just the tx fee).
/// `curator` only signs.
#[allow(clippy::too_many_arguments)]
pub fn update_order_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    profile_id: u8,
    new_rate_bps: u16,
    new_term_seconds: u32,
    new_flags: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::UpdateOrderForRiskProfile.to_vec();
    UpdateOrderForRiskProfileParams {
        profile_id,
        new_rate_bps,
        new_term_seconds,
        new_flags,
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
