use borsh::BorshSerialize;
use hypertree::DataIndex;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::program::processor::place_order::PlaceOrderParams;
use crate::program::YdeltaInstruction;
use crate::state::global_config::global_config_pda;
use crate::state::user_account::user_account_pda;
use crate::state::{OrderKind, OrderType, Side};
use crate::validation::token_checkers::get_vault_address;
use crate::validation::{
    get_borrower_integration_account_address, get_lender_integration_account_address,
    get_market_signer_address,
};

/// Build a primary `PlaceOrder` instruction.
#[allow(clippy::too_many_arguments)]
pub fn place_order_instruction(
    market: &Pubkey,
    payer: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    debt_liquidity_vault: &Pubkey,
    debt_bank_liquidity_vault_authority: &Pubkey,
    borrower_debt_token: &Pubkey,
    debt_mint: &Pubkey,
    token_program: &Pubkey,
    marginfi_program: &Pubkey,
    side: Side,
    order_type: OrderType,
    rate_bps: u16,
    term_seconds: u32,
    principal_atoms: u64,
    collateral_atoms: u64,
    last_valid_unix_ts: i64,
    flags: u8,
    seat_index_hint: Option<DataIndex>,
    // Borrower-side LTV cap (Bids only; ignored for Asks). `None` →
    // processor defaults to marginfi's init-LTV ceiling so existing
    // callers see no behavior change.
    borrower_ltv_bps: Option<u16>,
) -> Instruction {
    place_order_instruction_full(
        market,
        payer,
        marginfi_group,
        debt_bank,
        collateral_bank,
        debt_oracles,
        collateral_oracles,
        debt_liquidity_vault,
        debt_bank_liquidity_vault_authority,
        borrower_debt_token,
        debt_mint,
        token_program,
        marginfi_program,
        side,
        order_type,
        OrderKind::Primary,
        rate_bps,
        term_seconds,
        principal_atoms,
        collateral_atoms,
        /*asking_price_atoms=*/ 0,
        last_valid_unix_ts,
        flags,
        seat_index_hint,
        /*secondary_loan=*/ None,
        borrower_ltv_bps,
    )
}

/// Build a secondary-loan sale order. Pricing is fixed at par.
#[allow(clippy::too_many_arguments)]
pub fn secondary_place_order_instruction(
    market: &Pubkey,
    seller: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    debt_liquidity_vault: &Pubkey,
    debt_bank_liquidity_vault_authority: &Pubkey,
    borrower_debt_token: &Pubkey,
    debt_mint: &Pubkey,
    token_program: &Pubkey,
    marginfi_program: &Pubkey,
    loan_pda: &Pubkey,
    last_valid_unix_ts: i64,
    flags: u8,
    seat_index_hint: Option<DataIndex>,
) -> Instruction {
    place_order_instruction_full(
        market,
        seller,
        marginfi_group,
        debt_bank,
        collateral_bank,
        debt_oracles,
        collateral_oracles,
        debt_liquidity_vault,
        debt_bank_liquidity_vault_authority,
        borrower_debt_token,
        debt_mint,
        token_program,
        marginfi_program,
        Side::Bid,
        OrderType::Limit,
        OrderKind::SecondaryLoanSale,
        /*rate_bps=*/ 0,
        /*term_seconds=*/ 0,
        /*principal_atoms=*/ 0,
        /*collateral_atoms=*/ 0,
        /*asking_price_atoms=*/ 0, // engine overrides to principal at placement
        last_valid_unix_ts,
        flags,
        seat_index_hint,
        Some(*loan_pda),
        /*borrower_ltv_bps=*/ None,
    )
}

