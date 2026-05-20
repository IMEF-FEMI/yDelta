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

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct WithdrawParams {
    /// Atoms to withdraw. Ignored entirely when `withdraw_all == true`.
    pub amount_atoms: u64,
    pub trader_index_hint: Option<DataIndex>,
    /// When true, drain the seat's entire `*_withdrawable_shares` balance
    /// for the side selected by the token mint; `amount_atoms` is ignored.
    pub withdraw_all: bool,
}

pub fn process_withdraw(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = WithdrawParams::try_from_slice(data)?;
    // The `amount_atoms > 0` guard only applies to the explicit-amount
    // path. For `withdraw_all` the amount is ignored; an empty seat
    // (0 withdrawable shares) is handled below as a clean no-op-error
    // via the `expected_shares == 0` check.
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

    // ── Determine the share quantity to burn ────────────────────────
    //
    // Two paths:
    //   * explicit amount — convert `amount_atoms` → shares (floored
    //     `amount_to_asset_shares`); the seat is debited exactly those
    //     shares.
    //   * `withdraw_all`  — read the seat's entire `*_withdrawable_shares`
    //     for the side and burn exactly that, ignoring `amount_atoms`.
    //
    // The seat-share debit and the physical atom movement are reconciled
    // below per the rounding policy.
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

    // An empty seat under `withdraw_all` is a clean error rather than a
    // 0-atom CPI: consistent with the explicit path's `amount > 0` guard
    // and the seat-side `InsufficientWithdrawableBalance` style.
    require!(
        expected_shares > 0,
        YdeltaError::InsufficientWithdrawableBalance,
        "nothing withdrawable on this side"
    )?;

    // Rounding policy: the seat is debited `expected_shares`, so the
    // user must never be paid more than those shares are *worth* at the
    // current bank price. `shares_to_amount` floors, so `expected_atoms`
    // is the protocol-favourable ceiling on the payout. The adapter may
    // hand back `actual_atoms = expected_atoms ± 1` (marginfi's
    // `assert_within_one_token` drift tolerance); paying the raw
    // `actual_atoms` when it is `expected_atoms + 1` would slowly drain
    // the shared marginfi balance funded by other holders. We pay
    // `min(actual_atoms, expected_atoms)` so the share-ledger debit and
    // the atoms paid always agree in the protocol's favour.
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
        // Burns exactly `expected_shares` — for `withdraw_all` this is the
        // seat's full `*_withdrawable_shares`, leaving the side at 0.
        da.withdraw_from_seat(seat_index, expected_shares, is_debt)?;
    }

    // Build the marginfi `remaining_accounts` list dynamically: one
    // `(bank, …oracles)` tuple per active balance, in the order marginfi
    // iterates active balances (slot order — i.e. the order they appear
    // in `MarginfiAccount.balances`). The oracle slice is variadic per
    // the bank's `OracleSetup` (1 entry for Pyth-push / Switchboard-pull,
    // more for multi-oracle setups). For now we only know about the two
    // banks the market wraps.
    let active_pairs = build_active_bank_oracle_pairs(
        marginfi_account.info,
        debt_bank.info,
        collateral_bank.info,
        debt_oracle_ais.as_slice(),
        collateral_oracle_ais.as_slice(),
    )?;

    // Withdraw atoms from the bank's liquidity
    // vault into the market's staging vault, signed by `market_signer`.
    // Destination is the staging `vault`, NOT `trader_token`, because
    // marginfi's withdraw destination doesn't have to be owned by the
    // authority but the subsequent SPL transfer (vault → user) does.
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
    let actual_atoms: u64 =
        MarginfiV18Adapter.withdraw(&adapter_accounts, expected_shares, &[market_signer_seeds])?;

    // Pay at most what the debited shares are worth. The staging
    // `vault` keeps any +1-atom drift (it stays inside the protocol and
    // is reconciled into the next withdraw / accounting pass) rather than
    // being handed to the user.
    let payout_atoms: u64 = actual_atoms.min(expected_atoms);

    // Transfer the payout from the staging vault to the user's wallet,
    // signed by `market_signer` (the vault's owner).
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

    // Sync the signer's MarketPosition mirror.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

/// Signed SPL transfer: vault → user, signed by `market_signer`.
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

/// Walk the marginfi-account's balances in slot order and collect the
/// `(bank, …oracles)` AccountInfo tuple for each currently-active balance.
/// `bank_pk` matches against the two banks ydelta knows about (debt and
/// collateral lending pools). The oracle slice per bank is variadic per
/// the bank's `OracleSetup`. Returns an empty vec if no balances are
/// active (the marginfi-account was just initialized).
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
            // Active balance on a bank ydelta doesn't know about. The
            // matching/borrow flow only introduces balances on the
            // two banks ydelta already tracks; anything else is
            // unexpected state. Surface as an error.
            return Err(YdeltaError::IncorrectAccount.into());
        }
    }
    Ok(pairs)
}
