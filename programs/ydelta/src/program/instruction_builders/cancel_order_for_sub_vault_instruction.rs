//! Builds the `YdeltaInstruction::CancelOrderForSubVault` instruction:
//! curator-gated cancel of a vault ask on a given market.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::cancel_order_for_sub_vault::CancelOrderForSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `CancelOrderForSubVault` instruction. `curator` must sign;
/// `fee_payer` covers tx fees. Removes the `sub_vault_id` resting ask on
/// `market` for the vault keyed by `bank`.
pub fn cancel_order_for_sub_vault_instruction(
    bank: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::CancelOrderForSubVault.to_vec();
    CancelOrderForSubVaultParams { sub_vault_id }
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
