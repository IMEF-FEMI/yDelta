//! Place a borrower IOC bid.
//!
//! The orderbook is QUOTE-ONLY: the only resting orders are vault
//! risk-profile asks (placed via `place_order_for_risk_profile`).
//! `place_order` is the borrower path and is **immediate-or-cancel
//! only**: it crosses resting asks and any residual either fires the
//! P2Pool marginfi fallback or drops. No wallet-owned order ever rests.

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

/// Process a borrower IOC bid only.
///
/// The taker side is always `Side::Bid` and the order type is always
/// `OrderType::ImmediateOrCancel`: the bid crosses resting risk-profile
/// asks and any residual either fires the P2Pool marginfi fallback or
/// drops. The bid never rests on the book.
pub fn process_place_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = PlaceOrderParams::try_from_slice(data)?;
    // place_order is borrower-IOC-only: the taker is always a Bid and
    // the order type is always ImmediateOrCancel.
    let side = Side::Bid;
    let order_type = OrderType::ImmediateOrCancel;

    // place_order's account list carries marginfi accounts for
    // LTV-at-match and P2Pool fallback. Off-chain indexers reading
    // OrderPlacedLog should not assume a fixed account count.
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

    // Snapshot the collateral bank's asset_share_value at place time —
    // a borrower bid encumbers collateral.
    let snapshot_fp48: u128 = {
        let data = collateral_bank.info.try_borrow_data()?;
        let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
            .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
        wrapped_i80f48_to_u128(bank.asset_share_value)
    };

    // Snapshot oracle prices + bank weights for the LTV-at-match
    // check. `oracle_price` reads bank+oracle pairs and dispatches by
    // oracle setup; `init_weight` decodes weights from the bank's
    // BankConfig region.
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

    // Borrowers do not declare an LTV. LTV enforcement is entirely
    // match-time: `match_order` checks the matched collateral against
    // both the marginfi-init weights and the crossed vault profile's
    // curator-set `max_ltv_bps` cap (read live from the `RiskProfile`).
    // No place-time LTV pre-flight is needed here.

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    // Snapshot the borrower marginfi-account's current debt-bank
    // liability shares BEFORE matching so we can compute the
    // post-CPI delta and stamp it onto the P2Pool MatchedLoan node.
    // Zero when no liability balance exists yet.
    let pre_borrow_liability_shares: u128 =
        read_debt_bank_liability_shares(borrower_marginfi_account.info, debt_bank.info.key)?;

    // ─── Reserve MatchedLoan blocks for the matching pass ───
    //
    // Vault asks are unbounded standing quotes that matching never
    // removes — every cross allocates one `MatchedLoan` block from the
    // market free list and frees nothing. A borrower bid crosses each
    // resting ask at most once, so it can allocate at most `ask_count`
    // MatchedLoan blocks; reserve one extra for a possible P2Pool node.
    // Without this, a bid crossing 2+ asks fails with
    // `AccountDataTooSmall` (`expand_market_if_needed` only adds one).
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

    // Risk-profile maker bookkeeping is applied inline by the matching
    // engine (the gate writes `RiskProfile.encumbered_in_orders_atoms
    // +=` and `seat.deployed_atoms +=` at accept time, under the same
    // borrow as the gate read).

    // When match_borrower_bid converted the residual into a P2Pool
    // MatchedLoan, fire `marginfi.borrow` against the borrower's
    // marginfi-account for the residual atoms. Destination = market's
    // debt-mint vault PDA (then deposit-back path routes atoms onto
    // the lender side). Authority = market_signer (signed via
    // invoke_signed). The borrower's collateral asset (already
    // deposited on the borrower account) backs marginfi's solvency
    // check; debt+collateral oracles flow through `remaining_accounts`
    // per marginfi v0.1.8's health-check protocol.
    if is_not_nil!(result.p2pool_loan_index) && result.match_result.remaining_principal > 0 {
        let market_signer_seeds: &[&[u8]] = &[
            crate::validation::MARKET_SIGNER_SEED,
            market_key.as_ref(),
            &[market_signer_bump],
        ];

        // Marginfi v0.1.8's health check iterates the marginfi-account's
        // active `balances[]` slots in order and matches each one
        // against the next `(bank, …oracles)` tuple in
        // `remaining_accounts`. The borrower-side account's slot[0] is
        // the collateral asset (deposited first in the lifecycle) and
        // slot[1] is the debt liability (just opened by this CPI). The
        // oracle slice per bank is variadic per the bank's `OracleSetup`
        // (1 entry for Pyth-push / Switchboard-pull, more for
        // multi-oracle setups). Order remaining_accounts accordingly. If
        // the borrower has no collateral balance yet, the borrow itself
        // would fail solvency long before this list matters, so the
        // only-debt path is unreachable.
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
        // Borrowed atoms route through `market_debt_vault` (a
        // market_signer-owned token account) instead of going
        // straight to the borrower's wallet. This sets up the
        // deposit-back into `lender_marginfi_account` below so the
        // borrower's principal earns yield. The borrower can
        // withdraw to their wallet later via `process_withdraw`.
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
            // remaining_accounts (health-check tuples in balance-slot
            // order: collateral asset, then new debt liability). The
            // bank itself is already in `invoke_accounts` so we only
            // need to forward each side's variadic oracle slice plus
            // (for the debt side) the debt_bank — but the debt_bank
            // pubkey is also already present as the `bank` arg above.
            // The tail must mirror the `remaining` AccountMetas exactly.
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

        // Read post-CPI liability-share delta and stamp it onto the
        // MatchedLoan node so the cranker (process_matched_loan) can
        // hand it off to LoanFixed without re-reading marginfi.
        let post_borrow_liability_shares: u128 =
            read_debt_bank_liability_shares(borrower_marginfi_account.info, debt_bank.info.key)?;
        let liability_shares_opened: u128 = post_borrow_liability_shares
            .checked_sub(pre_borrow_liability_shares)
            .ok_or(crate::program::YdeltaError::IncorrectAccount)?;

        // ─── Deposit-back into lender_marginfi_account.
        // Atoms are now in `market_debt_vault` (market_signer-owned).
        // Deposit them into `lender_marginfi_account` (where Fixed
        // loan principal sits and earns lender-side yield). Both the
        // SPL transfer authority (signer_token_account.owner) and
        // the marginfi authority (lender_marginfi_account.authority)
        // are `market_signer`, so a single `invoke_signed` deposit
        // satisfies both constraints.
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
            /*deposit_up_to_limit=*/ None,
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

        // Credit the borrower's seat with the deposited asset shares.
        // The borrower now has a `debt_withdrawable_shares` position
        // backed by `lender_marginfi_account`'s asset on `debt_bank`,
        // mirroring Fixed-loan principal credits. They withdraw to
        // atoms via `process_withdraw` in a later tx.
        if asset_shares_credited > 0 {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let borrower_seat_index = get_seat_index_with_hint(
                da.fixed,
                da.dynamic,
                payer.info.key,
                params.seat_index_hint,
            )?;
            da.deposit_to_seat(
                borrower_seat_index,
                asset_shares_credited,
                /*is_debt=*/ true,
            )?;
        }

        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        // The MatchedLoan tree node-index returned by `match_borrower_bid`
        // is relative to the **dynamic** region (everything after the
        // `MarketFixed` header). Splitting here matches every other
        // hypertree mutation in the codebase (`get_mut_helper_seat`
        // callers, `get_mut_helper_order` callers — all pass `dynamic`,
        // not the whole buffer). Writing into the whole buffer with a
        // dynamic-relative index would corrupt the header.
        let (_fixed, dynamic) = market_data.split_at_mut(core::mem::size_of::<MarketFixed>());
        let rb_node = get_mut_helper_matched_loan(dynamic, result.p2pool_loan_index);
        rb_node.get_mut_value().borrower_marginfi_borrow_shares = liability_shares_opened;
    }

    // IOC: any residual that did not go to the P2Pool fallback was
    // dropped — emit the fill log so off-chain indexers see the
    // dropped atoms. The bid never rests.
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

    // Sync the signer's MarketPosition mirror after place_order debits
    // encumbered balances on their seat.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}

