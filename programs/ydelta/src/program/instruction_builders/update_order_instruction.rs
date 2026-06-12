//! Builds the `UpdateOrder` instruction (v1 D6): borrower
//! cancel-and-replaces their resting bid with a new rate / term /
//! expiry; principal and collateral are untouched.

use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::processor::update_order::UpdateOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

/// Builds `UpdateOrder` for `market`. `payer` (signer) must own the bid
/// with `order_sequence`.
pub fn update_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence: u64,
    seat_index_hint: Option<DataIndex>,
    new_rate_bps: u16,
    new_term_seconds: u32,
    new_last_valid_unix_ts: i64,
) -> Instruction {
    let mut data = YdeltaInstruction::UpdateOrder.to_vec();
    UpdateOrderParams {
        order_sequence,
        seat_index_hint,
        new_rate_bps,
        new_term_seconds,
        new_last_valid_unix_ts,
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
