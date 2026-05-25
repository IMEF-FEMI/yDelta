//! `Deposit` instruction. Moves atoms from a user wallet ATA into the
//! signer's market `ClaimedSeat` (collateral side only — debt-side
//! direct deposit is rejected; debt arrives via loan settlement). The
//! atoms route through marginfi.deposit; the seat is credited in shares.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::logs::{emit_stack, DepositLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::{market_helpers::get_seat_index_with_hint, MarketFixed};
use crate::validation::loaders::DepositContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::{get_mut_dynamic_account, invoke};

/// Parameters for [`process_deposit`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct DepositParams {
    /// Atoms to transfer from the signer's ATA. Must be `> 0`.
    pub amount_atoms: u64,
    /// Optional hint pointing directly at the signer's `ClaimedSeat`
    /// index in the market; falls back to tree lookup when absent.
    pub trader_index_hint: Option<DataIndex>,
}

/// Deposit collateral atoms into the signer's market seat. Transfers
/// from the trader's ATA into the market staging vault, CPIs
/// marginfi.deposit, and credits the seat with the resulting shares.
/// Errors with `InvalidDepositAccounts` on debt-side deposits.
pub fn process_deposit(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = DepositParams::try_from_slice(data)?;
    require!(
        params.amount_atoms > 0,
        YdeltaError::InvalidDepositAccounts,
        "deposit amount must be > 0"
    )?;

    let DepositContext {
        payer,
        market,
        trader_token,
        vault,
        token_program,
        mint,
        is_debt,
        marginfi_group,
        marginfi_account,
        bank,
        liquidity_vault,
        market_signer,
        market_signer_bump,
        marginfi_program,
        user_account_ai,
    } = DepositContext::load(accounts)?;

    require!(
        !is_debt,
        YdeltaError::InvalidDepositAccounts,
        "direct deposit of the debt asset into a market seat is not supported \
         — post collateral; borrowed funds arrive via loan settlement"
    )?;

    let market_key = *market.info.key;
    let mint_key = if is_debt {
        market.get_fixed()?.debt_mint
    } else {
        market.get_fixed()?.collateral_mint
    };

    let vault_before_atoms = vault.get_balance_atoms()?;
    transfer_user_to_vault(
        token_program.info,
        trader_token.info,
        vault.info,
        mint.info,
        payer.info,
        params.amount_atoms,
        mint.mint.decimals,
    )?;
    let received_atoms = vault
        .get_balance_atoms()?
        .checked_sub(vault_before_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    require!(
        received_atoms > 0,
        YdeltaError::InvalidDepositAccounts,
        "staging vault received 0 atoms"
    )?;

    let adapter_accounts = [
        marginfi_group.info.clone(),
        marginfi_account.info.clone(),
        market_signer.clone(),
        bank.info.clone(),
        vault.info.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
    let credited_shares: u128 =
        MarginfiV18Adapter.deposit(&adapter_accounts, received_atoms, &[market_signer_seeds])?;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index = get_seat_index_with_hint(
            da.fixed,
            da.dynamic,
            payer.info.key,
            params.trader_index_hint,
        )?;
        da.deposit_to_seat(seat_index, credited_shares, is_debt)?;
    }

    emit_stack(DepositLog {
        market: market_key,
        trader: *payer.info.key,
        mint: mint_key,
        amount_atoms: received_atoms,
    })?;

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

/// SPL token transfer helper shared with other processors. Routes via
/// `transfer_checked` for token-2022 mints, otherwise `transfer`.
pub(crate) fn transfer_user_to_vault<'info>(
    token_program: &AccountInfo<'info>,
    trader_token: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            trader_token.key,
            mint.key,
            vault.key,
            owner.key,
            &[],
            amount,
            decimals,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                mint.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            trader_token.key,
            vault.key,
            owner.key,
            &[],
            amount,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    }
}
