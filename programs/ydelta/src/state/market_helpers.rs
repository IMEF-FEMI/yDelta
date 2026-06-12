//! Mutation primitives over the market account's dynamic region: seat
//! balance bookkeeping, free-list allocation, the order-matching engine
//! (`match_order`, `match_borrower_bid`), risk-profile ask placement
//! (`rest_vault_ask`), and the P2Pool residual refinance scan
//! (`match_p2pool_residual_against_asks`). Each function pairs a
//! `MarketFixed` mutator with a `&mut [u8]` view of the tree region.

use hypertree::{
    is_not_nil, DataIndex, FreeList, HyperTreeReadOperations, HyperTreeValueIteratorTrait,
    HyperTreeWriteOperations, RedBlackTreeReadOnly, NIL,
};
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};

use super::claimed_seat::{ClaimedSeat, OWNER_KIND_USER};
use super::market::{
    get_helper_order, get_helper_seat, get_mut_helper_seat, Bookside, ClaimedSeatTree, MarketFixed,
    MarketRefMut, MarketUnusedFreeListPadding, MatchedLoan, MatchedLoanTree,
};
use super::resting_order::{order_type_can_take, OrderType, RestingOrder, Side};
use super::utils::assert_can_take;
use crate::logs::{emit_stack, MatchedLoanCreatedLog, OrderPlacedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_helper_risk_profile, get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;

/// One-atom dust cushion subtracted from per-profile idle capacity at
/// match time so the lender pool stays one atom above marginfi's
/// share-rounding floor.
pub const MARGINFI_ROUNDING_RESERVE_ATOMS: u64 = 1;

/// Pops a block off the market's free list and returns its index, or
/// `NIL` when the list is empty.
pub fn get_free_address_on_market_fixed(fixed: &mut MarketFixed, dynamic: &mut [u8]) -> DataIndex {
    let mut free_list: FreeList<MarketUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.free_list_head_index = free_list.get_head();
    free_address
}

/// Allocator alias used at seat-claim sites.
pub fn get_free_address_on_market_fixed_for_seat(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

/// Allocator alias used at ask-rest sites.
pub fn get_free_address_on_market_fixed_for_ask_order(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

/// Allocator alias used at matched-loan insert sites.
pub fn get_free_address_on_market_fixed_for_matched_loan(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

/// Pushes `index` back onto the market's free list.
pub fn release_address_on_market_fixed(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<MarketUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    free_list.add(index);
    fixed.free_list_head_index = free_list.get_head();
}

/// Grows the market's dynamic region by one block, pushing the new
/// block's address onto the free list. Caller is responsible for first
/// realloc'ing the underlying account.
pub fn market_expand(fixed: &mut MarketFixed, dynamic: &mut [u8]) -> ProgramResult {
    let mut free_list: FreeList<MarketUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    free_list.add(fixed.num_bytes_allocated);
    fixed.num_bytes_allocated = fixed
        .num_bytes_allocated
        .checked_add(super::constants::MARKET_BLOCK_SIZE as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    fixed.free_list_head_index = free_list.get_head();
    Ok(())
}

/// Selects which axis (debt or collateral) of a `ClaimedSeat` to mutate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceAxis {
    /// The debt-side shares fields.
    Debt,
    /// The collateral-side shares fields.
    Collateral,
}

/// Selects which bucket (withdrawable or encumbered) of a `ClaimedSeat`
/// to mutate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceBucket {
    /// Free balance, transferable out.
    Withdrawable,
    /// Locked balance backing an open order or active loan.
    Encumbered,
}

/// Direction of a balance update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceSign {
    /// Add to the selected bucket.
    Plus,
    /// Subtract from the selected bucket.
    Minus,
}

fn update_balance(
    seat: &mut ClaimedSeat,
    axis: BalanceAxis,
    bucket: BalanceBucket,
    sign: BalanceSign,
    atoms: u128,
) -> ProgramResult {
    let field: &mut u128 = match (axis, bucket) {
        (BalanceAxis::Debt, BalanceBucket::Withdrawable) => &mut seat.debt_withdrawable_shares,
        (BalanceAxis::Debt, BalanceBucket::Encumbered) => &mut seat.debt_encumbered_shares,
        (BalanceAxis::Collateral, BalanceBucket::Withdrawable) => {
            &mut seat.collateral_withdrawable_shares
        }
        (BalanceAxis::Collateral, BalanceBucket::Encumbered) => {
            &mut seat.collateral_encumbered_shares
        }
    };
    *field = match sign {
        BalanceSign::Plus => field
            .checked_add(atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?,
        BalanceSign::Minus => field
            .checked_sub(atoms)
            .ok_or(YdeltaError::InsufficientWithdrawableBalance)?,
    };
    Ok(())
}

/// Identity share-price (`1.0` in fp48) used where a real share price is
/// not yet wired in.
pub const PLACEHOLDER_SHARE_PRICE_FP48: crate::math::Fp48 = crate::math::Fp48::ONE;

/// Converts a u64 atom count to I80F48-scaled shares at the given share
/// price snapshot. Errors with `MathDivisionByZero` on a zero snapshot.
/// Result keeps the `value × 2^48` scale used by `ClaimedSeat.*_shares`.
pub fn atoms_to_shares_at_snapshot(
    atoms: u64,
    snapshot: crate::math::Fp48,
) -> Result<u128, ProgramError> {
    if snapshot.is_zero() {
        return Err(crate::program::YdeltaError::MathDivisionByZero.into());
    }
    // Result is I80F48-scaled shares (real_shares × 2^48). Stays raw u128
    // for now because the seat's `*_shares` fields are still u128; they
    // will be ported to [`Fp48`] in a follow-up stage.
    let atoms_fp48 = crate::math::Fp48::from_atoms(atoms);
    Ok(atoms_fp48.checked_div(snapshot)?.raw())
}

fn encumber_for_order(
    dynamic: &mut [u8],
    seat_index: DataIndex,
    side: Side,
    principal_shares: u128,
    collateral_shares: u128,
) -> ProgramResult {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    match side {
        Side::Bid => {
            update_balance(
                seat,
                BalanceAxis::Collateral,
                BalanceBucket::Withdrawable,
                BalanceSign::Minus,
                collateral_shares,
            )?;
            update_balance(
                seat,
                BalanceAxis::Collateral,
                BalanceBucket::Encumbered,
                BalanceSign::Plus,
                collateral_shares,
            )?;
        }
        Side::Ask => {
            update_balance(
                seat,
                BalanceAxis::Debt,
                BalanceBucket::Withdrawable,
                BalanceSign::Minus,
                principal_shares,
            )?;
            update_balance(
                seat,
                BalanceAxis::Debt,
                BalanceBucket::Encumbered,
                BalanceSign::Plus,
                principal_shares,
            )?;
            seat.open_lend_count = seat
                .open_lend_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }
    Ok(())
}

fn unencumber_for_order(
    dynamic: &mut [u8],
    seat_index: DataIndex,
    side: Side,
    principal_shares: u128,
    collateral_shares: u128,
) -> ProgramResult {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    match side {
        Side::Bid => {
            update_balance(
                seat,
                BalanceAxis::Collateral,
                BalanceBucket::Encumbered,
                BalanceSign::Minus,
                collateral_shares,
            )?;
            update_balance(
                seat,
                BalanceAxis::Collateral,
                BalanceBucket::Withdrawable,
                BalanceSign::Plus,
                collateral_shares,
            )?;
        }
        Side::Ask => {
            update_balance(
                seat,
                BalanceAxis::Debt,
                BalanceBucket::Encumbered,
                BalanceSign::Minus,
                principal_shares,
            )?;
            update_balance(
                seat,
                BalanceAxis::Debt,
                BalanceBucket::Withdrawable,
                BalanceSign::Plus,
                principal_shares,
            )?;
            seat.open_lend_count = seat
                .open_lend_count
                .checked_sub(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }
    Ok(())
}

/// Release a loan's encumbered collateral back to the seat's withdrawable
/// bucket at close. Hard-errors with `InsufficientEncumberedCollateral`
/// when the seat's encumbered bucket is smaller than the loan's recorded
/// `total_collateral_shares` — that means state has corrupted (collateral
/// moved without the loan-close accounting that should have decremented
/// `encumbered`), and silently clamping the release would permanently
/// strand the difference (neither released to withdrawable nor seized).
pub fn release_loan_collateral(
    dynamic: &mut [u8],
    seat_index: DataIndex,
    total_collateral_shares: u128,
    returned_shares: u128,
) -> ProgramResult {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    require!(
        seat.collateral_encumbered_shares >= total_collateral_shares,
        crate::program::YdeltaError::InsufficientEncumberedCollateral,
        "release_loan_collateral: seat.collateral_encumbered_shares ({}) < \
         loan.total_collateral_shares ({}) — refusing silent collateral drop",
        seat.collateral_encumbered_shares,
        total_collateral_shares,
    )?;
    seat.collateral_encumbered_shares = seat
        .collateral_encumbered_shares
        .checked_sub(total_collateral_shares)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let credit = returned_shares.min(total_collateral_shares);
    seat.collateral_withdrawable_shares = seat
        .collateral_withdrawable_shares
        .checked_add(credit)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(())
}

/// Resolves the user seat index for `signer`, validating a client-supplied
/// `hint` when present. Falls back to a tree lookup when the hint is
/// missing or invalid; errors with `NoSeatClaimed` when there is no seat.
pub fn get_seat_index_with_hint(
    fixed: &MarketFixed,
    dynamic: &[u8],
    signer: &Pubkey,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    if let Some(idx) = hint {
        if is_not_nil!(idx) {
            let seat: &ClaimedSeat = get_helper_seat(dynamic, idx).get_value();
            if seat.owner == *signer
                && seat.owner_kind == OWNER_KIND_USER
                && seat.risk_profile_id == 0
            {
                return Ok(idx);
            }
        }
    }
    let tree =
        RedBlackTreeReadOnly::<ClaimedSeat>::new(dynamic, fixed.claimed_seats_root_index, NIL);
    let probe = ClaimedSeat::new_empty(*signer, OWNER_KIND_USER, 0);
    let idx = tree.lookup_index(&probe);
    if !is_not_nil!(idx) {
        return Err(YdeltaError::NoSeatClaimed.into());
    }
    Ok(idx)
}

/// Inputs to [`match_order`]; one struct per call so the engine doesn't
/// take a 14-argument signature.
#[derive(Clone, Copy)]
pub struct MatchArgs {
    /// Market the match runs against.
    pub market_pubkey: Pubkey,
    /// Taker's seat index in the market.
    pub taker_seat_index: DataIndex,

    /// Side the taker is on (currently always `Bid`).
    pub side: Side,
    /// Taker's quoted rate.
    pub rate_bps: u16,
    /// Taker's requested term.
    pub term_seconds: u32,
    /// Taker's principal to fill.
    pub principal_atoms: u64,
    /// Taker's collateral attached to the bid.
    pub collateral_atoms: u64,
    /// Taker order type (controls residual handling).
    pub order_type: OrderType,
    /// Now-timestamp for expiry checks.
    pub now_unix_ts: i64,
    /// Market-level protocol-fee floor applied to the lender rate.
    pub fee_floor_bps: u16,

    /// Taker-side share-price snapshot used when minting MatchedLoan
    /// nodes.
    pub taker_share_price_snapshot_fp48: crate::math::Fp48,

    /// Debt-side oracle price for LTV gating.
    pub debt_oracle_price_fp48: crate::math::Fp48,

    /// Collateral-side oracle price for LTV gating.
    pub collateral_oracle_price_fp48: crate::math::Fp48,

    /// Marginfi `liability_weight_init` for the debt bank.
    pub debt_liability_weight_init_fp48: crate::math::Fp48,

    /// Marginfi `asset_weight_init` for the collateral bank.
    pub collateral_asset_weight_init_fp48: crate::math::Fp48,

    /// When `true`, gate each fill on the LTV requirement; otherwise
    /// match without an oracle check.
    pub enforce_ltv: bool,
}

/// Output of a single [`match_order`] / [`match_borrower_bid`] call.
#[derive(Default, Clone)]
pub struct MatchResult {
    /// Principal left unmatched after the scan.
    pub remaining_principal: u64,
    /// Collateral left attached to the residual.
    pub remaining_collateral: u64,
    /// Principal that successfully filled against resting asks.
    pub total_filled_principal: u64,
    /// Number of resting orders consumed.
    pub num_fills: u32,

    /// What to do with the residual (set by `match_borrower_bid` based
    /// on the `FLAG_OB_ONLY` bit).
    pub residual_action: ResidualAction,
}

/// What the placement path does with an unfilled residual.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResidualAction {
    /// Cancel the leftover and unwind its encumbrance.
    #[default]
    Drop,

    /// Open a P2Pool (variable-rate marginfi-backed) loan for the
    /// residual.
    P2PoolBorrow,
}

/// Per-order flag: when set on a borrower bid, the residual is dropped
/// rather than spilling into a P2Pool loan.
pub const FLAG_OB_ONLY: u8 = 0b0000_0010;

/// Walks the ask bookside from best to worst rate, filling against
/// risk-profile makers up to the taker's principal. Mutates seat counters
/// and the profile's `encumbered_in_orders_atoms`, emits
/// `MatchedLoanCreated` logs, and inserts a `MatchedLoan` node per fill.
/// `vault_ai` is the resting-ask vault account; passing `None` skips
/// every fill (used in tests).
pub fn match_order(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: MatchArgs,
    vault_ai: Option<&solana_program::account_info::AccountInfo<'_>>,
) -> Result<MatchResult, ProgramError> {
    let mut remaining_principal = args.principal_atoms;
    let mut remaining_collateral = args.collateral_atoms;
    let mut total_filled = 0u64;
    let mut num_fills = 0u32;

    let mut current_maker_index: DataIndex = fixed.asks_best_index;

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        require!(
            maker.trader_seat_index != args.taker_seat_index,
            YdeltaError::SelfMatchForbidden,
            "taker seat {} matches its own maker order at index {}",
            args.taker_seat_index,
            current_maker_index
        )?;

        let (bid_rate, ask_rate, bid_term, ask_term) = (
            args.rate_bps,
            maker.rate_bps,
            args.term_seconds,
            maker.term_seconds,
        );
        if bid_rate < ask_rate {
            break;
        }
        if bid_term > ask_term {
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        if !order_type_can_take(args.order_type) {
            assert_can_take(args.order_type)?;
        }

        let matched_principal: u64;
        let profile_id: u8;

        let mut profile_max_ltv_bps: u16 = 0;
        let mut profile_max_term_seconds: u32 = 0;
        {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            require!(
                lender_seat.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE,
                YdeltaError::IncorrectAccount,
                "resting ask's lender seat owner_kind is {} (expected RISK_PROFILE)",
                lender_seat.owner_kind,
            )?;
            let vault_ai_ref = match vault_ai {
                Some(v) => v,
                None => {
                    current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                    continue;
                }
            };
            profile_id = lender_seat.risk_profile_id;

            let profile_idle: u64 = {
                let vault_data = vault_ai_ref.try_borrow_data()?;
                let (fixed_bytes, vault_dyn) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
                let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
                let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
                let tree =
                    RiskProfileTreeReadOnly::new(vault_dyn, header.risk_profiles_root_index, NIL);
                let idx = tree.lookup_index(&probe);
                if idx == NIL {
                    0
                } else {
                    let p = get_helper_risk_profile(vault_dyn, idx).get_value();
                    if p.is_sunset != 0 {
                        0
                    } else {
                        profile_max_ltv_bps = p.max_ltv_bps;
                        profile_max_term_seconds = p.max_term_seconds;
                        p.total_principal_atoms
                            .saturating_sub(p.deployed_principal_atoms)
                            .saturating_sub(p.encumbered_in_orders_atoms)
                            .saturating_sub(MARGINFI_ROUNDING_RESERVE_ATOMS)
                    }
                }
            };
            if profile_idle == 0 {
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            if profile_max_term_seconds > 0 && maker.term_seconds > profile_max_term_seconds {
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            matched_principal = remaining_principal.min(profile_idle);
        }

        let matched_collateral_taker = if matched_principal == remaining_principal {
            remaining_collateral
        } else {
            mul_div_u64(
                matched_principal,
                args.collateral_atoms,
                args.principal_atoms,
            )?
        };
        let matched_collateral_maker = 0u64;
        let total_collateral_for_match = matched_collateral_taker
            .checked_add(matched_collateral_maker)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if args.enforce_ltv {
            let required_collateral =
                crate::state::ltv::get_required_quote_collateral_to_back_debt(
                    matched_principal,
                    args.debt_oracle_price_fp48,
                    args.collateral_oracle_price_fp48,
                    args.debt_liability_weight_init_fp48,
                    args.collateral_asset_weight_init_fp48,
                    fixed.fee_config.ltv_buffer_bps,
                    fixed.debt_mint_decimals,
                    fixed.collateral_mint_decimals,
                )?;
            require!(
                total_collateral_for_match >= required_collateral,
                YdeltaError::CollateralBelowMatchLTV,
                "matched collateral {} < required {} at oracle prices",
                total_collateral_for_match,
                required_collateral
            )?;

            // Stored cap only — the marginfi-auto sentinel was removed
            // (v1 D17): the curator's cap is the lender-side LTV policy.
            if profile_max_ltv_bps > 0 {
                let collateral_asset_weight_fp48 =
                    crate::math::to_scaled(profile_max_ltv_bps as u128)?
                        / 10_000u128;
                let required_at_profile_cap =
                    crate::state::ltv::get_required_quote_collateral_to_back_debt(
                        matched_principal,
                        args.debt_oracle_price_fp48,
                        args.collateral_oracle_price_fp48,
                        crate::math::Fp48::ONE,
                        crate::math::Fp48::from_raw(collateral_asset_weight_fp48),
                        0,
                        fixed.debt_mint_decimals,
                        fixed.collateral_mint_decimals,
                    )?;
                // The profile cap is per-ask policy, not a property of the
                // bid: a stricter profile skips and the scan walks on to
                // asks whose cap the bid's collateral does satisfy.
                if total_collateral_for_match < required_at_profile_cap {
                    current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                    continue;
                }
            }
        }

        // Every gate has passed — only now reserve the fill on the profile,
        // so a skipped ask leaves no stale encumbrance behind.
        if let Some(vault_ai_ref) = vault_ai {
            let mut vault_data = vault_ai_ref.try_borrow_mut_data()?;
            let (fixed_bytes, vault_dyn) = vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
            let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
            let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
            let tree =
                RiskProfileTreeReadOnly::new(vault_dyn, header.risk_profiles_root_index, NIL);
            let idx = tree.lookup_index(&probe);
            if idx != NIL {
                let p = get_mut_helper_risk_profile(vault_dyn, idx).get_mut_value();
                p.encumbered_in_orders_atoms = p
                    .encumbered_in_orders_atoms
                    .checked_add(matched_principal)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }

        let maker_snapshot = maker.share_price_snapshot();

        let taker_snapshot = args.taker_share_price_snapshot_fp48;

        let lender_rate = ask_rate;

        let floored_lender_rate: u32 = (ask_rate as u32) + (args.fee_floor_bps as u32);
        require!(
            floored_lender_rate <= u16::MAX as u32,
            YdeltaError::InvalidArgument,
            "ask_rate {} + fee_floor {} exceeds u16::MAX — borrower rate \
             would clamp and under-collect the protocol floor",
            ask_rate,
            args.fee_floor_bps
        )?;
        let borrower_rate = bid_rate.max(floored_lender_rate as u16);
        let (lender_seat_index, borrower_seat_index, term) =
            (maker.trader_seat_index, args.taker_seat_index, bid_term);

        let total_collateral = total_collateral_for_match;
        let origination_atoms = crate::math::mul_div_u64(
            matched_principal,
            fixed.fee_config.origination_bps as u64,
            crate::state::loan::BPS_PER_UNIT as u64,
            false,
        )?;

        let vault_flag: u8 = crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER;

        let (lender_debt_snapshot, borrower_collateral_snapshot) = (maker_snapshot, taker_snapshot);

        let sequence = fixed.matched_loan_sequence;
        let mut node: MatchedLoan = Default::default();
        node.sequence = sequence;
        node.principal_atoms = matched_principal;
        node.origination_atoms = origination_atoms;
        node.collateral_atoms = total_collateral;
        node.matched_at_unix = args.now_unix_ts;
        node.lender_seat_index = lender_seat_index;
        node.borrower_seat_index = borrower_seat_index;
        node.term_seconds = term;
        node.borrower_rate_bps = borrower_rate;
        node.lender_rate_bps = lender_rate;
        node.loan_type = 0;
        node.flags = vault_flag;
        node.curator_fee_bps_snapshot = fixed.fee_config.curator_fee_bps;
        node.lender_debt_share_price_snapshot_fp48 = lender_debt_snapshot;
        node.borrower_collateral_share_price_snapshot_fp48 = borrower_collateral_snapshot;
        let node_index = get_free_address_on_market_fixed_for_matched_loan(fixed, dynamic);
        require!(
            is_not_nil!(node_index),
            ProgramError::AccountDataTooSmall,
            "No free block for MatchedLoan — expand market"
        )?;
        let mut matched_tree = MatchedLoanTree::new(dynamic, fixed.matched_loans_root_index, NIL);
        matched_tree.insert(node_index, node);
        fixed.matched_loans_root_index = matched_tree.get_root_index();
        drop(matched_tree);

        emit_stack(MatchedLoanCreatedLog {
            market: args.market_pubkey,
            loan_pda: Pubkey::default(),
            sequence,
            lender_seat_index,
            borrower_seat_index,
            principal_atoms: matched_principal,
            collateral_atoms: total_collateral,
            borrower_rate_bps: borrower_rate,
            lender_rate_bps: lender_rate,
            term_seconds: term,
            matched_at_unix: args.now_unix_ts,
            loan_type: 0,
            flags: vault_flag,
            _padding: [0; 6],
        })?;

        fixed.matched_loan_sequence = fixed
            .matched_loan_sequence
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        {
            let seat = get_mut_helper_seat(dynamic, borrower_seat_index).get_mut_value();
            seat.open_borrow_count = seat
                .open_borrow_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        // `open_lend_count` counts resting asks plus active loans; the loan
        // close-out paths decrement it, so each fill must stamp it here.
        {
            let seat = get_mut_helper_seat(dynamic, lender_seat_index).get_mut_value();
            seat.open_lend_count = seat
                .open_lend_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        remaining_principal = remaining_principal
            .checked_sub(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        remaining_collateral = remaining_collateral
            .checked_sub(matched_collateral_taker)
            .unwrap_or(0);
        total_filled = total_filled
            .checked_add(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        num_fills = num_fills.saturating_add(1);

        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
    }

    Ok(MatchResult {
        remaining_principal,
        remaining_collateral,
        total_filled_principal: total_filled,
        num_fills,

        residual_action: ResidualAction::Drop,
    })
}

fn next_maker_index(fixed: &MarketFixed, dynamic: &[u8], current: DataIndex) -> DataIndex {
    let tree: super::market::BooksideReadOnly =
        super::market::BooksideReadOnly::new(dynamic, fixed.asks_root_index, fixed.asks_best_index);
    tree.get_next_lower_index::<RestingOrder>(current)
}

/// Removes the ask at `order_index` from the bookside tree and returns
/// its block to the free list. Caller is responsible for any seat-side
/// unencumber that the order required.
pub fn remove_order_from_tree_and_free(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    order_index: DataIndex,
) {
    let mut tree: Bookside = Bookside::new(dynamic, fixed.asks_root_index, fixed.asks_best_index);
    tree.remove_by_index(order_index);
    fixed.asks_root_index = tree.get_root_index();
    fixed.asks_best_index = tree.get_max_index();
    release_address_on_market_fixed(fixed, dynamic, order_index);
}

fn mul_div_u64(a: u64, b: u64, c: u64) -> Result<u64, ProgramError> {
    crate::math::mul_div_u64(a, b, c, false)
}

impl<'a> MarketRefMut<'a> {
    /// Convenience wrapper that claims a seat with `risk_profile_id = 0`
    /// (the user-seat default).
    pub fn claim_seat(&mut self, owner: &Pubkey, owner_kind: u8) -> ProgramResult {
        self.claim_seat_with_profile(owner, owner_kind, 0)
    }

    /// Inserts a fresh, zeroed `ClaimedSeat` into the market's seat tree.
    /// Errors with `AlreadyClaimedSeat` when one already exists for the
    /// `(owner, owner_kind, risk_profile_id)` triple.
    pub fn claim_seat_with_profile(
        &mut self,
        owner: &Pubkey,
        owner_kind: u8,
        risk_profile_id: u8,
    ) -> ProgramResult {
        let MarketRefMut { fixed, dynamic } = self;

        let probe = ClaimedSeat::new_empty(*owner, owner_kind, risk_profile_id);
        let lookup_tree =
            RedBlackTreeReadOnly::<ClaimedSeat>::new(dynamic, fixed.claimed_seats_root_index, NIL);
        if is_not_nil!(lookup_tree.lookup_index(&probe)) {
            return Err(YdeltaError::AlreadyClaimedSeat.into());
        }
        drop(lookup_tree);

        let free_addr: DataIndex = get_free_address_on_market_fixed_for_seat(fixed, dynamic);
        require!(
            is_not_nil!(free_addr),
            ProgramError::AccountDataTooSmall,
            "Market account has no free block — expand before claim_seat"
        )?;

        let seat = ClaimedSeat::new_empty(*owner, owner_kind, risk_profile_id);
        let mut tree = ClaimedSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.insert(free_addr, seat);
        fixed.claimed_seats_root_index = tree.get_root_index();
        fixed.position_count = fixed
            .position_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }

    /// Adds `shares` to the seat's withdrawable bucket on the chosen
    /// axis (`is_debt` selects debt vs collateral).
    pub fn deposit_to_seat(
        &mut self,
        seat_index: DataIndex,
        shares: u128,
        is_debt: bool,
    ) -> ProgramResult {
        let MarketRefMut { dynamic, .. } = self;
        let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
        let axis = if is_debt {
            BalanceAxis::Debt
        } else {
            BalanceAxis::Collateral
        };
        update_balance(
            seat,
            axis,
            BalanceBucket::Withdrawable,
            BalanceSign::Plus,
            shares,
        )
    }

    /// Removes `shares` from the seat's withdrawable bucket on the
    /// chosen axis; errors with `InsufficientWithdrawableBalance` when
    /// the bucket is too small.
    pub fn withdraw_from_seat(
        &mut self,
        seat_index: DataIndex,
        shares: u128,
        is_debt: bool,
    ) -> ProgramResult {
        let MarketRefMut { dynamic, .. } = self;
        let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
        let axis = if is_debt {
            BalanceAxis::Debt
        } else {
            BalanceAxis::Collateral
        };
        update_balance(
            seat,
            axis,
            BalanceBucket::Withdrawable,
            BalanceSign::Minus,
            shares,
        )
    }

    /// Reads the seat's withdrawable balance on the chosen axis.
    pub fn withdrawable_shares_for_seat(&self, seat_index: DataIndex, is_debt: bool) -> u128 {
        let MarketRefMut { dynamic, .. } = self;
        let seat = get_helper_seat(dynamic, seat_index).get_value();
        if is_debt {
            seat.debt_withdrawable_shares
        } else {
            seat.collateral_withdrawable_shares
        }
    }

    /// Inserts an ask `order` at `order_index` into the market's
    /// bookside tree. Errors with `InvalidArgument` if a bid is supplied
    /// (bids cannot rest in the quote-only model).
    pub fn rest_order(&mut self, order_index: DataIndex, order: RestingOrder) -> ProgramResult {
        let MarketRefMut { fixed, dynamic } = self;
        require!(
            order.side == Side::Ask as u8,
            YdeltaError::InvalidArgument,
            "rest_order: bids cannot rest in the quote-only model (got side={})",
            order.side,
        )?;
        let mut tree: Bookside =
            Bookside::new(dynamic, fixed.asks_root_index, fixed.asks_best_index);
        tree.insert(order_index, order);
        fixed.asks_root_index = tree.get_root_index();
        fixed.asks_best_index = tree.get_max_index();
        Ok(())
    }
}

/// Inputs to [`match_borrower_bid`] — the borrower-side place_order
/// driver that bundles encumbrance, match, and residual handling.
#[derive(Clone, Copy)]
pub struct PlaceOrderArgs {
    /// Market the order is placed against.
    pub market_pubkey: Pubkey,
    /// Taker's seat index in the market.
    pub taker_seat_index: DataIndex,

    /// Side (always `Bid` for borrowers).
    pub side: Side,

    /// Order type semantics.
    pub order_type: OrderType,
    /// Quoted bid rate.
    pub rate_bps: u16,
    /// Loan term in seconds.
    pub term_seconds: u32,
    /// Principal to fill.
    pub principal_atoms: u64,
    /// Collateral attached to the bid.
    pub collateral_atoms: u64,
    /// Order flags (e.g. [`FLAG_OB_ONLY`]).
    pub flags: u8,
    /// Now-timestamp.
    pub now_unix_ts: i64,

    /// Taker-side share-price snapshot.
    pub share_price_snapshot_fp48: crate::math::Fp48,

    /// Debt oracle price.
    pub debt_oracle_price_fp48: crate::math::Fp48,
    /// Collateral oracle price.
    pub collateral_oracle_price_fp48: crate::math::Fp48,
    /// Marginfi `liability_weight_init` for the debt bank.
    pub debt_liability_weight_init_fp48: crate::math::Fp48,
    /// Marginfi `asset_weight_init` for the collateral bank.
    pub collateral_asset_weight_init_fp48: crate::math::Fp48,

    /// Gate match fills on the LTV requirement.
    pub enforce_ltv: bool,
}

/// Inputs to [`rest_vault_ask`].
#[derive(Clone, Copy)]
pub struct RestVaultAskArgs {
    /// Market the ask is placed against.
    pub market_pubkey: Pubkey,

    /// Curator's risk-profile seat index in the market.
    pub maker_seat_index: DataIndex,
    /// Quoted ask rate.
    pub rate_bps: u16,
    /// Loan term in seconds.
    pub term_seconds: u32,
    /// Order flags.
    pub flags: u8,
    /// Now-timestamp.
    pub now_unix_ts: i64,
}

/// Output of [`match_borrower_bid`].
#[derive(Clone)]
pub struct PlaceOrderResult {
    /// Per-market order sequence assigned to this placement.
    pub sequence: u64,
    /// Underlying match outcome.
    pub match_result: MatchResult,

    /// `MatchedLoan` index for the P2Pool residual, or `NIL` if none.
    pub p2pool_loan_index: DataIndex,

    /// Sequence assigned to the P2Pool residual `MatchedLoan` (0 when
    /// none).
    pub p2pool_loan_sequence: u64,
}

impl Default for PlaceOrderResult {
    fn default() -> Self {
        Self {
            sequence: 0,
            match_result: MatchResult::default(),
            p2pool_loan_index: NIL,
            p2pool_loan_sequence: 0,
        }
    }
}

/// Borrower-side place_order driver. Encumbers the taker's collateral
/// and principal-shadow on its seat, runs [`match_order`], then either
/// opens a P2Pool `MatchedLoan` for the residual (when `OB_ONLY` is off)
/// or unwinds the residual encumbrance.
pub fn match_borrower_bid(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: PlaceOrderArgs,
    vault_ai: Option<&solana_program::account_info::AccountInfo<'_>>,
) -> Result<PlaceOrderResult, ProgramError> {
    require!(
        args.principal_atoms >= super::constants::MIN_PRINCIPAL_ATOMS,
        YdeltaError::CollateralInsufficient,
        "principal_atoms below minimum"
    )?;
    require!(
        args.term_seconds > 0,
        YdeltaError::TermNotCompatible,
        "term_seconds must be > 0"
    )?;
    require!(
        args.collateral_atoms > 0,
        YdeltaError::CollateralInsufficient,
        "borrower bid requires collateral atoms"
    )?;

    let snapshot = args.share_price_snapshot_fp48;
    let principal_shares = atoms_to_shares_at_snapshot(args.principal_atoms, snapshot)?;
    let collateral_shares = atoms_to_shares_at_snapshot(args.collateral_atoms, snapshot)?;
    encumber_for_order(
        dynamic,
        args.taker_seat_index,
        Side::Bid,
        principal_shares,
        collateral_shares,
    )?;

    let seq = fixed.order_sequence_number;

    fixed.order_sequence_number = seq.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;

    emit_stack(OrderPlacedLog {
        market: args.market_pubkey,
        trader: get_helper_seat(dynamic, args.taker_seat_index)
            .get_value()
            .owner,
        trader_seat_index: args.taker_seat_index,
        side: Side::Bid as u8,
        _reserved_kind: 0,
        order_type: OrderType::ImmediateOrCancel as u8,
        _padding1: 0,
        rate_bps: args.rate_bps,
        _padding2: 0,
        term_seconds: args.term_seconds,
        principal_atoms: args.principal_atoms,
        collateral_atoms: args.collateral_atoms,
        sequence: seq,
        last_valid_unix_ts: super::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
    })?;

    let mut match_result = match_order(
        fixed,
        dynamic,
        MatchArgs {
            market_pubkey: args.market_pubkey,
            taker_seat_index: args.taker_seat_index,
            side: Side::Bid,
            rate_bps: args.rate_bps,
            term_seconds: args.term_seconds,
            principal_atoms: args.principal_atoms,
            collateral_atoms: args.collateral_atoms,
            order_type: OrderType::ImmediateOrCancel,
            now_unix_ts: args.now_unix_ts,
            fee_floor_bps: fixed.fee_config.protocol_fee_bps_floor,
            taker_share_price_snapshot_fp48: snapshot,
            debt_oracle_price_fp48: args.debt_oracle_price_fp48,
            collateral_oracle_price_fp48: args.collateral_oracle_price_fp48,
            debt_liability_weight_init_fp48: args.debt_liability_weight_init_fp48,
            collateral_asset_weight_init_fp48: args.collateral_asset_weight_init_fp48,
            enforce_ltv: args.enforce_ltv,
        },
        vault_ai,
    )?;

    let ob_only = (args.flags & FLAG_OB_ONLY) != 0;
    match_result.residual_action = if !ob_only && match_result.remaining_principal > 0 {
        ResidualAction::P2PoolBorrow
    } else {
        ResidualAction::Drop
    };

    let mut p2pool_loan_index: DataIndex = NIL;
    let mut p2pool_loan_sequence: u64 = 0;

    match (
        match_result.residual_action,
        match_result.remaining_principal > 0,
    ) {
        (ResidualAction::P2PoolBorrow, true) => {
            {
                let seat = get_mut_helper_seat(dynamic, args.taker_seat_index).get_mut_value();
                seat.open_borrow_count = seat
                    .open_borrow_count
                    .checked_add(1)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }

            let origination_atoms = crate::math::mul_div_u64(
                match_result.remaining_principal,
                fixed.fee_config.origination_bps as u64,
                crate::state::loan::BPS_PER_UNIT as u64,
                false,
            )?;

            let sequence = fixed.matched_loan_sequence;
            let mut node: MatchedLoan = Default::default();
            node.sequence = sequence;
            node.principal_atoms = match_result.remaining_principal;
            node.origination_atoms = origination_atoms;
            node.collateral_atoms = match_result.remaining_collateral;
            node.matched_at_unix = args.now_unix_ts;
            node.lender_seat_index = NIL;
            node.borrower_seat_index = args.taker_seat_index;
            node.term_seconds = args.term_seconds;
            node.borrower_rate_bps = args.rate_bps;
            node.lender_rate_bps = args.rate_bps;
            node.loan_type = 1;
            node.borrower_marginfi_borrow_shares = 0;
            // P2Pool residual has no curator-fee path (no vault lender),
            // but snapshot anyway so the field has a well-defined source
            // and process_matched_loan can read it uniformly.
            node.curator_fee_bps_snapshot = fixed.fee_config.curator_fee_bps;

            node.borrower_collateral_share_price_snapshot_fp48 = snapshot;

            let node_index = get_free_address_on_market_fixed_for_matched_loan(fixed, dynamic);
            require!(
                is_not_nil!(node_index),
                ProgramError::AccountDataTooSmall,
                "No free block for P2Pool MatchedLoan — expand market"
            )?;
            let mut matched_tree =
                MatchedLoanTree::new(dynamic, fixed.matched_loans_root_index, NIL);
            matched_tree.insert(node_index, node);
            fixed.matched_loans_root_index = matched_tree.get_root_index();
            drop(matched_tree);

            emit_stack(MatchedLoanCreatedLog {
                market: args.market_pubkey,
                loan_pda: Pubkey::default(),
                sequence,
                lender_seat_index: NIL,
                borrower_seat_index: args.taker_seat_index,
                principal_atoms: match_result.remaining_principal,
                collateral_atoms: match_result.remaining_collateral,
                borrower_rate_bps: args.rate_bps,
                lender_rate_bps: args.rate_bps,
                term_seconds: args.term_seconds,
                matched_at_unix: args.now_unix_ts,
                loan_type: 1,
                flags: 0,
                _padding: [0; 6],
            })?;

            fixed.matched_loan_sequence = fixed
                .matched_loan_sequence
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            p2pool_loan_index = node_index;
            p2pool_loan_sequence = sequence;
        }
        (ResidualAction::Drop, true) => {
            unencumber_for_order(
                dynamic,
                args.taker_seat_index,
                Side::Bid,
                atoms_to_shares_at_snapshot(match_result.remaining_principal, snapshot)?,
                atoms_to_shares_at_snapshot(match_result.remaining_collateral, snapshot)?,
            )?;
        }
        (_, false) => {}
    }

    Ok(PlaceOrderResult {
        sequence: seq,
        match_result,
        p2pool_loan_index,
        p2pool_loan_sequence,
    })
}

/// Rests a curator's risk-profile ask on the book. Vault asks are
/// unbounded (`principal_atoms = u64::MAX`) and zero-collateral: the
/// per-fill cap comes from the profile's idle balance at match time.
/// Returns the assigned per-market order sequence.
pub fn rest_vault_ask(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: RestVaultAskArgs,
) -> Result<u64, ProgramError> {
    require!(
        args.term_seconds > 0,
        YdeltaError::TermNotCompatible,
        "term_seconds must be > 0"
    )?;

    {
        let seat = get_mut_helper_seat(dynamic, args.maker_seat_index).get_mut_value();
        seat.open_lend_count = seat
            .open_lend_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    let seq = fixed.order_sequence_number;

    fixed.order_sequence_number = seq.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;

    emit_stack(OrderPlacedLog {
        market: args.market_pubkey,
        trader: get_helper_seat(dynamic, args.maker_seat_index)
            .get_value()
            .owner,
        trader_seat_index: args.maker_seat_index,
        side: Side::Ask as u8,
        _reserved_kind: 0,
        order_type: OrderType::PostOnly as u8,
        _padding1: 0,
        rate_bps: args.rate_bps,
        _padding2: 0,
        term_seconds: args.term_seconds,

        principal_atoms: u64::MAX,
        collateral_atoms: 0,
        sequence: seq,
        last_valid_unix_ts: super::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
    })?;

    let order_index = get_free_address_on_market_fixed_for_ask_order(fixed, dynamic);
    require!(
        is_not_nil!(order_index),
        ProgramError::AccountDataTooSmall,
        "No free block for vault ask — expand market"
    )?;

    let resting = RestingOrder::new_primary(
        args.maker_seat_index,
        seq,
        Side::Ask,
        OrderType::PostOnly,
        args.rate_bps,
        args.term_seconds,
        u64::MAX,
        0,
        super::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
        args.flags,
        crate::math::Fp48::ZERO,
    );
    let mut market = MarketRefMut { fixed, dynamic };
    market.rest_order(order_index, resting)?;
    Ok(seq)
}

/// Locates the ask owned by `trader_seat_index` with `sequence_number ==
/// sequence`. Validates a client-supplied `hint` when present, then falls
/// back to a tree scan; errors with `OrderNotFound` when missing.
pub fn lookup_order_by_seq(
    fixed: &MarketFixed,
    dynamic: &[u8],
    trader_seat_index: DataIndex,
    sequence: u64,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    if let Some(idx) = hint {
        if is_not_nil!(idx) {
            let order: &RestingOrder = get_helper_order(dynamic, idx).get_value();
            if order.trader_seat_index == trader_seat_index && order.sequence_number == sequence {
                return Ok(idx);
            }
            return Err(YdeltaError::WrongIndexHintParams.into());
        }
    }

    let tree = RedBlackTreeReadOnly::<RestingOrder>::new(
        dynamic,
        fixed.asks_root_index,
        fixed.asks_best_index,
    );
    for (idx, order) in tree.iter::<RestingOrder>() {
        if order.trader_seat_index == trader_seat_index && order.sequence_number == sequence {
            return Ok(idx);
        }
    }
    Err(YdeltaError::OrderNotFound.into())
}

/// Cancels the ask at `order_index`. For risk-profile asks decrements
/// `open_lend_count`; for user asks unencumbers the seat's debt /
/// collateral shares using the order's stored share-price snapshot.
/// Errors with `OrderNotOwnedBySigner` when the caller's seat doesn't
/// own the order.
pub fn cancel_order_by_index(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    signer_seat_index: DataIndex,
    order_index: DataIndex,
) -> ProgramResult {
    let order: RestingOrder = *get_helper_order(dynamic, order_index).get_value();
    require!(
        order.trader_seat_index == signer_seat_index,
        YdeltaError::OrderNotOwnedBySigner,
        "Order owned by a different seat"
    )?;

    let owner_kind: u8 = {
        let seat = get_helper_seat(dynamic, order.trader_seat_index).get_value();
        seat.owner_kind
    };
    if owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
        let seat = get_mut_helper_seat(dynamic, order.trader_seat_index).get_mut_value();
        seat.open_lend_count = seat
            .open_lend_count
            .checked_sub(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        remove_order_from_tree_and_free(fixed, dynamic, order_index);
        return Ok(());
    }

    let snapshot = order.share_price_snapshot();
    unencumber_for_order(
        dynamic,
        order.trader_seat_index,
        Side::Ask,
        atoms_to_shares_at_snapshot(order.principal_atoms, snapshot)?,
        atoms_to_shares_at_snapshot(order.collateral_atoms, snapshot)?,
    )?;
    remove_order_from_tree_and_free(fixed, dynamic, order_index);
    Ok(())
}

/// Inputs to [`match_p2pool_residual_against_asks`] — the engine that
/// converts an open P2Pool loan into one or more fixed-term loans by
/// matching against resting risk-profile asks.
pub struct MatchP2PoolRefinanceArgs {
    /// Market the refinance runs against.
    pub market_pubkey: Pubkey,

    /// Borrower's seat index in the market.
    pub borrower_seat_index: DataIndex,

    /// Maximum principal to refinance.
    pub principal_cap_atoms: u64,

    /// Collateral attached to the original P2Pool loan; sliced
    /// proportionally to each fill.
    pub loan_collateral_atoms: u64,

    /// Borrower-side collateral share-price snapshot to stamp on each
    /// minted `MatchedLoan`.
    pub borrower_collateral_share_price_snapshot_fp48: crate::math::Fp48,

    /// Remaining time on the borrower's intended fixed term.
    pub term_remaining_seconds: u32,

    /// Highest rate the borrower will accept; asks above this break the
    /// scan.
    pub max_acceptable_rate_bps: u16,

    /// Market protocol-fee floor applied to the lender rate.
    pub fee_floor_bps: u16,
    /// Now-timestamp.
    pub now_unix_ts: i64,

    /// Debt oracle price.
    pub debt_oracle_price_fp48: crate::math::Fp48,

    /// Collateral oracle price.
    pub collateral_oracle_price_fp48: crate::math::Fp48,

    /// Marginfi `liability_weight_init` for the debt bank.
    pub debt_liability_weight_init_fp48: crate::math::Fp48,

    /// Marginfi `asset_weight_init` for the collateral bank.
    pub collateral_asset_weight_init_fp48: crate::math::Fp48,

    /// LTV top-up buffer in bps.
    pub ltv_buffer_bps: u16,

    /// Debt mint decimals.
    pub debt_mint_decimals: u8,

    /// Collateral mint decimals.
    pub collateral_mint_decimals: u8,
}

/// One fill produced by [`match_p2pool_residual_against_asks`]; the
/// processor uses these to drive per-lender marginfi accounting.
#[derive(Clone, Copy)]
pub struct P2PoolRefinanceCross {
    /// Lender risk-profile id that filled.
    pub lender_profile_id: u8,

    /// Effective lender rate.
    pub lender_rate_bps: u16,

    /// Principal filled by this cross.
    pub filled_principal_atoms: u64,
}

/// Output of [`match_p2pool_residual_against_asks`].
#[derive(Default)]
pub struct MatchP2PoolRefinanceResult {
    /// Total principal that crossed.
    pub total_filled_principal_atoms: u64,

    /// Total collateral that crossed.
    pub total_filled_collateral_atoms: u64,
    /// Number of fills produced.
    pub num_fills: u32,

    /// Per-cross detail, one entry per fill.
    pub crosses: Vec<P2PoolRefinanceCross>,
}

/// Variant of [`match_order`] that targets an existing P2Pool loan. Walks
/// the ask book, mints one pre-settled `MatchedLoan` per fill (carrying
/// the `VAULT_PRESETTLED | VAULT_LENDER` flags), and returns enough
/// per-cross detail for `convert_p2pool_to_fixed` to do the marginfi
/// share transfers itself.
pub fn match_p2pool_residual_against_asks(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: MatchP2PoolRefinanceArgs,
    vault_ai: Option<&solana_program::account_info::AccountInfo<'_>>,
) -> Result<MatchP2PoolRefinanceResult, ProgramError> {
    let mut remaining_principal = args.principal_cap_atoms;
    let mut total_filled_principal: u64 = 0;
    let mut total_filled_collateral: u64 = 0;
    let mut num_fills: u32 = 0;
    let mut crosses: Vec<P2PoolRefinanceCross> = Vec::new();
    let mut current_maker_index: DataIndex = fixed.asks_best_index;

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        require!(
            maker.trader_seat_index != args.borrower_seat_index,
            YdeltaError::SelfMatchForbidden,
            "convert refinance: borrower seat {} matches their own resting ask at index {}",
            args.borrower_seat_index,
            current_maker_index
        )?;

        if maker.rate_bps > args.max_acceptable_rate_bps {
            break;
        }

        if maker.term_seconds < args.term_remaining_seconds {
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        let matched_principal: u64;
        let lender_profile_id: u8;

        let mut profile_max_ltv_bps: u16 = 0;
        {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            require!(
                lender_seat.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE,
                YdeltaError::IncorrectAccount,
                "resting ask's lender seat owner_kind is {} (expected RISK_PROFILE)",
                lender_seat.owner_kind,
            )?;
            let vault_ai_ref = match vault_ai {
                Some(v) => v,
                None => {
                    current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                    continue;
                }
            };
            let profile_id = lender_seat.risk_profile_id;
            lender_profile_id = profile_id;
            let profile_idle: u64 = {
                let vault_data = vault_ai_ref.try_borrow_data()?;
                let (fixed_bytes, vault_dyn) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
                let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
                let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
                let tree =
                    RiskProfileTreeReadOnly::new(vault_dyn, header.risk_profiles_root_index, NIL);
                let idx = tree.lookup_index(&probe);
                if idx == NIL {
                    0
                } else {
                    let p = get_helper_risk_profile(vault_dyn, idx).get_value();
                    if p.is_sunset != 0 {
                        0
                    } else {
                        profile_max_ltv_bps = p.max_ltv_bps;
                        p.total_principal_atoms
                            .saturating_sub(p.deployed_principal_atoms)
                            .saturating_sub(p.encumbered_in_orders_atoms)
                            .saturating_sub(MARGINFI_ROUNDING_RESERVE_ATOMS)
                    }
                }
            };
            if profile_idle == 0 {
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            matched_principal = remaining_principal.min(profile_idle);

            let matched_collateral_for_gate: u64 = crate::math::mul_div_u64(
                args.loan_collateral_atoms,
                matched_principal,
                args.principal_cap_atoms,
                false,
            )?;
            {
                let required_collateral =
                    crate::state::ltv::get_required_quote_collateral_to_back_debt(
                        matched_principal,
                        args.debt_oracle_price_fp48,
                        args.collateral_oracle_price_fp48,
                        args.debt_liability_weight_init_fp48,
                        args.collateral_asset_weight_init_fp48,
                        args.ltv_buffer_bps,
                        args.debt_mint_decimals,
                        args.collateral_mint_decimals,
                    )?;
                require!(
                    matched_collateral_for_gate >= required_collateral,
                    YdeltaError::CollateralBelowMatchLTV,
                    "convert refinance: cross collateral {} < required {} \
                     at oracle prices",
                    matched_collateral_for_gate,
                    required_collateral
                )?;

                // Stored cap only — marginfi-auto sentinel removed (v1 D17).
                if profile_max_ltv_bps > 0 {
                    let collateral_asset_weight_fp48 =
                        crate::math::to_scaled(profile_max_ltv_bps as u128)?
                            / 10_000u128;
                    let required_at_profile_cap =
                        crate::state::ltv::get_required_quote_collateral_to_back_debt(
                            matched_principal,
                            args.debt_oracle_price_fp48,
                            args.collateral_oracle_price_fp48,
                            crate::math::Fp48::ONE,
                            crate::math::Fp48::from_raw(collateral_asset_weight_fp48),
                            0,
                            args.debt_mint_decimals,
                            args.collateral_mint_decimals,
                        )?;
                    if matched_collateral_for_gate < required_at_profile_cap {
                        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                        continue;
                    }
                }
            }

            {
                let mut vault_data = vault_ai_ref.try_borrow_mut_data()?;
                let (fixed_bytes, vault_dyn) = vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
                let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
                let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
                let tree =
                    RiskProfileTreeReadOnly::new(vault_dyn, header.risk_profiles_root_index, NIL);
                let idx = tree.lookup_index(&probe);
                if idx != NIL {
                    let p = get_mut_helper_risk_profile(vault_dyn, idx).get_mut_value();
                    p.encumbered_in_orders_atoms = p
                        .encumbered_in_orders_atoms
                        .checked_add(matched_principal)
                        .ok_or(ProgramError::ArithmeticOverflow)?;
                }
            }
        }

        let matched_collateral: u64 = crate::math::mul_div_u64(
            args.loan_collateral_atoms,
            matched_principal,
            args.principal_cap_atoms,
            false,
        )?;

        let maker_snapshot = maker.share_price_snapshot();

        let lender_rate = maker.rate_bps;

        let floored_lender_rate: u32 = (maker.rate_bps as u32) + (args.fee_floor_bps as u32);
        require!(
            floored_lender_rate <= u16::MAX as u32,
            YdeltaError::InvalidArgument,
            "ask_rate {} + fee_floor {} exceeds u16::MAX — borrower rate \
             would clamp and under-collect the protocol floor",
            maker.rate_bps,
            args.fee_floor_bps
        )?;
        let borrower_rate = args.max_acceptable_rate_bps.max(floored_lender_rate as u16);

        let sequence = fixed.matched_loan_sequence;
        let mut node: MatchedLoan = Default::default();
        node.sequence = sequence;
        node.principal_atoms = matched_principal;
        node.origination_atoms = 0;
        node.collateral_atoms = matched_collateral;
        node.matched_at_unix = args.now_unix_ts;
        node.lender_seat_index = maker.trader_seat_index;
        node.borrower_seat_index = args.borrower_seat_index;
        node.term_seconds = args.term_remaining_seconds;
        node.borrower_rate_bps = borrower_rate;
        node.lender_rate_bps = lender_rate;
        node.loan_type = 0;

        node.flags = crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED
            | crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER;
        node.curator_fee_bps_snapshot = fixed.fee_config.curator_fee_bps;
        node.lender_debt_share_price_snapshot_fp48 = maker_snapshot;
        node.borrower_collateral_share_price_snapshot_fp48 =
            args.borrower_collateral_share_price_snapshot_fp48;

        let node_index = get_free_address_on_market_fixed_for_matched_loan(fixed, dynamic);
        require!(
            is_not_nil!(node_index),
            ProgramError::AccountDataTooSmall,
            "No free block for refinance MatchedLoan — expand market"
        )?;
        let mut matched_tree = MatchedLoanTree::new(dynamic, fixed.matched_loans_root_index, NIL);
        matched_tree.insert(node_index, node);
        fixed.matched_loans_root_index = matched_tree.get_root_index();
        drop(matched_tree);

        emit_stack(MatchedLoanCreatedLog {
            market: args.market_pubkey,
            loan_pda: Pubkey::default(),
            sequence,
            lender_seat_index: maker.trader_seat_index,
            borrower_seat_index: args.borrower_seat_index,
            principal_atoms: matched_principal,
            collateral_atoms: matched_collateral,
            borrower_rate_bps: borrower_rate,
            lender_rate_bps: lender_rate,
            term_seconds: args.term_remaining_seconds,
            matched_at_unix: args.now_unix_ts,
            loan_type: 0,
            flags: crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED,
            _padding: [0; 6],
        })?;

        fixed.matched_loan_sequence = fixed
            .matched_loan_sequence
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // Same open-lend stamp as match_order: each refinance cross becomes
        // a Fixed loan whose close-out decrements the lender seat's counter.
        {
            let seat = get_mut_helper_seat(dynamic, maker.trader_seat_index).get_mut_value();
            seat.open_lend_count = seat
                .open_lend_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        crosses.push(P2PoolRefinanceCross {
            lender_profile_id,
            lender_rate_bps: lender_rate,
            filled_principal_atoms: matched_principal,
        });

        remaining_principal = remaining_principal
            .checked_sub(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        total_filled_principal = total_filled_principal
            .checked_add(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        total_filled_collateral = total_filled_collateral
            .checked_add(matched_collateral)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        num_fills = num_fills.saturating_add(1);

        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
    }

    Ok(MatchP2PoolRefinanceResult {
        total_filled_principal_atoms: total_filled_principal,
        total_filled_collateral_atoms: total_filled_collateral,
        num_fills,
        crosses,
    })
}
