use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

/// Build a permissionless `SyncMarketPosition` instruction for an existing user.
pub fn sync_market_position_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    owner: &Pubkey,
) -> Instruction {
    let (owner_user_account, _) = user_account_pda(owner);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(owner_user_account, false),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new_readonly(*owner, false),
        ],
        data: YdeltaInstruction::SyncMarketPosition.to_vec(),
    }
}
