//! Place wallet-side primary orders and secondary loan-sale bids.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{is_not_nil, DataIndex, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, instruction::AccountMeta,
    program::invoke_signed, program_error::ProgramError, pubkey::Pubkey,
};

use crate::logs::{emit_stack, OrderFilledIocLog, OrderRestedLog, SecondaryBidPlacedLog};
use crate::program::YdeltaError;
use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
use crate::protocol::LendingProtocol;
use crate::require;
use crate::state::{
    get_now_unix_ts,
    loan::{accrue_loan, LoanFixed, LOAN_FIXED_SIZE},
    market::get_mut_helper_matched_loan,
    market_helpers::{
        get_seat_index_with_hint, place_order_inner, PlaceOrderArgs, PlaceOrderResult,
    },
    MarketFixed, OrderKind, OrderType, Side,
};
use crate::validation::loaders::PlaceOrderContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct PlaceOrderParams {
    pub seat_index_hint: Option<DataIndex>,
    pub side: u8,
    pub order_type: u8,
    pub flags: u8,
    /// `OrderKind { Primary = 0, SecondaryLoanSale = 1 }`. For
    /// `SecondaryLoanSale`, the loader expects a Loan PDA appended to
    /// the account list, and the engine snapshots the loan's
    /// rate/term/principal — overriding the corresponding params
    /// fields.
    pub kind: u8,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    /// Secondary-only field — the cash the seller wants now in
    /// exchange for transferring the loan's lender claim. Zero for
    /// primary orders.
    pub asking_price_atoms: u64,
    pub last_valid_unix_ts: i64,
    /// Borrower-side LTV cap (Bids only). `None` defaults to marginfi's
    /// init LTV — every loan that satisfies marginfi-init also satisfies
    /// the borrower's declared cap, so existing callers see no behavior
    /// change. Setting an explicit value lets the borrower opt into a
    /// tighter risk band: the matching loop walks past vault makers
    /// whose `RiskProfile.max_ltv_bps < borrower_ltv_bps` (risk-tier
    /// mismatch) and the pre-flight check rejects bids whose actual
    /// loan LTV exceeds the declared cap.
    pub borrower_ltv_bps: Option<u16>,
}

