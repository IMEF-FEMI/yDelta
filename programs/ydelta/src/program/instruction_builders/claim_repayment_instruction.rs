use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::claim_repayment::ClaimRepaymentParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::loan::loan_pda;
use crate::state::user_account::user_account_pda;

/// Build a `ClaimRepayment` instruction.
#[allow(clippy::too_many_arguments)]
pub fn claim_repayment_instruction(
    market: &Pubkey,
    lender: &Pubkey,
    sequence: u64,
    debt_bank: &Pubkey,
    marginfi_program: &Pubkey,
    cranker_refund: Option<&Pubkey>,
    lender_seat_index_hint: Option<DataIndex>,
) -> Instruction {
    let (loan, _) = loan_pda(market, sequence);
    let mut data = YdeltaInstruction::ClaimRepayment.to_vec();
    ClaimRepaymentParams {
        lender_seat_index_hint,
    }
    .serialize(&mut data)
    .unwrap();

    let mut accounts = vec![
        AccountMeta::new(*lender, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(loan, false),
        AccountMeta::new_readonly(*debt_bank, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        // Lender's UserAccount + system_program (auto-create).
        // Mandatory; cranker_refund (if any) trails after.
        AccountMeta::new(user_account_pda(lender).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    if let Some(refund) = cranker_refund {
        accounts.push(AccountMeta::new(*refund, false));
    }

    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
