//! Builds the `YdeltaInstruction::CreateSubVault` instruction: vault-admin
//! appends a new `SubVault` to a vault and the processor auto-assigns a
//! monotonic `sub_vault_id` starting at 1 (0 is the sentinel/invalid id).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::create_sub_vault::CreateSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `CreateSubVault` instruction for the vault keyed by `mint`.
/// `payer` (signer) must be the vault admin. `curator` is stamped as the
/// profile's curator. `max_ltv_bps = Some(n)` sets and enforces an explicit
/// cap (basis points); `None` is rejected by the processor (the
/// marginfi-auto sentinel was removed, v1 D17). `max_term_seconds` caps
/// loan term in seconds.
pub fn create_sub_vault_instruction(
    bank: &Pubkey,
    payer: &Pubkey,
    curator: &Pubkey,
    max_ltv_bps: Option<u16>,
    max_term_seconds: u32,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);

    let mut data = YdeltaInstruction::CreateSubVault.to_vec();
    CreateSubVaultParams {
        curator: *curator,
        max_ltv_bps,
        max_term_seconds,
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
