//! Builds the permissionless `MatchCrank` instruction (v1 D7/D8).

use borsh::BorshSerialize;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::match_crank::MatchCrankParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::vault::global_vault_pda;

/// Builds `MatchCrank` for `market`. Anyone may sign.
#[allow(clippy::too_many_arguments)]
pub fn match_crank_instruction(
    bank: &Pubkey,
    market: &Pubkey,
    payer: &Pubkey,
    debt_bank: &Pubkey,
    marginfi_group: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    max_fills: u32,
) -> Instruction {
    let (vault, _) = global_vault_pda(bank);
    let mut data = YdeltaInstruction::MatchCrank.to_vec();
    MatchCrankParams { max_fills }.serialize(&mut data).unwrap();
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
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
    // The take path may realloc the market (block expansion), which
    // needs the system program present in the tx.
    accounts.push(AccountMeta::new_readonly(system_program::id(), false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
