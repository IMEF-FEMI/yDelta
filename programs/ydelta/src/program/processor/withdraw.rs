//! `Withdraw` — trader-side debt or collateral withdrawal from their
//! `ClaimedSeat`. Signer is the seat owner (payer). Drains either the
//! seat's withdrawable share balance (`withdraw_all`) or the exact
//! `amount_atoms`, decrements the seat's per-side balance, withdraws from
//! the per-market marginfi integration account through the market signer,
//! and pays out the user via SPL token transfer.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use marginfi_mocks::state::MarginfiAccount;

use crate::logs::{emit_stack, WithdrawLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::{market_helpers::get_seat_index_with_hint, MarketFixed};
use crate::validation::loaders::WithdrawContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

/// Withdrawal parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct WithdrawParams {
    /// Atoms to withdraw; ignored when `withdraw_all == true`. Must be `> 0` otherwise.
    pub amount_atoms: u64,
    /// Optional `ClaimedSeat` hint for the trader; falls back to lookup if stale.
    pub trader_index_hint: Option<DataIndex>,

    /// When true, withdraws the trader's entire withdrawable share balance on this side.
    pub withdraw_all: bool,
}

/// Withdraw from the trader's `ClaimedSeat` on either the debt or collateral
/// side (determined by which mint matches the supplied bank).
pub fn process_withdraw(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = WithdrawParams::try_from_slice(data)?;

    if !params.withdraw_all {
        require!(
            params.amount_atoms > 0,
            YdeltaError::InvalidWithdrawAccounts,
            "withdraw amount must be > 0"
        )?;
    }

    let WithdrawContext {
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
        bank_liquidity_vault_authority,
        debt_bank,
        collateral_bank,
        debt_oracle_ais,
        collateral_oracle_ais,
        market_signer,
        market_signer_bump,
        marginfi_program,
        user_account_ai,
    } = WithdrawContext::load(accounts)?;

    let market_key = *market.info.key;
    let mint_key = if is_debt {
        market.get_fixed()?.debt_mint
    } else {
        market.get_fixed()?.collateral_mint
    };

    let expected_shares: u128 = if params.withdraw_all {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index = get_seat_index_with_hint(
            da.fixed,
            da.dynamic,
            payer.info.key,
            params.trader_index_hint,
        )?;
        da.withdrawable_shares_for_seat(seat_index, is_debt)
    } else {
        MarginfiV18Adapter.amount_to_asset_shares(&[bank.info.clone()], params.amount_atoms)?
    };

    require!(
        expected_shares > 0,
        YdeltaError::InsufficientWithdrawableBalance,
        "nothing withdrawable on this side"
    )?;

    let expected_atoms: u64 =
        MarginfiV18Adapter.shares_to_amount(&[bank.info.clone()], expected_shares)?;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index = get_seat_index_with_hint(
            da.fixed,
            da.dynamic,
            payer.info.key,
            params.trader_index_hint,
        )?;

        da.withdraw_from_seat(seat_index, expected_shares, is_debt)?;
    }

    let active_pairs = build_active_bank_oracle_pairs(
        marginfi_account.info,
        debt_bank.info,
        collateral_bank.info,
        debt_oracle_ais.as_slice(),
        collateral_oracle_ais.as_slice(),
    )?;

    let mut adapter_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        marginfi_account.info.clone(),
        market_signer.clone(),
        bank.info.clone(),
        vault.info.clone(),
        bank_liquidity_vault_authority.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    adapter_accounts.extend(active_pairs);

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
    let (actual_atoms, _actual_shares_burned) =
        MarginfiV18Adapter.withdraw(&adapter_accounts, expected_shares, &[market_signer_seeds])?;

    let payout_atoms: u64 = actual_atoms.min(expected_atoms);

    transfer_vault_to_user(
        token_program.info,
        vault.info,
        trader_token.info,
        mint.info,
        market_signer,
        market_key,
        mint_key,
        market_signer_bump,
        payout_atoms,
        mint.mint.decimals,
    )?;

    emit_stack(WithdrawLog {
        market: market_key,
        trader: *payer.info.key,
        mint: mint_key,
        amount_atoms: payout_atoms,
    })?;

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transfer_vault_to_user<'info>(
    token_program: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    trader_token: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    market_signer: &AccountInfo<'info>,
    market_key: Pubkey,
    _mint_key: Pubkey,
    market_signer_bump: u8,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    let market_bytes = market_key.to_bytes();
    let bump_arr = [market_signer_bump];
    let signer_seeds: &[&[u8]] = &[MARKET_SIGNER_SEED, &market_bytes, &bump_arr];
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            vault.key,
            mint.key,
            trader_token.key,
            market_signer.key,
            &[],
            amount,
            decimals,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                vault.clone(),
                mint.clone(),
                trader_token.clone(),
                market_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            vault.key,
            trader_token.key,
            market_signer.key,
            &[],
            amount,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                vault.clone(),
                trader_token.clone(),
                market_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    }
}

fn build_active_bank_oracle_pairs<'a, 'info>(
    marginfi_account_ai: &'a AccountInfo<'info>,
    debt_bank_ai: &'a AccountInfo<'info>,
    collateral_bank_ai: &'a AccountInfo<'info>,
    debt_oracle_ais: &[&'a AccountInfo<'info>],
    collateral_oracle_ais: &[&'a AccountInfo<'info>],
) -> Result<Vec<AccountInfo<'info>>, ProgramError> {
    let data = marginfi_account_ai.try_borrow_data()?;
    let mfi =
        MarginfiAccount::try_from_account_data(&data).map_err(|_| YdeltaError::IncorrectAccount)?;

    let mut pairs: Vec<AccountInfo<'info>> =
        Vec::with_capacity(2 + debt_oracle_ais.len() + collateral_oracle_ais.len());
    for bal in mfi.balances.iter() {
        if bal.active == 0 {
            continue;
        }
        if &bal.bank_pk == debt_bank_ai.key {
            pairs.push(debt_bank_ai.clone());
            for ai in debt_oracle_ais {
                pairs.push((*ai).clone());
            }
        } else if &bal.bank_pk == collateral_bank_ai.key {
            pairs.push(collateral_bank_ai.clone());
            for ai in collateral_oracle_ais {
                pairs.push((*ai).clone());
            }
        } else {
            return Err(YdeltaError::IncorrectAccount.into());
        }
    }
    Ok(pairs)
}
