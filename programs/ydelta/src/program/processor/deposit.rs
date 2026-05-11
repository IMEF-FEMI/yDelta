use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, DepositLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::{market_helpers::get_seat_index_with_hint, MarketFixed};
use crate::validation::loaders::DepositContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::{get_mut_dynamic_account, invoke};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct DepositParams {
    pub amount_atoms: u64,
    pub trader_index_hint: Option<DataIndex>,
}

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

    let market_key = *market.info.key;
    let mint_key = if is_debt {
        market.get_fixed()?.debt_mint
    } else {
        market.get_fixed()?.collateral_mint
    };

    // Transfer atoms from the user's ATA to the market's
    // staging vault. The user signs as `trader_token`'s owner. (Marginfi's
    // deposit CPI requires `signer_token_account.owner == authority`,
    // which is `market_signer` — so atoms must arrive in a market-signer-
    // owned account first.)
    transfer_user_to_vault(
        token_program.info,
        trader_token.info,
        vault.info,
        mint.info,
        payer.info,
        params.amount_atoms,
        mint.mint.decimals,
    )?;

    // Deposit from the staging vault into the
    // bank's liquidity vault, signed by `market_signer`.
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
    let credited_shares: u128 = MarginfiV18Adapter.deposit(
        &adapter_accounts,
        params.amount_atoms,
        &[market_signer_seeds],
    )?;

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
        amount_atoms: params.amount_atoms,
    })?;

    // Sync the signer's MarketPosition mirror from the canonical
    // ClaimedSeat we just wrote.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

/// SPL-transfer `amount` atoms from the user's `trader_token` to the
/// market's staging `vault`, signed by the user (the trader_token's
/// owner). Pub(crate) so `global_vault_deposit` can reuse it.
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
