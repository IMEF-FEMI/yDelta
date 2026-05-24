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

pub const MARGINFI_ROUNDING_RESERVE_ATOMS: u64 = 1;

pub fn get_free_address_on_market_fixed(fixed: &mut MarketFixed, dynamic: &mut [u8]) -> DataIndex {
    let mut free_list: FreeList<MarketUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.free_list_head_index = free_list.get_head();
    free_address
}

pub fn get_free_address_on_market_fixed_for_seat(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

pub fn get_free_address_on_market_fixed_for_ask_order(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

pub fn get_free_address_on_market_fixed_for_matched_loan(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    get_free_address_on_market_fixed(fixed, dynamic)
}

pub fn release_address_on_market_fixed(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<MarketUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    free_list.add(index);
    // M-15: read head back from the freelist rather than assuming `index`
    // became the new head. Coincidentally correct today; brittle to a
    // future FreeList::add refactor.
    fixed.free_list_head_index = free_list.get_head();
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceAxis {
    Debt,
    Collateral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceBucket {
    Withdrawable,
    Encumbered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceSign {
    Plus,
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

pub const PLACEHOLDER_SHARE_PRICE_FP48: u128 = 1u128 << 48;

pub fn atoms_to_shares_at_snapshot(
    atoms: u64,
    snapshot_fp48: u128,
) -> Result<u128, ProgramError> {
    if snapshot_fp48 == 0 {
        return Err(crate::program::YdeltaError::MathDivisionByZero.into());
    }
    let atoms_fp48 = crate::math::to_scaled(atoms as u128)?;
    crate::math::div_scale(atoms_fp48, snapshot_fp48)
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
            // M-16: checked_sub — drift surfaces as ArithmeticOverflow
            // rather than silently saturating past zero.
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
///
/// H-5: pre-fix used `total.min(encumbered)` which papered over the
/// corruption. Per the closed-program memory there's no live state, so
/// fail-closed is safe and surfaces the bug rather than hiding it.
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

pub fn get_seat_index_with_hint(
    fixed: &MarketFixed,
    dynamic: &[u8],
    signer: &Pubkey,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    if let Some(idx) = hint {
        if is_not_nil!(idx) {
            let seat: &ClaimedSeat = get_helper_seat(dynamic, idx).get_value();
            // M-14: enforce owner_kind on the hint path. The fallback
            // tree lookup builds a USER probe, but the hint path skipped
            // that check — a wrong hint could route to a risk-profile
            // seat in a future code path.
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

#[derive(Clone, Copy)]
pub struct MatchArgs {
    pub market_pubkey: Pubkey,
    pub taker_seat_index: DataIndex,

    pub side: Side,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub order_type: OrderType,
    pub now_unix_ts: i64,
    pub fee_floor_bps: u16,

    pub taker_share_price_snapshot_fp48: u128,

    pub debt_oracle_price_fp48: u128,

    pub collateral_oracle_price_fp48: u128,

    pub debt_liability_weight_init_fp48: u128,

    pub collateral_asset_weight_init_fp48: u128,

    pub enforce_ltv: bool,
}

#[derive(Default, Clone)]
pub struct MatchResult {
    pub remaining_principal: u64,
    pub remaining_collateral: u64,
    pub total_filled_principal: u64,
    pub num_fills: u32,

    pub residual_action: ResidualAction,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResidualAction {
    #[default]
    Drop,

    P2PoolBorrow,
}

pub const FLAG_OB_ONLY: u8 = 0b0000_0010;

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

        let mut profile_max_ltv_bps: u16 = 0;
        let mut profile_max_term_seconds: u32 = 0;
        {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            // M-5: hard-runtime invariant. debug_assert_eq! was compiled
            // out in release. Only risk-profile vault asks rest on the
            // orderbook; anything else is corruption.
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
                    profile_max_ltv_bps = p.max_ltv_bps;
                    profile_max_term_seconds = p.max_term_seconds;
                    p.total_principal_atoms
                        .saturating_sub(p.deployed_principal_atoms)
                        .saturating_sub(p.encumbered_in_orders_atoms)
                        .saturating_sub(MARGINFI_ROUNDING_RESERVE_ATOMS)
                }
            };
            if profile_idle == 0 {
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            // M-11: re-validate the maker's resting-order term against
            // the profile's CURRENT max_term_seconds. If the curator
            // lowered the cap after the ask was placed, the stale ask
            // is no longer matchable — skip the maker instead of
            // honoring the over-term ask.
            if profile_max_term_seconds > 0 && maker.term_seconds > profile_max_term_seconds {
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            matched_principal = remaining_principal.min(profile_idle);

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

            if profile_max_ltv_bps > 0 {
                let collateral_asset_weight_fp48 =
                    crate::math::to_scaled(profile_max_ltv_bps as u128)? / 10_000u128;
                let required_at_profile_cap =
                    crate::state::ltv::get_required_quote_collateral_to_back_debt(
                        matched_principal,
                        args.debt_oracle_price_fp48,
                        args.collateral_oracle_price_fp48,
                        crate::math::SCALE,
                        collateral_asset_weight_fp48,
                        0,
                        fixed.debt_mint_decimals,
                        fixed.collateral_mint_decimals,
                    )?;
                require!(
                    total_collateral_for_match >= required_at_profile_cap,
                    YdeltaError::CollateralBelowMatchLTV,
                    "matched collateral {} < required {} at profile LTV cap {} bps",
                    total_collateral_for_match,
                    required_at_profile_cap,
                    profile_max_ltv_bps
                )?;
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
        // H-1: snapshot curator_fee_bps at MATCH time, not promotion time.
        // Lender capital is encumbered the moment this node is inserted;
        // promoting against a later `fee_config.curator_fee_bps` would let
        // a compromised admin retroactively reroute already-committed yield.
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
    pub fn claim_seat(&mut self, owner: &Pubkey, owner_kind: u8) -> ProgramResult {
        self.claim_seat_with_profile(owner, owner_kind, 0)
    }

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
        // H-9: checked_add — tree/counter desync surfaces as
        // ArithmeticOverflow rather than silently saturating.
        fixed.position_count = fixed
            .position_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }

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

    pub fn withdrawable_shares_for_seat(&self, seat_index: DataIndex, is_debt: bool) -> u128 {
        let MarketRefMut { dynamic, .. } = self;
        let seat = get_helper_seat(dynamic, seat_index).get_value();
        if is_debt {
            seat.debt_withdrawable_shares
        } else {
            seat.collateral_withdrawable_shares
        }
    }

    pub fn rest_order(&mut self, order_index: DataIndex, order: RestingOrder) -> ProgramResult {
        let MarketRefMut { fixed, dynamic } = self;
        // M-5: quote-only model rests only asks on the book.
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

#[derive(Clone, Copy)]
pub struct PlaceOrderArgs {
    pub market_pubkey: Pubkey,
    pub taker_seat_index: DataIndex,

    pub side: Side,

    pub order_type: OrderType,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub flags: u8,
    pub now_unix_ts: i64,

    pub share_price_snapshot_fp48: u128,

    pub debt_oracle_price_fp48: u128,
    pub collateral_oracle_price_fp48: u128,
    pub debt_liability_weight_init_fp48: u128,
    pub collateral_asset_weight_init_fp48: u128,

    pub enforce_ltv: bool,
}

#[derive(Clone, Copy)]
pub struct RestVaultAskArgs {
    pub market_pubkey: Pubkey,

    pub maker_seat_index: DataIndex,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub flags: u8,
    pub now_unix_ts: i64,
}

#[derive(Clone)]
pub struct PlaceOrderResult {
    pub sequence: u64,
    pub match_result: MatchResult,

    pub p2pool_loan_index: DataIndex,

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
        0,
    );
    let mut market = MarketRefMut { fixed, dynamic };
    market.rest_order(order_index, resting)?;
    Ok(seq)
}

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
        // M-16: checked_sub on the risk-profile lender path too.
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

pub struct MatchP2PoolRefinanceArgs {
    pub market_pubkey: Pubkey,

    pub borrower_seat_index: DataIndex,

    pub principal_cap_atoms: u64,

    pub loan_collateral_atoms: u64,

    pub borrower_collateral_share_price_snapshot_fp48: u128,

    pub term_remaining_seconds: u32,

    pub max_acceptable_rate_bps: u16,

    pub fee_floor_bps: u16,
    pub now_unix_ts: i64,

    pub debt_oracle_price_fp48: u128,

    pub collateral_oracle_price_fp48: u128,

    pub debt_liability_weight_init_fp48: u128,

    pub collateral_asset_weight_init_fp48: u128,

    pub ltv_buffer_bps: u16,

    pub debt_mint_decimals: u8,

    pub collateral_mint_decimals: u8,
}

#[derive(Clone, Copy)]
pub struct P2PoolRefinanceCross {
    pub lender_profile_id: u8,

    pub lender_rate_bps: u16,

    pub filled_principal_atoms: u64,
}

#[derive(Default)]
pub struct MatchP2PoolRefinanceResult {
    pub total_filled_principal_atoms: u64,

    pub total_filled_collateral_atoms: u64,
    pub num_fills: u32,

    pub crosses: Vec<P2PoolRefinanceCross>,
}

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
            // M-5: hard-runtime invariant. debug_assert_eq! was compiled
            // out in release. Only risk-profile vault asks rest on the
            // orderbook; anything else is corruption.
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
                    profile_max_ltv_bps = p.max_ltv_bps;
                    p.total_principal_atoms
                        .saturating_sub(p.deployed_principal_atoms)
                        .saturating_sub(p.encumbered_in_orders_atoms)
                        .saturating_sub(MARGINFI_ROUNDING_RESERVE_ATOMS)
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

                if profile_max_ltv_bps > 0 {
                    let collateral_asset_weight_fp48 =
                        crate::math::to_scaled(profile_max_ltv_bps as u128)? / 10_000u128;
                    let required_at_profile_cap =
                        crate::state::ltv::get_required_quote_collateral_to_back_debt(
                            matched_principal,
                            args.debt_oracle_price_fp48,
                            args.collateral_oracle_price_fp48,
                            crate::math::SCALE,
                            collateral_asset_weight_fp48,
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
        // H-1: refinance crosses always have a vault lender — snapshot
        // curator_fee_bps at match time so admin can't front-run yield
        // routing between match and promotion.
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
