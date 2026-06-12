//! Builds the `YdeltaInstruction::AdminCancelSubVaultOrder` instruction:
//! vault-admin force-cancel of a sunset profile's resting vault ask on a
//! market (wind-down escape hatch — non-sunset profiles must use
//! `cancel_order_for_sub_vault_instruction`).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::admin_cancel_sub_vault_order::AdminCancelSubVaultOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `AdminCancelSubVaultOrder` instruction. `payer` must be the
/// vault admin; targets `sub_vault_id`'s resting ask on `market` for the
/// vault keyed by `mint`.
pub fn admin_cancel_sub_vault_order_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    payer: &Pubkey,
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::AdminCancelSubVaultOrder.to_vec();
    AdminCancelSubVaultOrderParams { sub_vault_id }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
        ],
        data,
    }
}
