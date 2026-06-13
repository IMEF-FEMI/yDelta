//! Builds the `YdeltaInstruction::PlaceOrderForSubVault` instruction:
//! curator rests an unbounded vault ask on a market, auto-creating the
//! per-(sub-vault, market) seat and `SubVaultOrderRef` on first call.

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::place_order_for_sub_vault::PlaceOrderForSubVaultParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds the `PlaceOrderForSubVault` instruction. `curator` must sign;
/// `fee_payer` covers tx fees.: takes NO rate or term — the
/// processor computes `live bank lending APR + sub_vault.spread_bps`
/// and uses `sub_vault.max_term_seconds`; `debt_bank` + `marginfi_group`
/// must match the market header. `flags` carries the order bitmask.
#[allow(clippy::too_many_arguments)]
pub fn place_order_for_sub_vault_instruction(
    bank: &Pubkey,
    market: &Pubkey,
    fee_payer: &Pubkey,
    curator: &Pubkey,
    debt_bank: &Pubkey,
    marginfi_group: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    sub_vault_id: u16,
    flags: u8,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::PlaceOrderForSubVault.to_vec();
    PlaceOrderForSubVaultParams {
        sub_vault_id,
        flags,
    }
    .serialize(&mut data)
    .unwrap();
    let mut accounts = vec![
            AccountMeta::new(*fee_payer, true),
            AccountMeta::new_readonly(*curator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*debt_bank, false),
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new_readonly(*collateral_bank, false),
        ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.push(AccountMeta::new_readonly(system_program::id(), false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
