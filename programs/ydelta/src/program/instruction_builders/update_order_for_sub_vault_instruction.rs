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
/// `fee_payer` covers tx fees. Parameterless re-sync (v1 D4): cancels the
/// `sub_vault_id` resting ask on `market` and re-rests it at `live bank
/// lending APR + sub_vault.spread_bps` for `sub_vault.max_term_seconds`,
/// carrying `new_flags`. Takes no rate or term argument.
#[allow(clippy::too_many_arguments)]
pub fn update_order_for_sub_vault_instruction(
    bank: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    debt_bank: &Pubkey,
    marginfi_group: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    sub_vault_id: u16,
    new_flags: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::UpdateOrderForSubVault.to_vec();
    UpdateOrderForSubVaultParams {
        sub_vault_id,
        new_flags,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
            AccountMeta::new(*fee_payer, true),
            AccountMeta::new_readonly(*curator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*debt_bank, false),
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new_readonly(*collateral_bank, false),
        ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.push(AccountMeta::new_readonly(system_program::id(), false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
