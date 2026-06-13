//! Builds the `CancelOrder` instruction: borrower cancels their
//! resting bid, releasing its encumbered collateral.

use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::cancel_order::CancelOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

/// Builds `CancelOrder` for `market`. `payer` (signer) must own the bid
/// with `order_sequence`.
pub fn cancel_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence: u64,
    seat_index_hint: Option<DataIndex>,
) -> Instruction {
    let mut data = YdeltaInstruction::CancelOrder.to_vec();
    CancelOrderParams {
        order_sequence,
        seat_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new(user_account_pda(payer).0, false),
        ],
        data,
    }
}
