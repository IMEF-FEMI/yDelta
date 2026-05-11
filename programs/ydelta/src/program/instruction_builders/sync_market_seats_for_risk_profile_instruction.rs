use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::sync_market_seats_for_risk_profile::SyncMarketSeatsForRiskProfileParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Build a permissionless `SyncMarketSeatsForRiskProfile` instruction.
pub fn sync_market_seats_for_risk_profile_instruction(
    payer: &Pubkey,
    debt_mint: &Pubkey,
    profile_id: u8,
    markets: &[Pubkey],
) -> Instruction {
    let (global_vault, _) = global_vault_pda(debt_mint);
    let mut data = YdeltaInstruction::SyncMarketSeatsForRiskProfile.to_vec();
    SyncMarketSeatsForRiskProfileParams { profile_id }
        .serialize(&mut data)
        .unwrap();
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new_readonly(global_vault, false),
    ];
    for m in markets {
        accounts.push(AccountMeta::new(*m, false));
    }
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
