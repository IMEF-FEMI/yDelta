//! Builds the `YdeltaInstruction::ProtocolFeeClaim` instruction: market admin
//! drains `market.accumulated_protocol_fee_shares` to their debt ATA via a
//! marginfi withdraw.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::validation::{
    get_lender_integration_account_address, get_market_signer_address, get_vault_address,
};

/// Builds the `ProtocolFeeClaim` instruction. `admin` (signer) must equal
/// the market admin; accumulated protocol fees are withdrawn into
/// `admin_debt_token` via the marginfi `debt_bank` /
/// `debt_liquidity_vault` / `debt_bank_lva` / `debt_oracles` path.
#[allow(clippy::too_many_arguments)]
pub fn protocol_fee_claim_instruction(
    market: &Pubkey,
    admin: &Pubkey,
    debt_mint: &Pubkey,
    admin_debt_token: &Pubkey,
    debt_bank: &Pubkey,
    debt_liquidity_vault: &Pubkey,
    debt_bank_lva: &Pubkey,
    debt_oracles: &[Pubkey],
    token_program: &Pubkey,
    marginfi_group: &Pubkey,
    marginfi_program: &Pubkey,
) -> Instruction {
    let (market_debt_vault, _) = get_vault_address(market, debt_mint);
    let (market_signer, _) = get_market_signer_address(market);
    let (lender_marginfi, _) = get_lender_integration_account_address(market);

    let mut accounts = vec![
        AccountMeta::new(*admin, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new(*admin_debt_token, false),
        AccountMeta::new(market_debt_vault, false),
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new(lender_marginfi, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*debt_bank_lva, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.extend([
        AccountMeta::new_readonly(*debt_mint, false),
        AccountMeta::new_readonly(*token_program, false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new_readonly(*marginfi_program, false),
    ]);
    Instruction {
        program_id: crate::id(),
        accounts,
        data: YdeltaInstruction::ProtocolFeeClaim.to_vec(),
    }
}
