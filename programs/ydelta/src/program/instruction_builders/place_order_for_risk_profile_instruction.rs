use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::place_order_for_risk_profile::PlaceOrderForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a `PlaceOrderForRiskProfile` ix. Curator-gated — `payer` must equal
/// `profile.curator`. Inserts both the market-side `RestingOrder`
/// (Ask only, vault is debt-side lender) and the vault-side
/// `RiskProfileOrderRef`. Order is bounded by `vault_seat.max_exposure_atoms`
/// and rests indefinitely until the curator cancels.
#[allow(clippy::too_many_arguments)]
pub fn place_order_for_risk_profile_instruction(
    mint: &Pubkey,
    market: &Pubkey,
    payer: &Pubkey,
    profile_id: u8,
    rate_bps: u16,
    term_seconds: u32,
    flags: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(mint);
    let mut data = YdeltaInstruction::PlaceOrderForRiskProfile.to_vec();
    PlaceOrderForRiskProfileParams {
        profile_id,
        rate_bps,
        term_seconds,
        flags,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
