//! Builds the `YdeltaInstruction::RemoveSubVault` instruction: vault-admin
//! removes a `SubVault` from the vault tree (only allowed after the
//! sub_vault has been sunset and wound down).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::remove_sub_vault::RemoveSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `RemoveSubVault` instruction for the vault keyed by
/// `mint`. `payer` (signer) must be the vault admin; the target
/// `sub_vault_id` must already have `is_sunset == 1`.
pub fn remove_sub_vault_instruction(
    bank: &Pubkey,
    payer: &Pubkey,
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);

    let mut data = YdeltaInstruction::RemoveSubVault.to_vec();
    RemoveSubVaultParams { sub_vault_id }
        .serialize(&mut data)
        .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}