/// Read the borrower marginfi-account's `liability_shares` for the given
/// debt bank, returning 0 when the account has no balance entry for that
/// bank. fp48-encoded `u128` (bit-pattern reinterpret of the
/// `WrappedI80F48` field). Used to compute the share delta opened by a
/// `marginfi.borrow` CPI in the P2Pool fallback path.
fn read_debt_bank_liability_shares(
    marginfi_account: &AccountInfo,
    debt_bank: &Pubkey,
) -> Result<u128, solana_program::program_error::ProgramError> {
    let data = marginfi_account.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    Ok(mfi
        .find_balance(debt_bank)
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares))
        .unwrap_or(0))
}

/// Sibling of `read_debt_bank_liability_shares`: read
/// `asset_shares` for the given bank. Used by the P2Pool deposit-back
/// path to compute the share delta credited to the borrower's seat
/// after `marginfi.deposit` lands the residual atoms in
/// `lender_marginfi_account`.
fn read_debt_bank_asset_shares(
    marginfi_account: &AccountInfo,
    debt_bank: &Pubkey,
) -> Result<u128, solana_program::program_error::ProgramError> {
    let data = marginfi_account.try_borrow_data()?;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    Ok(mfi
        .find_balance(debt_bank)
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares))
        .unwrap_or(0))
}
