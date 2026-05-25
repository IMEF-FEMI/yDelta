//! Builds the `GlobalConfig` admin instructions: `CreateGlobalConfig`,
//! two-step `TransferProtocolAdmin` / `AcceptProtocolAdmin`, and
//! `SetGlobalPause`.

use borsh::BorshSerialize;
use solana_program::{
    bpf_loader_upgradeable,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::global_config_admin::{
    SetGlobalPauseParams, TransferProtocolAdminParams,
};
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;

/// Builds the `CreateGlobalConfig` instruction. `payer` (signer) must be
/// the program's upgrade authority; they become the initial `protocol_admin`.
pub fn create_global_config_instruction(payer: &Pubkey) -> Instruction {
    let (global_config, _) = global_config_pda();
    let (program_data, _) =
        Pubkey::find_program_address(&[crate::ID.as_ref()], &bpf_loader_upgradeable::id());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(global_config, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(program_data, false),
        ],
        data: YdeltaInstruction::CreateGlobalConfig.to_vec(),
    }
}

/// Builds the `TransferProtocolAdmin` instruction. `current_admin` (signer)
/// stamps `new_admin` into `global_config.pending_admin`.
pub fn transfer_protocol_admin_instruction(
    current_admin: &Pubkey,
    new_admin: &Pubkey,
) -> Instruction {
    let (global_config, _) = global_config_pda();
    let mut data = YdeltaInstruction::TransferProtocolAdmin.to_vec();
    TransferProtocolAdminParams {
        new_admin: *new_admin,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*current_admin, true),
            AccountMeta::new(global_config, false),
        ],
        data,
    }
}

/// Builds the `AcceptProtocolAdmin` instruction. `pending_admin` (signer)
/// finalizes the protocol-admin handoff.
pub fn accept_protocol_admin_instruction(pending_admin: &Pubkey) -> Instruction {
    let (global_config, _) = global_config_pda();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*pending_admin, true),
            AccountMeta::new(global_config, false),
        ],
        data: YdeltaInstruction::AcceptProtocolAdmin.to_vec(),
    }
}

/// Builds the `SetGlobalPause` instruction. `admin` (signer) must be the
/// current `protocol_admin`; `paused` flips `GlobalConfig.is_paused` (1/0).
pub fn set_global_pause_instruction(admin: &Pubkey, paused: bool) -> Instruction {
    let (global_config, _) = global_config_pda();
    let mut data = YdeltaInstruction::SetGlobalPause.to_vec();
    SetGlobalPauseParams {
        paused: if paused { 1 } else { 0 },
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new(global_config, false),
        ],
        data,
    }
}
