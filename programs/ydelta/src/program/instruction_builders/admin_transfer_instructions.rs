//! Builds the two-step admin-handoff instructions for market, global-vault,
//! and per-sub-vault curator roles (`TransferMarketAdmin` / `AcceptMarketAdmin`
//! / `TransferGlobalVaultAdmin` / `AcceptGlobalVaultAdmin` / `TransferCurator`
//! / `AcceptCurator`).

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

/// Builds the `TransferMarketAdmin` instruction. `current_admin` (signer)
/// stamps `new_admin` into `market.pending_admin`.
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

/// Builds the `AcceptMarketAdmin` instruction. `pending_admin` (signer)
/// finalizes the handoff and becomes the new market admin.
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

/// Builds the `TransferGlobalVaultAdmin` instruction for the vault keyed by
/// `mint`. `current_admin` (signer) stamps `new_admin` into the vault's
/// `pending_admin`.
pub fn transfer_global_vault_admin_instruction(
    bank: &Pubkey,
    current_admin: &Pubkey,
    new_admin: &Pubkey,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
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

/// Builds the `AcceptGlobalVaultAdmin` instruction for the vault keyed by
/// `mint`. `pending_admin` (signer) finalizes the handoff.
pub fn accept_global_vault_admin_instruction(bank: &Pubkey, pending_admin: &Pubkey) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
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

/// Builds the `TransferCurator` instruction for the `sub_vault_id` curator
/// role on the vault keyed by `bank`. `current_curator` (signer) stamps
/// `new_curator` into the sub-vault's `pending_curator`.
pub fn transfer_curator_instruction(
    bank: &Pubkey,
    current_curator: &Pubkey,
    sub_vault_id: u16,
    new_curator: &Pubkey,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::TransferCurator.to_vec();
    TransferCuratorParams {
        sub_vault_id,
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

/// Builds the `AcceptCurator` instruction for the `sub_vault_id` curator role
/// on the vault keyed by `bank`. `pending_curator` (signer) finalizes the
/// handoff.
pub fn accept_curator_instruction(
    bank: &Pubkey,
    pending_curator: &Pubkey,
    sub_vault_id: u16,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::AcceptCurator.to_vec();
    AcceptCuratorParams { sub_vault_id }
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
