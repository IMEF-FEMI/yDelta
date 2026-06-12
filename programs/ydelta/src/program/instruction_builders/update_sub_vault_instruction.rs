//! Builds the `YdeltaInstruction::UpdateSubVault` instruction:
//! vault-admin patches a profile's mutable policy fields. Rejected on
//! sunset profiles.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::update_sub_vault::UpdateSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `UpdateSubVault` instruction for the vault keyed by
/// `bank`. `payer` (signer) must be the sub-vault's CURATOR (v1 D15 —
/// the owner, for Private sub-vaults). `None` fields are left
/// unchanged; the resulting LTV pair must satisfy the liq-gap invariant.
pub fn update_sub_vault_instruction(
    bank: &Pubkey,
    payer: &Pubkey,
    sub_vault_id: u16,
    new_max_ltv_bps: Option<u16>,
    new_liquidation_ltv_bps: Option<u16>,
    new_max_term_seconds: Option<u32>,
    new_spread_bps: Option<u16>,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);

    let mut data = YdeltaInstruction::UpdateSubVault.to_vec();
    UpdateSubVaultParams {
        sub_vault_id,
        new_max_ltv_bps,
        new_liquidation_ltv_bps,
        new_max_term_seconds,
        new_spread_bps,
    }
    .serialize(&mut data)
    .unwrap();

    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
