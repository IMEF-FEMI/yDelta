use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::update_order::UpdateOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;

#[allow(clippy::too_many_arguments)]
pub fn update_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence_number: u64,
    order_index_hint: Option<DataIndex>,
    seat_index_hint: Option<DataIndex>,
    new_rate_bps: Option<u16>,
    new_term_seconds: Option<u32>,
    new_principal_atoms: Option<u64>,
    new_collateral_atoms: Option<u64>,
    new_last_valid_unix_ts: Option<i64>,
    new_flags: Option<u8>,
) -> Instruction {
    update_order_instruction_full(
        market,
        payer,
        order_sequence_number,
        order_index_hint,
        seat_index_hint,
        new_rate_bps,
        new_term_seconds,
        new_principal_atoms,
        new_collateral_atoms,
        new_last_valid_unix_ts,
        new_flags,
        /*new_asking_price_atoms=*/ None,
    )
}

/// `update_order` for a `SecondaryLoanSale` bid. Only
/// `last_valid_unix_ts` and `flags` are mutable; rate / term /
/// principal / collateral / asking_price are all fixed at placement
/// (par exit) and any attempt to change them is rejected with
/// `SecondaryFieldImmutable`. The mutation is in-place, so the
/// seller doesn't lose price-time priority on an expiry-only touch.
#[allow(clippy::too_many_arguments)]
pub fn update_secondary_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence_number: u64,
    order_index_hint: Option<DataIndex>,
    seat_index_hint: Option<DataIndex>,
    new_last_valid_unix_ts: Option<i64>,
    new_flags: Option<u8>,
) -> Instruction {
    update_order_instruction_full(
        market,
        payer,
        order_sequence_number,
        order_index_hint,
        seat_index_hint,
        None,
        None,
        None,
        None,
        new_last_valid_unix_ts,
        new_flags,
        /*new_asking_price_atoms=*/ None,
    )
}

#[allow(clippy::too_many_arguments)]
fn update_order_instruction_full(
    market: &Pubkey,
    payer: &Pubkey,
    order_sequence_number: u64,
    order_index_hint: Option<DataIndex>,
    seat_index_hint: Option<DataIndex>,
    new_rate_bps: Option<u16>,
    new_term_seconds: Option<u32>,
    new_principal_atoms: Option<u64>,
    new_collateral_atoms: Option<u64>,
    new_last_valid_unix_ts: Option<i64>,
    new_flags: Option<u8>,
    new_asking_price_atoms: Option<u64>,
) -> Instruction {
    let mut data = YdeltaInstruction::UpdateOrder.to_vec();
    UpdateOrderParams {
        order_sequence_number,
        order_index_hint,
        seat_index_hint,
        new_rate_bps,
        new_term_seconds,
        new_principal_atoms,
        new_collateral_atoms,
        new_last_valid_unix_ts,
        new_flags,
        new_asking_price_atoms,
    }
    .serialize(&mut data)
    .unwrap();
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
            // Signer's UserAccount + system_program (the
            // OrderContext loader reads two trailing accounts; the
            // first is the UserAccount PDA, the second is
            // system_program again so auto-create can run if
            // needed).
            AccountMeta::new(user_account_pda(payer).0, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}
