//! Instruction builders for the six admin-transfer instructions.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::admin_transfer::{
    AcceptCuratorParams, TransferCuratorParams, TransferGlobalVaultAdminParams,
    TransferMarketAdminParams,
};
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

pub fn transfer_market_admin_instruction(
    market: &Pubkey,
    current_admin: &Pubkey,
    new_admin: &Pubkey,
) -> Instruction {
    let mut data = YdeltaInstruction::TransferMarketAdmin.to_vec();
    TransferMarketAdminParams {
        new_admin: *new_admin,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*current_admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
        ],
        data,
    }
}

pub fn accept_market_admin_instruction(market: &Pubkey, pending_admin: &Pubkey) -> Instruction {
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*pending_admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
        ],
        data: YdeltaInstruction::AcceptMarketAdmin.to_vec(),
    }
}

pub fn transfer_global_vault_admin_instruction(
    mint: &Pubkey,
    current_admin: &Pubkey,
    new_admin: &Pubkey,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::TransferGlobalVaultAdmin.to_vec();
    TransferGlobalVaultAdminParams {
        new_admin: *new_admin,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*current_admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}

pub fn accept_global_vault_admin_instruction(mint: &Pubkey, pending_admin: &Pubkey) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*pending_admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data: YdeltaInstruction::AcceptGlobalVaultAdmin.to_vec(),
    }
}

pub fn transfer_curator_instruction(
    mint: &Pubkey,
    current_curator: &Pubkey,
    profile_id: u8,
    new_curator: &Pubkey,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::TransferCurator.to_vec();
    TransferCuratorParams {
        profile_id,
        new_curator: *new_curator,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*current_curator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}

pub fn accept_curator_instruction(
    mint: &Pubkey,
    pending_curator: &Pubkey,
    profile_id: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::AcceptCurator.to_vec();
    AcceptCuratorParams { profile_id }
        .serialize(&mut data)
        .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*pending_curator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}
