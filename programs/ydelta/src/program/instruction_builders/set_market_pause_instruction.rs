use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::set_market_pause::SetMarketPauseParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;

pub fn set_market_pause_instruction(market: &Pubkey, admin: &Pubkey, paused: bool) -> Instruction {
    let mut data = YdeltaInstruction::SetMarketPause.to_vec();
    SetMarketPauseParams {
        paused: if paused { 1 } else { 0 },
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
        ],
        data,
    }
}