#[allow(clippy::too_many_arguments)]
fn place_order_instruction_full(
    market: &Pubkey,
    payer: &Pubkey,
    marginfi_group: &Pubkey,
    debt_bank: &Pubkey,
    collateral_bank: &Pubkey,
    debt_oracles: &[Pubkey],
    collateral_oracles: &[Pubkey],
    debt_liquidity_vault: &Pubkey,
    debt_bank_liquidity_vault_authority: &Pubkey,
    borrower_debt_token: &Pubkey,
    debt_mint: &Pubkey,
    token_program: &Pubkey,
    marginfi_program: &Pubkey,
    side: Side,
    order_type: OrderType,
    kind: OrderKind,
    rate_bps: u16,
    term_seconds: u32,
    principal_atoms: u64,
    collateral_atoms: u64,
    asking_price_atoms: u64,
    last_valid_unix_ts: i64,
    flags: u8,
    seat_index_hint: Option<DataIndex>,
    secondary_loan: Option<Pubkey>,
    borrower_ltv_bps: Option<u16>,
) -> Instruction {
    let marginfi_account = get_borrower_integration_account_address(market).0;
    let lender_marginfi_account = get_lender_integration_account_address(market).0;
    let market_debt_vault = get_vault_address(market, debt_mint).0;
    let market_signer = get_market_signer_address(market).0;

    let mut data = YdeltaInstruction::PlaceOrder.to_vec();
    PlaceOrderParams {
        seat_index_hint,
        side: side as u8,
        order_type: order_type as u8,
        flags,
        kind: kind as u8,
        rate_bps,
        term_seconds,
        principal_atoms,
        collateral_atoms,
        asking_price_atoms,
        last_valid_unix_ts,
        borrower_ltv_bps,
    }
    .serialize(&mut data)
    .unwrap();

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(global_config_pda().0, false),
        AccountMeta::new(*market, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(*marginfi_group, false),
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new(*debt_bank, false),
        AccountMeta::new(*collateral_bank, false),
    ];
    for o in debt_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    for o in collateral_oracles {
        accounts.push(AccountMeta::new_readonly(*o, false));
    }
    accounts.extend([
        AccountMeta::new_readonly(market_signer, false),
        AccountMeta::new_readonly(*marginfi_program, false),
        AccountMeta::new(*debt_liquidity_vault, false),
        AccountMeta::new_readonly(*debt_bank_liquidity_vault_authority, false),
        AccountMeta::new(*borrower_debt_token, false),
        AccountMeta::new_readonly(*token_program, false),
        // Signer's UserAccount + system_program (auto-create on first
        // signer-side ix).
        AccountMeta::new(user_account_pda(payer).0, false),
        AccountMeta::new_readonly(system_program::id(), false),
        // lender_marginfi_account + market_debt_vault for the P2Pool
        // deposit-back path. `marginfi.borrow` lands residual atoms
        // in `market_debt_vault`, then a follow-up `marginfi.deposit`
        // (signed by `market_signer`) routes them into
        // `lender_marginfi_account` so the borrower's principal
        // earns lender-side yield. The borrower's `ClaimedSeat` gets
        // credited with the asset-share delta and withdraws via
        // `process_withdraw`.
        AccountMeta::new(lender_marginfi_account, false),
        AccountMeta::new(market_debt_vault, false),
    ]);
    if let Some(loan) = secondary_loan {
        // SecondaryLoanSale bid: append the existing Loan PDA as the
        // trailing account. WRITABLE: the processor toggles
        // `LoanFixed.has_resting_secondary_bid` (O(1) duplicate-check
        // counterpart) on every secondary placement.
        accounts.push(AccountMeta::new(loan, false));
    }
    // Vault PDA (writable). Always derivable from `debt_mint`. The
    // processor reads/mutates GlobalVault state to apply match-time
    // encumbrance for any vault-owned makers crossed in this match.
    // If the vault doesn't exist on-chain (no `create_vault` ran for
    // this mint), the loader's data_len check downgrades to `None`
    // and vault crosses are skipped.
    let (vault_pk, _) = crate::state::vault::global_vault_pda(debt_mint);
    accounts.push(AccountMeta::new(vault_pk, false));
    Instruction {
        program_id: crate::id(),
        accounts,
        data,
    }
}
