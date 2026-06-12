//! `PlaceOrder` — borrower-side IOC bid. Signer is the borrower (payer).
//! Matches against resting asks; any unfilled remainder is funded as a
//! P2Pool (variable-rate marginfi-backed) loan when book liquidity is
//! exhausted, falling back to an `OrderFilledIoc` log otherwise. Borrower
//! collateral is encumbered on the seat, the P2Pool tail issues a marginfi
//! `borrow` against the borrower account and `deposit`s the proceeds into
//! the lender integration account, and the seat tracks the credited shares.

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

/// Borrower-side IOC bid parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct PlaceOrderParams {
    /// Optional hint pointing at the borrower's `ClaimedSeat`; falls back to lookup if `None` or stale.
    pub seat_index_hint: Option<DataIndex>,
    /// One of the `RESIDUAL_MODE_*` constants (v1 D6): P2Pool fallback
    /// (0, default), rest the residual as a bid (1), or drop it (2).
    pub residual_mode: u8,
    /// Expiry for a rested residual; `0` = never. Only meaningful with
    /// `RESIDUAL_MODE_REST`.
    pub last_valid_unix_ts: i64,
    /// Maximum borrower-paid rate in bps; asks at higher rates are skipped.
    pub rate_bps: u16,
    /// Loan term in seconds for any new fixed-rate match.
    pub term_seconds: u32,
    /// Principal to borrow in debt-mint atoms.
    pub principal_atoms: u64,
    /// Collateral to encumber in collateral-mint atoms.
    pub collateral_atoms: u64,
}

/// Borrower IOC bid. Matches resting asks, encumbers borrower collateral, and
/// either creates a `MatchedLoan` queue entry (Fixed) or opens a P2Pool tail
/// via marginfi `borrow` + `deposit`.
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

    // Adapter calls still return raw u128 fp48; wrap at the boundary so the
    // PlaceOrderArgs fields (now [`Fp48`]) get the type-safe payload.
    let snapshot_fp48: crate::math::Fp48 = {
        let data = collateral_bank.info.try_borrow_data()?;
        let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
            .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
        crate::math::Fp48::from_raw(wrapped_i80f48_to_u128(bank.asset_share_value)?)
    };

    let debt_oracle_price_fp48: crate::math::Fp48 = crate::math::Fp48::from_raw(
        MarginfiV18Adapter.oracle_price(&crate::validation::oracle_price_args(
            debt_bank.info,
            &debt_oracle_ais,
        ))?,
    );
    let collateral_oracle_price_fp48: crate::math::Fp48 = crate::math::Fp48::from_raw(
        MarginfiV18Adapter.oracle_price(&crate::validation::oracle_price_args(
            collateral_bank.info,
            &collateral_oracle_ais,
        ))?,
    );
    let (_debt_asset_init, debt_liability_weight_init_raw) =
        MarginfiV18Adapter.init_weight(&[debt_bank.info.clone()])?;
    let debt_liability_weight_init_fp48 =
        crate::math::Fp48::from_raw(debt_liability_weight_init_raw);
    let (collateral_asset_weight_init_raw, _coll_liab_init) =
        MarginfiV18Adapter.init_weight(&[collateral_bank.info.clone()])?;
    let collateral_asset_weight_init_fp48 =
        crate::math::Fp48::from_raw(collateral_asset_weight_init_raw);

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    // v1 D5: live bank lending APR (ceil bps) — the per-fill ask floor.
    let ask_floor_rate_bps: u16 =
        crate::protocol::marginfi_rate_calc::current_lending_apr_bps_ceil(
            debt_bank.info,
            marginfi_group.info,
        )?;

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
                residual_mode: params.residual_mode,
                last_valid_unix_ts: params.last_valid_unix_ts,
                now_unix_ts: now,
                share_price_snapshot_fp48: snapshot_fp48,
                debt_oracle_price_fp48,
                collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48,
                enforce_ltv: true,
                ask_floor_rate_bps,
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

    if result.rested {
        // v1 D6: track the resting bid on the borrower's UserAccount so
        // UIs can enumerate open orders without scanning every market.
        let needs_block: bool = {
            let data = user_account_ai.try_borrow_data()?;
            let fixed: &crate::state::user_account::UserAccountFixed =
                bytemuck::from_bytes(&data[..crate::state::USER_ACCOUNT_FIXED_SIZE]);
            !fixed.has_free_block()
        };
        if needs_block {
            crate::validation::expand_user_account(&payer, user_account_ai)?;
        }
        let data = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) =
            data.split_at_mut(crate::state::USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut crate::state::user_account::UserAccountFixed =
            bytemuck::from_bytes_mut(fixed_bytes);
        crate::state::user_account::insert_user_order(
            fixed,
            dynamic,
            market_key,
            result.sequence,
            crate::state::Side::Bid as u8,
            params.rate_bps,
            params.term_seconds,
            result.match_result.remaining_principal,
            now,
        )?;
    }

    if !result.rested
        && !is_not_nil!(result.p2pool_loan_index)
        && result.match_result.remaining_principal > 0
    {
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
