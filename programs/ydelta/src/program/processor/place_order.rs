use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{is_not_nil, DataIndex, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, instruction::AccountMeta,
    program::invoke_signed, pubkey::Pubkey,
};

use crate::logs::{emit_stack, OrderFilledIocLog};
use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
use crate::protocol::LendingProtocol;
use crate::state::{
    get_now_unix_ts,
    market::get_mut_helper_matched_loan,
    market_helpers::{
        get_seat_index_with_hint, match_borrower_bid, PlaceOrderArgs, PlaceOrderResult,
    },
    MarketFixed, OrderType, Side,
};
use crate::validation::loaders::PlaceOrderContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct PlaceOrderParams {
    pub seat_index_hint: Option<DataIndex>,
    pub flags: u8,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
}

pub fn process_place_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = PlaceOrderParams::try_from_slice(data)?;

    let side = Side::Bid;
    let order_type = OrderType::ImmediateOrCancel;

    let ctx = PlaceOrderContext::load(accounts)?;
    let PlaceOrderContext {
        payer,
        market,
        _system_program: _,
        marginfi_group,
        borrower_marginfi_account,
        lender_marginfi_account,
        market_debt_vault,
        debt_bank,
        collateral_bank,
        debt_oracle_ais,
        collateral_oracle_ais,
        market_signer,
        market_signer_bump,
        marginfi_program,
        debt_liquidity_vault,
        debt_bank_liquidity_vault_authority,
        token_program,
        user_account_ai,
        vault: vault_account,
    } = ctx;

    expand_market_if_needed(payer.info, &market)?;

    let snapshot_fp48: u128 = {
        let data = collateral_bank.info.try_borrow_data()?;
        let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
            .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
        wrapped_i80f48_to_u128(bank.asset_share_value)?
    };

    let debt_oracle_price_fp48: u128 = MarginfiV18Adapter.oracle_price(
        &crate::validation::oracle_price_args(debt_bank.info, &debt_oracle_ais),
    )?;
    let collateral_oracle_price_fp48: u128 = MarginfiV18Adapter.oracle_price(
        &crate::validation::oracle_price_args(collateral_bank.info, &collateral_oracle_ais),
    )?;
    let (_debt_asset_init, debt_liability_weight_init_fp48) =
        MarginfiV18Adapter.init_weight(&[debt_bank.info.clone()])?;
    let (collateral_asset_weight_init_fp48, _coll_liab_init) =
        MarginfiV18Adapter.init_weight(&[collateral_bank.info.clone()])?;

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    let pre_borrow_liability_shares: u128 =
        read_debt_bank_liability_shares(borrower_marginfi_account.info, debt_bank.info.key)?;

    {
        let ask_count = super::shared::count_resting_asks(&market)?;
        super::shared::expand_market_to_free_blocks(payer.info, &market, ask_count + 1)?;
    }

    let result: PlaceOrderResult = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index =
            get_seat_index_with_hint(da.fixed, da.dynamic, payer.info.key, params.seat_index_hint)?;
        match_borrower_bid(
            da.fixed,
            da.dynamic,
            PlaceOrderArgs {
                market_pubkey: market_key,
                taker_seat_index: seat_index,
                side,
                order_type,
                rate_bps: params.rate_bps,
                term_seconds: params.term_seconds,
                principal_atoms: params.principal_atoms,
                collateral_atoms: params.collateral_atoms,
                flags: params.flags,
                now_unix_ts: now,
                share_price_snapshot_fp48: snapshot_fp48,
                debt_oracle_price_fp48,
                collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48,
                enforce_ltv: true,
            },
            vault_account,
        )?
    };

    if is_not_nil!(result.p2pool_loan_index) && result.match_result.remaining_principal > 0 {
        let market_signer_seeds: &[&[u8]] = &[
            crate::validation::MARKET_SIGNER_SEED,
            market_key.as_ref(),
            &[market_signer_bump],
        ];

        let mut remaining: Vec<AccountMeta> = Vec::with_capacity(
            2 + collateral_oracle_ais.count as usize + debt_oracle_ais.count as usize,
        );
        remaining.push(AccountMeta::new_readonly(*collateral_bank.info.key, false));
        for ai in &collateral_oracle_ais.ais {
            remaining.push(AccountMeta::new_readonly(*ai.key, false));
        }
        remaining.push(AccountMeta::new_readonly(*debt_bank.info.key, false));
        for ai in &debt_oracle_ais.ais {
            remaining.push(AccountMeta::new_readonly(*ai.key, false));
        }

        let borrow_ix = marginfi_mocks::cpi::borrow_ix(
            &marginfi_mocks::cpi::BorrowAccounts {
                group: *marginfi_group.info.key,
                marginfi_account: *borrower_marginfi_account.info.key,
                authority: *market_signer.key,
                bank: *debt_bank.info.key,
                destination_token_account: *market_debt_vault.info.key,
                bank_liquidity_vault_authority: *debt_bank_liquidity_vault_authority.key,
                liquidity_vault: *debt_liquidity_vault.info.key,
                token_program: *token_program.info.key,
            },
            result.match_result.remaining_principal,
            &remaining,
        );
        let mut invoke_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            borrower_marginfi_account.info.clone(),
            market_signer.clone(),
            debt_bank.info.clone(),
            market_debt_vault.info.clone(),
            debt_bank_liquidity_vault_authority.clone(),
            debt_liquidity_vault.info.clone(),
            token_program.info.clone(),
            collateral_bank.info.clone(),
        ];
        for ai in &collateral_oracle_ais.ais {
            invoke_accounts.push((*ai).clone());
        }
        for ai in &debt_oracle_ais.ais {
            invoke_accounts.push((*ai).clone());
        }
        invoke_accounts.push(marginfi_program.info.clone());
        invoke_signed(&borrow_ix, &invoke_accounts, &[market_signer_seeds])?;

        let post_borrow_liability_shares: u128 =
            read_debt_bank_liability_shares(borrower_marginfi_account.info, debt_bank.info.key)?;
        let liability_shares_opened: u128 = post_borrow_liability_shares
            .checked_sub(pre_borrow_liability_shares)
            .ok_or(crate::program::YdeltaError::IncorrectAccount)?;

        let pre_deposit_lender_asset_shares: u128 =
            read_debt_bank_asset_shares(lender_marginfi_account.info, debt_bank.info.key)?;

        let deposit_ix = marginfi_mocks::cpi::deposit_ix(
            &marginfi_mocks::cpi::DepositAccounts {
                group: *marginfi_group.info.key,
                marginfi_account: *lender_marginfi_account.info.key,
                authority: *market_signer.key,
                bank: *debt_bank.info.key,
                signer_token_account: *market_debt_vault.info.key,
                liquidity_vault: *debt_liquidity_vault.info.key,
                token_program: *token_program.info.key,
            },
            result.match_result.remaining_principal,
            None,
            &[],
        );
        invoke_signed(
            &deposit_ix,
            &[
                marginfi_group.info.clone(),
                lender_marginfi_account.info.clone(),
                market_signer.clone(),
                debt_bank.info.clone(),
                market_debt_vault.info.clone(),
                debt_liquidity_vault.info.clone(),
                token_program.info.clone(),
                marginfi_program.info.clone(),
            ],
            &[market_signer_seeds],
        )?;

        let post_deposit_lender_asset_shares: u128 =
            read_debt_bank_asset_shares(lender_marginfi_account.info, debt_bank.info.key)?;
        let asset_shares_credited: u128 = post_deposit_lender_asset_shares
            .checked_sub(pre_deposit_lender_asset_shares)
            .ok_or(crate::program::YdeltaError::IncorrectAccount)?;

        if asset_shares_credited > 0 {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let borrower_seat_index = get_seat_index_with_hint(
                da.fixed,
                da.dynamic,
                payer.info.key,
                params.seat_index_hint,
            )?;
            da.deposit_to_seat(borrower_seat_index, asset_shares_credited, true)?;
        }

        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;

        let (_fixed, dynamic) = market_data.split_at_mut(core::mem::size_of::<MarketFixed>());
        let rb_node = get_mut_helper_matched_loan(dynamic, result.p2pool_loan_index);
        rb_node.get_mut_value().borrower_marginfi_borrow_shares = liability_shares_opened;
    }

    if !is_not_nil!(result.p2pool_loan_index) && result.match_result.remaining_principal > 0 {
        emit_stack(OrderFilledIocLog {
            market: market_key,
            trader: *payer.info.key,
            sequence: result.sequence,
            principal_dropped_atoms: result.match_result.remaining_principal,
            side: side as u8,
            _padding: [0; 7],
        })?;
    }

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

fn read_debt_bank_liability_shares(
    marginfi_account: &AccountInfo,
    debt_bank: &Pubkey,
) -> Result<u128, solana_program::program_error::ProgramError> {
    let data = marginfi_account.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    match mfi.find_balance(debt_bank) {
        Some(b) => wrapped_i80f48_to_u128(b.liability_shares),
        None => Ok(0),
    }
}

fn read_debt_bank_asset_shares(
    marginfi_account: &AccountInfo,
    debt_bank: &Pubkey,
) -> Result<u128, solana_program::program_error::ProgramError> {
    let data = marginfi_account.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    match mfi.find_balance(debt_bank) {
        Some(b) => wrapped_i80f48_to_u128(b.asset_shares),
        None => Ok(0),
    }
}
