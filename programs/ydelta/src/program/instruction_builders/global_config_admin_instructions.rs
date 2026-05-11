//! Instruction builders for `GlobalConfig` lifecycle instructions.

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
            // BpfLoaderUpgradeable ProgramData account; the loader
            // binds payer == upgrade_authority on this account.
            AccountMeta::new_readonly(program_data, false),
        ],
        data: YdeltaInstruction::CreateGlobalConfig.to_vec(),
    }
}

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
