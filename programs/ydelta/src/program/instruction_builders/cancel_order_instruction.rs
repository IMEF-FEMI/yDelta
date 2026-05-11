use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::cancel_order::CancelOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

/// Build a `CancelOrder` instruction.
pub fn cancel_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence_number: u64,
    order_index_hint: Option<DataIndex>,
    seat_index_hint: Option<DataIndex>,
    secondary_loan: Option<Pubkey>,
) -> Instruction {
    let mut data = YdeltaInstruction::CancelOrder.to_vec();
    CancelOrderParams {
        order_sequence_number,
        order_index_hint,
        seat_index_hint,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new_readonly(system_program::id(), false),
        // Signer's UserAccount + system_program (the OrderContext
        // loader reads two trailing accounts; the first is the
        // UserAccount PDA, the second is system_program again so
        // auto-create can run if needed).
        AccountMeta::new(user_account_pda(payer).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    if let Some(loan_pda) = secondary_loan {
        accounts.push(AccountMeta::new(loan_pda, false));
    }
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
