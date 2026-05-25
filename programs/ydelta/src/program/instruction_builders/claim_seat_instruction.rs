//! Builds the `YdeltaInstruction::ClaimSeat` instruction: inserts the
//! signer's per-market `ClaimedSeat` and auto-creates their
//! `UserAccountFixed` PDA on first call.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

/// Builds the `ClaimSeat` instruction. `payer` (signer) becomes the seat
/// owner on `market`.
pub fn claim_seat_instruction(market: &Pubkey, payer: &Pubkey) -> Instruction {
    let (user_account, _) = user_account_pda(payer);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(user_account, false),
        ],
        data: YdeltaInstruction::ClaimSeat.to_vec(),
    }
}
