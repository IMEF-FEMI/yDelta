use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_error::ProgramError,
    pubkey::Pubkey,
    system_instruction, system_program,
    sysvar::rent::Rent,
};

use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::MARKET_FIXED_SIZE;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address, get_vault_address,
};

/// Derive the per-market lender-side marginfi-account PDA.
pub fn derive_lender_marginfi_account(market: &Pubkey) -> Pubkey {
    get_lender_integration_account_address(market).0
}

/// Derive the per-market borrower-side marginfi-account PDA.
pub fn derive_borrower_marginfi_account(market: &Pubkey) -> Pubkey {
    get_borrower_integration_account_address(market).0
}

/// Alias for the lender-side marginfi-account PDA. New callers should
/// use `derive_lender_marginfi_account` directly.
pub fn derive_marginfi_account(market: &Pubkey) -> Pubkey {
    derive_lender_marginfi_account(market)
}

/// Derive the per-market signer PDA.
pub fn derive_market_signer(market: &Pubkey) -> Pubkey {
    get_market_signer_address(market).0
}

/// Create the market account and its `CreateMarket` instruction.
#[allow(clippy::too_many_arguments)]
pub fn create_market_instructions(
    market: &Pubkey,
    debt_mint: &Pubkey,
    collateral_mint: &Pubkey,
    market_creator: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    marginfi_program: &Pubkey,
) -> Result<Vec<Instruction>, ProgramError> {
    let space: usize = MARKET_FIXED_SIZE;
    Ok(vec![
        system_instruction::create_account(
            market_creator,
            market,
            Rent::default().minimum_balance(space),
            space as u64,
            &crate::id(),
        ),
        create_market_instruction(
            market,
            debt_mint,
            collateral_mint,
            market_creator,
            marginfi_group,
            debt_bank,
            collateral_bank,
            marginfi_program,
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
pub fn create_market_instruction(
    market: &Pubkey,
    debt_mint: &Pubkey,
    collateral_mint: &Pubkey,
    market_creator: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    marginfi_program: &Pubkey,
) -> Instruction {
    let (debt_vault, _) = get_vault_address(market, debt_mint);
    let (collateral_vault, _) = get_vault_address(market, collateral_mint);
    let lender_marginfi_account = derive_lender_marginfi_account(market);
    let borrower_marginfi_account = derive_borrower_marginfi_account(market);
    let market_signer = derive_market_signer(market);
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*market_creator, true),
            AccountMeta::new_readonly(global_config_pda().0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*debt_mint, false),
            AccountMeta::new_readonly(*collateral_mint, false),
            AccountMeta::new(debt_vault, false),
            AccountMeta::new(collateral_vault, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(spl_token_2022::id(), false),
            // Integration accounts.
            AccountMeta::new_readonly(*marginfi_group, false),
            AccountMeta::new_readonly(*debt_bank, false),
            AccountMeta::new_readonly(*collateral_bank, false),
            // Two marginfi-accounts — both writable because marginfi
            // initialises them via inner CPI in `process_create_market`.
            AccountMeta::new(lender_marginfi_account, false),
            AccountMeta::new(borrower_marginfi_account, false),
            // market_signer: PDA passed through so the CPI can resolve
            // it by pubkey. Read-only; ydelta signs via invoke_signed.
            AccountMeta::new_readonly(market_signer, false),
            AccountMeta::new_readonly(*marginfi_program, false),
        ],
        data: YdeltaInstruction::CreateMarket.to_vec(),
    }
}
