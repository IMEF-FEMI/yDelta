//! Builds the `YdeltaInstruction::CreateRiskProfile` instruction: vault-admin
//! appends a new `RiskProfile` to a vault and the processor auto-assigns a
//! monotonic `profile_id` starting at 1 (0 is the sentinel/invalid id).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::create_risk_profile::CreateRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `CreateRiskProfile` instruction for the vault keyed by `mint`.
/// `payer` (signer) must be the vault admin. `curator` is stamped as the
/// profile's curator. `max_ltv_bps = Some(n)` sets and enforces an explicit
/// cap (basis points); `None` is rejected by the processor (the
/// marginfi-auto sentinel was removed, v1 D17). `max_term_seconds` caps
/// loan term in seconds.
pub fn create_risk_profile_instruction(
    mint: &Pubkey,
    payer: &Pubkey,
    curator: &Pubkey,
    max_ltv_bps: Option<u16>,
    max_term_seconds: u32,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);

    let mut data = YdeltaInstruction::CreateRiskProfile.to_vec();
    CreateRiskProfileParams {
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
