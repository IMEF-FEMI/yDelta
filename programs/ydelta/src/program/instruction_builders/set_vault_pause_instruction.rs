//! Builds the `YdeltaInstruction::SetVaultPause` instruction: vault-admin
//! flips `GlobalVaultFixed.is_paused`.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::set_vault_pause::SetVaultPauseParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;

/// Builds the `SetVaultPause` instruction. `admin` (signer) must equal the
/// vault admin; `vault` is the `GlobalVaultFixed` PDA. `paused` is
/// serialized as 1/0.
pub fn set_vault_pause_instruction(vault: &Pubkey, admin: &Pubkey, paused: bool) -> Instruction {
    let mut data = YdeltaInstruction::SetVaultPause.to_vec();
    SetVaultPauseParams {
        paused: if paused { 1 } else { 0 },
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*vault, false),
        ],
        data,
    }
}