pub fn process_place_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = PlaceOrderParams::try_from_slice(data)?;
    let side = Side::try_from(params.side)
        .map_err(|_| solana_program::program_error::ProgramError::InvalidInstructionData)?;
    let order_type = OrderType::try_from(params.order_type)
        .map_err(|_| solana_program::program_error::ProgramError::InvalidInstructionData)?;
    let kind = OrderKind::try_from(params.kind)
        .map_err(|_| solana_program::program_error::ProgramError::InvalidInstructionData)?;

    // Secondary bids must be Limit (no IOC/PostOnly), must be a Bid
    // (asks can't be secondary), and must come from the current
    // lender (checked below once we have the seat index).
    if kind == OrderKind::SecondaryLoanSale {
        require!(
            order_type == OrderType::Limit,
            YdeltaError::SecondaryOrderTypeInvalid,
            "SecondaryLoanSale requires order_type = Limit (got {:?})",
            order_type
        )?;
        require!(
            side == Side::Bid,
            YdeltaError::SecondaryOrderTypeInvalid,
            "SecondaryLoanSale orders are always Bids (got {:?})",
            side
        )?;
    }

    // place_order's account list carries marginfi accounts for
    // LTV-at-match and P2Pool fallback. Off-chain indexers reading
    // OrderPlacedLog should not assume a fixed account count.
    let ctx = PlaceOrderContext::load(accounts, kind)?;
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
        borrower_debt_token,
        token_program,
        user_account_ai,
        secondary_loan,
        vault: vault_account,
    } = ctx;

    expand_market_if_needed(payer.info, &market)?;

    // `SecondaryLoanSale` placement is a separate codepath: no
    // encumbrance, snapshot loan fields, validate ownership +
    // duplicate-check, walk asks for crosses, then rest the residual.
    if kind == OrderKind::SecondaryLoanSale {
        let loan = secondary_loan.ok_or(crate::program::YdeltaError::IncorrectAccount)?;

        let market_key_local = *market.info.key;
        let now = get_now_unix_ts()?;

        // O(1) duplicate check via the LoanFixed flag. Caller-side
        // gate so `place_secondary_bid` doesn't have to walk the bids
        // tree. The flag is set below after a successful insert and
        // cleared by cancel_order / cranker finalize / staleness sweep.
        {
            let loan_data = loan.info.try_borrow_data()?;
            let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
            require!(
                header.has_resting_secondary_bid == 0,
                YdeltaError::SecondaryDuplicate,
                "loan already has a resting SecondaryLoanSale bid"
            )?;
        }

        // Liveness + LTV gates at secondary bid placement.
        //
        // The placing seller passes the loan PDA — we have everything
        // needed to confirm the loan is (a) still active and pre-maturity
        // and (b) solvent at current oracle prices, before resting a bid
        // that any future primary ask can cross. The LTV check fires
        // once here; all subsequent crosses (full or split) inherit the
        // result because the LTV math is linear in debt — a pro-rata
        // α-fraction of (collateral, debt) satisfies LTV iff the parent
        // does. That's why `process_matched_loan` doesn't repeat the
        // check at finalize.
        //
        {
            // Liveness: state == Active and matures_at_unix > now.
            // (`accrue_loan` is a no-op against a Repaid loan since
            // outstanding == 0, but we reject up front to avoid touching
            // a settled loan at all.)
            let (state_byte, matures_at_unix) = {
                let loan_data = loan.info.try_borrow_data()?;
                let header: &LoanFixed = bytemuck::from_bytes(&loan_data[..LOAN_FIXED_SIZE]);
                (header.state, header.matures_at_unix)
            };
            require!(
                state_byte == crate::state::loan::LoanState::Active as u8,
                YdeltaError::SecondaryLoanSettled,
                "secondary bid: loan is not Active (state={})",
                state_byte
            )?;
            require!(
                matures_at_unix > now,
                YdeltaError::SecondaryLoanMatured,
                "secondary bid: loan matured at {} (now {})",
                matures_at_unix,
                now
            )?;

            let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;
            let ltv_buffer_bps: u16 = market.get_fixed()?.fee_config.ltv_buffer_bps;

            // Accrue first so the LTV check uses fresh outstanding.
            let (outstanding_now, collateral_now) = {
                let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
                let header: &mut LoanFixed =
                    bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
                accrue_loan(header, now, grace_period_seconds)?;
                (header.outstanding_debt_atoms, header.collateral_atoms)
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

            let required = crate::state::ltv::get_required_quote_collateral_to_back_debt(
                outstanding_now,
                debt_oracle_price_fp48,
                collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48,
                ltv_buffer_bps,
            )?;
            require!(
                collateral_now >= required,
                YdeltaError::CollateralBelowMatchLTV,
                "secondary bid: loan collateral {} < required {} at current oracle prices",
                collateral_now,
                required
            )?;
        }

        // Read loan snapshot fields. Borrowed read of loan first, drop
        // before mutating the market.
        //
        // **Rate snapshot = `borrower_rate_bps`, not `lender_rate_bps`.**
        // The cross gate asks "can the protocol profitably route the
        // borrower's interest stream to the new lender at their
        // `ask.rate`?" The borrower pays `borrower_rate` (immutable
        // contract). The new lender accepts `ask.rate`. Cross is
        // economically possible iff `borrower_rate >= ask.rate +
        // floor`. The OLD lender's rate is irrelevant — Option A
        // discards it on cross by setting `loan.lender_rate_bps =
        // ask.rate_bps`. Gating on `lender_rate_bps` instead would
        // block crosses where `ask.rate` sits between old
        // `lender_rate` and `borrower_rate`, even though the protocol
        // could profit handsomely from that spread.
        let (
            loan_lender_seat_index,
            snapshot_rate_bps,
            snapshot_principal_atoms,
            snapshot_term_seconds,
            loan_pda_key,
            loan_sequence_snapshot,
        ) = {
            let l = loan.get_fixed()?;
            // Reject past-maturity loans.
            require!(
                l.matures_at_unix > now,
                YdeltaError::SecondaryLoanMatured,
                "loan matured at {} (now {}); secondary sale of past-maturity loans rejected",
                l.matures_at_unix,
                now
            )?;
            let term_remaining: i64 = l.matures_at_unix - now;
            (
                l.lender_seat_index,
                l.borrower_rate_bps,
                l.principal_debt_atoms,
                term_remaining as u32,
                *loan.info.key,
                l.matched_loan_sequence as u32,
            )
        };

        // Match-then-rest. Walk the asks tree first; cross any
        // compatible primary ask immediately. Only the residual (if
        // any) rests as a SecondaryLoanSale bid.
        //
        // Par-exit pricing: cash paid to seller equals
        // matched_principal exactly. The user-supplied
        // `params.asking_price_atoms` is ignored.
        let (residual_principal, place_result_opt, seller_seat_index) = {
            let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
            let da = get_mut_dynamic_account::<MarketFixed>(market_data);
            let seat_index = get_seat_index_with_hint(
                da.fixed,
                da.dynamic,
                payer.info.key,
                params.seat_index_hint,
            )?;

            // Ownership gate. place_secondary_bid checks this on the
            // residual-rest path, but a full match (residual == 0)
            // skips that branch — without this require!, anyone could
            // place a SecondaryLoanSale referencing a victim's loan,
            // match it against existing asks, and credit the matched
            // principal as withdrawable shares on the attacker's seat
            // while transferring loan ownership to the buyers.
            require!(
                seat_index == loan_lender_seat_index,
                YdeltaError::SecondaryNotCurrentLender,
                "secondary bid: signer's seat {} is not the loan's current \
                 lender seat {}",
                seat_index,
                loan_lender_seat_index
            )?;

            // Match against the asks tree first (residual rests as a
            // SecondaryLoanSale bid below). The vault account is
            // threaded so the matching loop can run the idle /
            // exposure gate against any risk-profile maker it
            // crosses.
            let fee_floor_bps = da.fixed.fee_config.protocol_fee_bps_floor;
            let match_res = crate::state::market_helpers::match_secondary_bid_against_asks(
                da.fixed,
                da.dynamic,
                crate::state::market_helpers::MatchSecondaryBidArgs {
                    market_pubkey: market_key_local,
                    seller_seat_index: seat_index,
                    loan_pda: loan_pda_key,
                    loan_sequence_snapshot,
                    borrower_rate_bps: snapshot_rate_bps,
                    term_remaining_seconds: snapshot_term_seconds,
                    principal_atoms: snapshot_principal_atoms,
                    now_unix_ts: now,
                    fee_floor_bps,
                },
                vault_account,
            )?;

            // Residual rest (skip if match consumed everything).
            let place_opt = if match_res.residual_principal_atoms > 0 {
                let r = crate::state::market_helpers::place_secondary_bid(
                    da.fixed,
                    da.dynamic,
                    crate::state::market_helpers::PlaceSecondaryBidArgs {
                        market_pubkey: market_key_local,
                        seller_seat_index: seat_index,
                        loan_pda: loan_pda_key,
                        loan_lender_seat_index,
                        loan_sequence_snapshot,
                        snapshot_rate_bps,
                        snapshot_term_seconds,
                        snapshot_principal_atoms: match_res.residual_principal_atoms,
                        asking_price_atoms: match_res.residual_principal_atoms,
                        // Hardcode NO_EXPIRATION on secondary bids
                        // regardless of what the caller passed. The
                        // matching engine's expiry sweep removes
                        // expired makers from the bids tree but
                        // doesn't clear the referenced loan's
                        // `has_resting_secondary_bid` flag — which
                        // would leave the loan permanently
                        // duplicate-blocked. Rather than thread the
                        // loan PDA into the sweep, eliminate the
                        // surface entirely: sellers must explicitly
                        // cancel via `cancel_order` (which DOES clear
                        // the flag).
                        last_valid_unix_ts:
                            crate::state::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
                        flags: params.flags,
                        now_unix_ts: now,
                    },
                )?;
                Some(r)
            } else {
                None
            };
            (match_res.residual_principal_atoms, place_opt, seat_index)
        };

        // Set the O(1) flag iff a residual bid was rested. If Scenario
        // B fully consumed the loan, no bid is in the tree → flag
        // stays 0.
        if place_result_opt.is_some() {
            let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
            let header: &mut LoanFixed =
                bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
            header.has_resting_secondary_bid = 1;
        }

        // Emit the rested-bid log only when a residual rested.
        // Crosses already emitted MatchedLoanCreatedLog per fill.
        if let Some(result) = place_result_opt {
            emit_stack(SecondaryBidPlacedLog {
                market: market_key_local,
                loan_pda: loan_pda_key,
                seller: *payer.info.key,
                seller_seat_index,
                _pad0: [0; 4],
                sequence: result.sequence,
                asking_price_atoms: residual_principal,
                snapshot_principal_atoms: residual_principal,
                snapshot_term_seconds,
                snapshot_rate_bps,
                _padding: [0; 2],
            })?;
        }

        // Marginfi/oracle/CPI accounts are unused on the secondary
        // path. Bind them to `_` so the unused-warning lint stays
        // quiet, and bail out without touching the matching engine.
        let _ = (
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
            borrower_debt_token,
            token_program,
            vault_account,
        );

        // Sync the seller's MarketPosition mirror. Seat balances are
        // unchanged on secondary placement, but the upsert ensures
        // the mirror exists with the correct `seat_index_in_market`
        // back-reference for later lookups.
        super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
        return Ok(());
    }

    // Snapshot the side-relevant bank's asset_share_value at place
    // time. Bid orders encumber collateral → snapshot the collateral
    // bank. Ask orders encumber debt → snapshot the debt bank.
    let snapshot_fp48: u128 = match side {
        Side::Bid => {
            let data = collateral_bank.info.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
            wrapped_i80f48_to_u128(bank.asset_share_value)
        }
        Side::Ask => {
            let data = debt_bank.info.try_borrow_data()?;
            let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
                .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
            wrapped_i80f48_to_u128(bank.asset_share_value)
        }
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

    // ─── Borrower-LTV resolution + pre-flight (Bids only) ───
    //
    // Borrower-set LTV completes the symmetry of yDelta's market: lenders
    // declare their max_ltv on `RiskProfile`, borrowers declare theirs in
    // `PlaceOrderParams.borrower_ltv_bps`. A loan crosses iff
    //     actual_ltv ≤ borrower_ltv ≤ profile.max_ltv ≤ marginfi_init.
    // The strict transitivity (not min-of) creates explicit risk tiers on
    // the orderbook.
    //
    // marginfi_init_ltv_bps = (collateral_asset_weight × 10_000) /
    //                         debt_liability_weight  (both fp48).
    // The fp48 factors cancel in the ratio.
    let marginfi_init_ltv_bps: u16 = {
        let num = collateral_asset_weight_init_fp48
            .checked_mul(10_000)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let raw = num
            .checked_div(debt_liability_weight_init_fp48)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // Saturate to u16::MAX. Sane mainnet weights yield bps in the
        // 5_000-9_500 range; saturation only matters for pathological
        // marginfi configs (e.g. liability_weight ≈ 0) where the bid is
        // already unsafe and the match-time check would catch it anyway.
        u16::try_from(raw.min(u16::MAX as u128)).unwrap_or(u16::MAX)
    };
    let effective_borrower_ltv_bps: u16 = params.borrower_ltv_bps.unwrap_or(marginfi_init_ltv_bps);
    require!(
        effective_borrower_ltv_bps <= marginfi_init_ltv_bps,
        YdeltaError::BorrowerLtvOverInit,
        "borrower_ltv_bps {} > marginfi init LTV {}",
        effective_borrower_ltv_bps,
        marginfi_init_ltv_bps
    )?;
    if side == Side::Bid && params.principal_atoms > 0 && params.collateral_atoms > 0 {
        // actual_ltv_bps = (principal × debt_price × 10_000)
        //                  / (collateral × coll_price). u128 throughout
        // so the (atoms × fp48 × 10_000) numerator can't overflow for
        // realistic balances.
        let num = (params.principal_atoms as u128)
            .checked_mul(debt_oracle_price_fp48)
            .and_then(|v| v.checked_mul(10_000))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let denom = (params.collateral_atoms as u128)
            .checked_mul(collateral_oracle_price_fp48)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        require!(
            denom > 0,
            YdeltaError::BorrowerLtvExceeded,
            "denominator zero (collateral or oracle price)"
        )?;
        let actual_ltv_raw = num
            .checked_div(denom)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let actual_ltv_bps =
            u16::try_from(actual_ltv_raw.min(u16::MAX as u128)).unwrap_or(u16::MAX);
        require!(
            actual_ltv_bps <= effective_borrower_ltv_bps,
            YdeltaError::BorrowerLtvExceeded,
            "actual loan LTV {} bps > borrower's declared cap {} bps",
            actual_ltv_bps,
            effective_borrower_ltv_bps
        )?;
    }
    // For Asks, the field is meaningless — pass 0 so the per-maker gate
    // is a no-op.
    let resolved_borrower_ltv_bps: u16 = if side == Side::Bid {
        effective_borrower_ltv_bps
    } else {
        0
    };

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    // Snapshot the borrower marginfi-account's current debt-bank
    // liability shares BEFORE matching so we can compute the
    // post-CPI delta and stamp it onto the P2Pool MatchedLoan node.
    // Zero when no liability balance exists yet.
    let pre_borrow_liability_shares: u128 =
        read_debt_bank_liability_shares(borrower_marginfi_account.info, debt_bank.info.key)?;

    let result: PlaceOrderResult = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index =
            get_seat_index_with_hint(da.fixed, da.dynamic, payer.info.key, params.seat_index_hint)?;
        place_order_inner(
            da.fixed,
            da.dynamic,
            PlaceOrderArgs {
                market_pubkey: market_key,
                taker_seat_index: seat_index,
                side,
                kind: OrderKind::Primary,
                order_type,
                rate_bps: params.rate_bps,
                term_seconds: params.term_seconds,
                principal_atoms: params.principal_atoms,
                collateral_atoms: params.collateral_atoms,
                last_valid_unix_ts: params.last_valid_unix_ts,
                flags: params.flags,
                now_unix_ts: now,
                share_price_snapshot_fp48: snapshot_fp48,
                debt_oracle_price_fp48,
                collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48,
                enforce_ltv: true,
                is_vault_lender: false,
                borrower_ltv_bps: resolved_borrower_ltv_bps,
            },
            vault_account,
        )?
    };

    // Risk-profile maker bookkeeping is applied inline by the matching
    // engine (the gate writes `RiskProfile.encumbered_in_orders_atoms
    // +=` and `seat.deployed_atoms +=` at accept time, under the same
    // borrow as the gate read). The vault account is only consumed
    // if the matcher actually crossed a risk-profile maker; bind to
    // `_` here so Ask-side place_orders (which can't cross
    // risk-profile makers) don't trip the unused-warning.
    let _ = vault_account;

    // When place_order_inner converted a Bid residual into a P2Pool
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
        // borrower_debt_token is unused on this CPI path now (atoms
        // route through market_debt_vault). Keep it in the account
        // list for future ergonomics (e.g. opt-in immediate-liquidity
        // flag) and to avoid churning every place_order call site.
        let _ = borrower_debt_token;

        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        // The MatchedLoan tree node-index returned by `place_order_inner`
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

    if result.rested && is_not_nil!(result.rested_order_index) {
        emit_stack(OrderRestedLog {
            market: market_key,
            trader: *payer.info.key,
            sequence: result.sequence,
            principal_remaining_atoms: result.match_result.remaining_principal,
            last_valid_unix_ts: params.last_valid_unix_ts,
            rate_bps: params.rate_bps,
            side: side as u8,
            _padding: [0; 1],
            term_seconds: params.term_seconds,
        })?;
    } else if !result.rested && result.match_result.remaining_principal > 0 {
        emit_stack(OrderFilledIocLog {
            market: market_key,
            trader: *payer.info.key,
            sequence: result.sequence,
            principal_dropped_atoms: result.match_result.remaining_principal,
            side: side as u8,
            _padding: [0; 7],
        })?;
    }

    // Sync the signer's MarketPosition mirror after the primary
    // place_order debits encumbered balances on their seat.
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
