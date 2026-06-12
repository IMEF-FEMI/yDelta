//! Builds the `YdeltaInstruction::UpdateOrderForSubVault` instruction:
//! curator-gated cancel-and-replace of a vault ask in a single transaction
//! with a fresh order sequence.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::update_order_for_sub_vault::UpdateOrderForSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `UpdateOrderForSubVault` instruction. `curator` must sign;
/// `fee_payer` covers tx fees. Replaces the `sub_vault_id` resting ask on
/// `market` with `new_rate_bps` (basis points), `new_term_seconds`
/// (seconds), and `new_flags`.
#[allow(clippy::too_many_arguments)]
pub fn update_order_for_sub_vault_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    sub_vault_id: u16,
    new_rate_bps: u16,
    new_term_seconds: u32,
    new_flags: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::UpdateOrderForSubVault.to_vec();
    UpdateOrderForSubVaultParams {
        sub_vault_id,
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
