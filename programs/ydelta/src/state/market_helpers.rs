use hypertree::{
    is_not_nil, DataIndex, FreeList, HyperTreeReadOperations, HyperTreeValueIteratorTrait,
    HyperTreeWriteOperations, RedBlackTreeReadOnly, NIL,
};
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};

use super::claimed_seat::{ClaimedSeat, OWNER_KIND_USER};
use super::market::{
    get_helper_order, get_helper_seat, get_mut_helper_order, get_mut_helper_seat, Bookside,
    ClaimedSeatTree, MarketFixed, MarketRefMut, MarketUnusedFreeListPadding, MatchedLoan,
    MatchedLoanTree, MATCHED_LOAN_FLAG_SECONDARY, MATCHED_LOAN_FLAG_SECONDARY_SPLIT,
};
use super::resting_order::{order_type_can_take, OrderKind, OrderType, RestingOrder, Side};
use super::utils::{assert_can_take, assert_not_already_expired};
use crate::logs::{emit_stack, MatchedLoanCreatedLog, OrderExpiredLog, OrderPlacedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_helper_risk_profile, get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;

// ────────────────────── Free-list helpers ──────────────────────

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

pub fn get_free_address_on_market_fixed_for_bid_order(
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
    fixed.free_list_head_index = index;
}

/// Append one fresh `MARKET_BLOCK_SIZE` block to the market's dynamic
/// region's free list. Caller must have already realloc'd the underlying
/// account by the same number of bytes.
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

// ───────────────── Seat balance bookkeeping ─────────────────
//
// Every seat-balance write goes through `update_balance`. Composite helpers
// (`encumber_for_order`, `unencumber_for_order`, `decrement_encumbrance_on_match`,
// `decrement_open_count`) build on it.

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

/// fp48 share-price representing 1.0 — placeholder used where the real
/// bank read isn't wired through. At 1.0 the share/atom conversion is
/// identity (atom-as-share semantics); the snapshot framework still
/// rides through the encumber/cancel/match path so the symmetry
/// plumbing is exercised.
pub const PLACEHOLDER_SHARE_PRICE_FP48: u128 = 1u128 << 48;

/// Convert an atom count to fp48 share count at a given share-price
/// snapshot. `shares = atoms / share_price` in fp48 math; equivalently
/// `shares = atoms * SCALE / snapshot` where `SCALE = 2^48`. Saturates
/// to `u128::MAX` on overflow (caller-rejected at the require-level
/// check in `update_balance`).
///
/// Returns 0 when `snapshot_fp48` is 0 (sentinel used by the vault
/// path; see `place_order_for_risk_profile`).
pub fn atoms_to_shares_at_snapshot(atoms: u64, snapshot_fp48: u128) -> u128 {
    if snapshot_fp48 == 0 {
        return 0;
    }
    // (atoms << 48) / snapshot.  atoms is u64 ≤ 2^64; (atoms << 48) ≤ 2^112,
    // fits in u128 with headroom.
    ((atoms as u128) << 48) / snapshot_fp48
}

/// Move funds `withdrawable → encumbered` for an order being placed.
/// Bid encumbers collateral; ask encumbers debt. Quantities in fp48
/// shares — caller computed via `atoms_to_shares_at_snapshot` against
/// the side-relevant bank's `asset_share_value`.
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
            seat.open_borrow_count = seat
                .open_borrow_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
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

/// Reverse `encumber_for_order` on cancel: move `encumbered → withdrawable`
/// and decrement the open-counter. Quantities are fp48 shares — caller
/// computed at the same snapshot recorded on the resting order.
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
            seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
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
            seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
        }
    }
    Ok(())
}

/// Match settlement: decrement encumbrance only (atoms re-credited via
/// `MatchedLoan` → `Loan` flow). Quantities are fp48 shares converted
/// at the resting order's snapshot share-price.
fn decrement_encumbrance_on_match(
    dynamic: &mut [u8],
    seat_index: DataIndex,
    side: Side,
    principal_shares: u128,
    collateral_shares: u128,
) -> ProgramResult {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    match side {
        Side::Bid => update_balance(
            seat,
            BalanceAxis::Collateral,
            BalanceBucket::Encumbered,
            BalanceSign::Minus,
            collateral_shares,
        ),
        Side::Ask => update_balance(
            seat,
            BalanceAxis::Debt,
            BalanceBucket::Encumbered,
            BalanceSign::Minus,
            principal_shares,
        ),
    }
}

/// Walk the bids tree and remove every `SecondaryLoanSale` resting bid
/// that references `loan_pda`. Returns the number of bids swept.
/// Called by `process_repay` on full repay (the loan is settled; any
/// secondary bid for it is stale).
///
/// Cheap: secondary bids are rare; tree walk is bounded by the bids
/// tree size. Defense in depth alongside the cranker's own staleness
/// check.
pub fn sweep_stale_secondary_bids_for_loan(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    loan_pda: &Pubkey,
) -> Result<u32, ProgramError> {
    // Collect indices first (avoid mutating the tree mid-iter).
    let mut victims: Vec<DataIndex> = Vec::new();
    {
        let tree = RedBlackTreeReadOnly::<RestingOrder>::new(
            dynamic,
            fixed.bids_root_index,
            fixed.bids_best_index,
        );
        for (idx, order) in tree.iter::<RestingOrder>() {
            if order.kind == OrderKind::SecondaryLoanSale && order.loan_pda == *loan_pda {
                victims.push(idx);
            }
        }
    }
    let count = victims.len() as u32;
    for idx in victims {
        // Secondary bids never encumbered anything, so we just remove
        // from tree + free slot. No seat balance to touch.
        remove_order_from_tree_and_free(fixed, dynamic, idx, Side::Bid);
    }
    Ok(count)
}

/// Debit a seat's `debt_encumbered_shares` by `shares`. Used by the
/// cranker on secondary cross finalization (the new lender's
/// encumbered cash, locked at primary-ask placement, is released when
/// the cranker settles the transfer or refunds the stale node).
/// Caller follows up with a `deposit_to_seat(... is_debt)` to credit
/// the destination seat (seller for transfer, same seat for stale-bid
/// refund).
pub fn seat_debit_encumbered_debt(
    dynamic: &mut [u8],
    seat_index: DataIndex,
    shares: u128,
) -> ProgramResult {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    update_balance(
        seat,
        BalanceAxis::Debt,
        BalanceBucket::Encumbered,
        BalanceSign::Minus,
        shares,
    )
}

fn decrement_open_count(dynamic: &mut [u8], seat_index: DataIndex, side: Side) {
    let seat = get_mut_helper_seat(dynamic, seat_index).get_mut_value();
    match side {
        Side::Bid => seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1),
        Side::Ask => seat.open_lend_count = seat.open_lend_count.saturating_sub(1),
    }
}

// ─────────────────────── Seat lookup ───────────────────────

/// Resolve the signer's seat index, preferring the caller's hint when valid.
/// Returns `Err(NoSeatClaimed)` if the signer has no seat.
pub fn get_seat_index_with_hint(
    fixed: &MarketFixed,
    dynamic: &[u8],
    signer: &Pubkey,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    if let Some(idx) = hint {
        if is_not_nil!(idx) {
            let seat: &ClaimedSeat = get_helper_seat(dynamic, idx).get_value();
            if seat.owner == *signer && seat.risk_profile_id == 0 {
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

// ─────────────────────── Matching engine ───────────────────────

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
    /// fp48 share-price snapshot recorded at place-order time for the
    /// taker. The taker's encumber at place_order_inner used this same
    /// snapshot, so per-match decrement uses it too — keeping
    /// taker-side encumber/decrement byte-symmetric.
    pub taker_share_price_snapshot_fp48: u128,
    // ─── LTV-at-match inputs ───
    /// fp48 USD-per-token from the debt bank's oracle, snapshot at
    /// place-order time.
    pub debt_oracle_price_fp48: u128,
    /// fp48 USD-per-token from the collateral bank's oracle.
    pub collateral_oracle_price_fp48: u128,
    /// fp48 borrower-side weight from the debt bank's
    /// `liability_weight_init`.
    pub debt_liability_weight_init_fp48: u128,
    /// fp48 lender-side weight from the collateral bank's
    /// `asset_weight_init`.
    pub collateral_asset_weight_init_fp48: u128,
    /// Set to `true` when the caller has loaded the marginfi/oracle
    /// accounts and wants the per-match LTV check enforced.
    /// `update_order` sets this `false` because its account list
    /// doesn't carry oracles.
    pub enforce_ltv: bool,
    /// Borrower-side LTV cap (Bids only; resolved by caller). The
    /// matching loop walks past vault makers whose
    /// `ClaimedSeat.risk_profile_max_ltv_bps` is below this. 0 = gate is a
    /// no-op (Asks, or a wallet-only path).
    pub borrower_ltv_bps: u16,
}

#[derive(Default, Clone)]
pub struct MatchResult {
    pub remaining_principal: u64,
    pub remaining_collateral: u64,
    pub total_filled_principal: u64,
    pub num_fills: u32,
    /// Fate of any residual after the matching pass.
    /// `Rest` — Limit residual rests on the book.
    /// `Drop` — IOC residual or OB_ONLY-flagged Bid residual; the
    /// processor unencumbers and emits `OrderFilledIocLog`.
    /// `P2PoolBorrow` — Bid residual fires a `marginfi.borrow` CPI for
    /// the unfilled atoms; processor records a `MatchedLoan` with
    /// `loan_type = P2Pool` and the resulting
    /// `borrower_marginfi_borrow_shares`.
    pub residual_action: ResidualAction,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResidualAction {
    #[default]
    Rest,
    Drop,
    /// Bid's unfilled residual auto-borrows from the debt bank via
    /// `marginfi.borrow`. The borrower-side and lender-side marginfi
    /// accounts are kept separate per market to avoid the marginfi
    /// constraint that an account cannot simultaneously hold an asset
    /// and a liability on the same bank.
    P2PoolBorrow,
}

/// Bit position 1 in `RestingOrder.flags` / `PlaceOrderArgs.flags`. When
/// set on a `Bid`, the unfilled residual goes to `Drop` instead of
/// triggering the P2Pool fallback. Default OFF.
pub const FLAG_OB_ONLY: u8 = 0b0000_0010;

/// Walk the opposite-side tree from best, applying cross conditions
/// and settlement. Removes expired makers mid-sweep and continues.
///
/// Iteration shape:
/// - `current_maker_index` starts at `*_best_index` (= tree's `max_index`,
///   which under our `RestingOrder` Ord direction is the BEST resting
///   order).
/// - **Full match / expired removal**: re-read `*_best_index` after the
///   tree is mutated — hypertree's `remove_by_index` updates `max_index`
///   to the next-best node automatically.
/// - **Skip without removal** (term incompatibility): walk via
///   `get_next_lower_index` (in-order predecessor under our Ord
///   direction = next-best by rate / FIFO).
// `vault_ai` is the GlobalVault account (Some when a vault account was
// passed to the calling ix; None for wallet-only paths). When the
// matching loop hits a risk-profile maker, it reads the profile's idle
// pool inline and writes the new commitment (`encumbered_in_orders +=`,
// `seat.deployed_atoms +=`) under the same vault account borrow.
// Risk-profile orders that fail the gate (idle insufficient or
// `seat.deployed_atoms + matched > max_exposure_atoms`) are silently
// skipped — never removed.
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

    let mut current_maker_index: DataIndex = match args.side {
        Side::Bid => fixed.asks_best_index,
        Side::Ask => fixed.bids_best_index,
    };

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            let maker_owner_kind: u8 = {
                let seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
                seat.owner_kind
            };
            // Risk-profile orders are non-expiring (placed with
            // `last_valid_unix_ts = 0`); reaching this branch with a
            // vault-owned seat would mean a corrupted order. Walk past
            // defensively rather than removing — only the curator
            // removes risk-profile orders, via `cancel_order_for_risk_profile`.
            if maker_owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
                current_maker_index =
                    next_maker_index(fixed, dynamic, args.side, current_maker_index);
                continue;
            }
            // Wallet path: byte-exact symmetric unencumber at the
            // maker's place-time snapshot.
            let maker_snapshot = maker.share_price_snapshot();
            unencumber_for_order(
                dynamic,
                maker.trader_seat_index,
                maker.side,
                atoms_to_shares_at_snapshot(maker.principal_atoms, maker_snapshot),
                atoms_to_shares_at_snapshot(maker.collateral_atoms, maker_snapshot),
            )?;
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            emit_stack(OrderExpiredLog {
                market: args.market_pubkey,
                owner_seat_index: maker.trader_seat_index,
                side: maker.side as u8,
                _padding: [0; 3],
                sequence: maker.sequence_number,
            })?;
            current_maker_index = match args.side {
                Side::Bid => fixed.asks_best_index,
                Side::Ask => fixed.bids_best_index,
            };
            continue;
        }

        // Self-match prevention. A trader cannot match against their
        // own resting order — fail loudly so off-chain UIs surface the
        // conflict rather than silently washing the trader's seat
        // balances.
        require!(
            maker.trader_seat_index != args.taker_seat_index,
            YdeltaError::SelfMatchForbidden,
            "taker seat {} matches its own maker order at index {}",
            args.taker_seat_index,
            current_maker_index
        )?;

        // Cross condition.
        let (bid_rate, ask_rate, bid_term, ask_term) = match args.side {
            Side::Bid => (
                args.rate_bps,
                maker.rate_bps,
                args.term_seconds,
                maker.term_seconds,
            ),
            Side::Ask => (
                maker.rate_bps,
                args.rate_bps,
                maker.term_seconds,
                args.term_seconds,
            ),
        };
        let spread_bps = (bid_rate as i32) - (ask_rate as i32);
        if spread_bps < args.fee_floor_bps as i32 {
            // The opposite tree is rate-sorted in the taker's favour; once the
            // best maker fails the floor, no later maker satisfies it either.
            break;
        }
        if bid_term > ask_term {
            // Term mismatch — rate ordering is independent of term,
            // so a later (worse-rate) maker with a longer ask_term
            // may still cross. Walk to the in-order predecessor
            // (next-best under our Ord direction) and try again.
            current_maker_index = next_maker_index(fixed, dynamic, args.side, current_maker_index);
            continue;
        }

        // ──────── Borrower-LTV risk-tier gate ────────
        // When the taker is a Bid, walk past vault makers whose cached
        // `RiskProfile.max_ltv_bps` is below the borrower's declared
        // cap. This is intentional market structure: a borrower
        // declaring a loose risk band (e.g. 80% LTV) cannot match a
        // conservative vault profile (e.g. 60% LTV) — their risk
        // preferences don't agree. Wallet makers (no cached value) and
        // taker = Ask (gate bps = 0) skip this branch.
        if args.side == Side::Bid && args.borrower_ltv_bps > 0 {
            let maker_seat_value = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            if maker_seat_value.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE
                && maker_seat_value.risk_profile_max_ltv_bps > 0
                && args.borrower_ltv_bps > maker_seat_value.risk_profile_max_ltv_bps
            {
                current_maker_index =
                    next_maker_index(fixed, dynamic, args.side, current_maker_index);
                continue;
            }
        }

        if !order_type_can_take(args.order_type) {
            assert_can_take(args.order_type)?;
        }

        let matched_principal = remaining_principal.min(maker.principal_atoms);

        // ─────────── Match-time vault lock (H-3 inline gate) ───────────
        //
        // CRITICAL: when a borrower bid crosses a risk-profile ask, we
        // MUST bump `RiskProfile.encumbered_in_orders_atoms` and
        // `ClaimedSeat.deployed_atoms` synchronously here — before the
        // match-time tx returns. The cranker is a SEPARATE transaction
        // (often many slots later) that runs `do_vault_settle` to
        // physically migrate the principal from `vault.integration` to
        // `market.lender_integration`; until it does, this inline write
        // is the ONLY thing preventing a second bid from seeing the
        // same atoms as idle and double-spending the profile.
        //
        // Read profile state directly from the vault account, gate on
        // live idle + seat max_exposure, and (on accept) write the
        // bookkeeping bumps inline. Subsequent iterations of THIS
        // matching loop re-read the same profile and see the updated
        // state; subsequent transactions re-read on entry.
        //
        // Any future redesign that queues this bookkeeping (instead of
        // inlining) re-introduces the cranker-race window — two bids
        // in different txs would each see the full pre-match idle
        // balance and both succeed against the same atoms.
        //
        // OWNER_KIND_RISK_PROFILE denotes "a profile in the vault owns
        // the seat", not "the vault as a whole owns it" — so the live
        // state we care about lives on `RiskProfile`, not the vault
        // header.
        if args.side == Side::Bid {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            if lender_seat.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE {
                if let Some(vault_ai_ref) = vault_ai {
                    let profile_id = lender_seat.risk_profile_id;
                    // Read profile.idle from the vault. Read-only
                    // borrow scoped to this block so the mut-borrow
                    // below can re-acquire cleanly.
                    let profile_idle: u64 = {
                        let vault_data = vault_ai_ref.try_borrow_data()?;
                        let (fixed_bytes, vault_dyn) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
                        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
                        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
                        let tree = RiskProfileTreeReadOnly::new(
                            vault_dyn,
                            header.risk_profiles_root_index,
                            NIL,
                        );
                        let idx = tree.lookup_index(&probe);
                        if idx == NIL {
                            0
                        } else {
                            let p = get_helper_risk_profile(vault_dyn, idx).get_value();
                            p.total_principal_atoms
                                .saturating_sub(p.deployed_principal_atoms)
                                .saturating_sub(p.encumbered_in_orders_atoms)
                        }
                    };
                    // Seat-side max_exposure gate: live deployed
                    // (already includes any prior bumps from earlier
                    // iterations of this same loop because we wrote
                    // inline) plus this match's atoms must stay
                    // within the cap. `max_exposure_atoms` is a hard
                    // cap; `claim_seat_for_risk_profile` rejects 0 at
                    // admin time, so we never treat 0 as "unlimited".
                    let max_exposure = lender_seat.max_exposure_atoms();
                    let new_seat_deployed = lender_seat
                        .deployed_atoms()
                        .saturating_add(matched_principal);
                    if profile_idle < matched_principal || new_seat_deployed > max_exposure {
                        current_maker_index =
                            next_maker_index(fixed, dynamic, args.side, current_maker_index);
                        continue;
                    }
                    // Accept: bump profile.encumbered_in_orders_atoms
                    // and seat.deployed_atoms inline so subsequent
                    // matches see the locked state.
                    {
                        let mut vault_data = vault_ai_ref.try_borrow_mut_data()?;
                        let (fixed_bytes, vault_dyn) =
                            vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
                        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
                        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
                        let tree = RiskProfileTreeReadOnly::new(
                            vault_dyn,
                            header.risk_profiles_root_index,
                            NIL,
                        );
                        let idx = tree.lookup_index(&probe);
                        if idx != NIL {
                            let p = get_mut_helper_risk_profile(vault_dyn, idx).get_mut_value();
                            p.encumbered_in_orders_atoms = p
                                .encumbered_in_orders_atoms
                                .checked_add(matched_principal)
                                .ok_or(ProgramError::ArithmeticOverflow)?;
                        }
                    }
                    let seat_mut =
                        get_mut_helper_seat(dynamic, maker.trader_seat_index).get_mut_value();
                    seat_mut.set_deployed_atoms(new_seat_deployed);
                }
            }
        }

        // ──────── Secondary cross branch (Scenario A) ────────
        //
        // Taker = primary ask; maker = `SecondaryLoanSale` bid. The
        // cross transfers ownership of the referenced loan (whole or
        // part) to the new lender. No fresh `MatchedLoan(Fixed)` is
        // created; instead a `SECONDARY`-flagged queue node carries
        // the cross to the cranker for finalization (loan mutation,
        // optional split, accrued seizure per Option A).
        //
        // SCOPE: only Scenario A (primary ask × resting secondary bid)
        // is supported. Secondary bids always rest until a future
        // primary ask sweeps them.
        if maker.kind == OrderKind::SecondaryLoanSale {
            require!(
                args.side == Side::Ask,
                YdeltaError::InvalidArgument,
                "secondary maker reached with taker.side != Ask \
                 (only primary-ask × secondary-bid is supported)"
            )?;

            // Par exit: cash transferred to the seller equals the
            // matched chunk's principal value exactly. No
            // proportional-price math — sellers can't discount or
            // premium-price; their sole exit cost is the Option-A
            // accrued-interest seizure at cranker time. The legacy
            // `maker.asking_price_atoms` field is set to
            // `principal_atoms` at placement and is no longer read
            // here.
            let cash_paid = matched_principal;

            // Cash gate: the ask must have enough cash for the
            // matched chunk. If not, walk to the next bid.
            if remaining_principal < cash_paid {
                current_maker_index =
                    next_maker_index(fixed, dynamic, args.side, current_maker_index);
                continue;
            }

            // Encumbrance: reduce the new lender's debt encumbrance
            // by the cash they're paying for this chunk. Secondary
            // makers have no encumbrance (placed without locking
            // anything), so only the ask side moves.
            let taker_snapshot = args.taker_share_price_snapshot_fp48;
            decrement_encumbrance_on_match(
                dynamic,
                args.taker_seat_index,
                args.side,
                atoms_to_shares_at_snapshot(cash_paid, taker_snapshot),
                0,
            )?;

            // Reduce the maker's resting bid (full removal vs partial).
            let did_full_secondary_fill = matched_principal == maker.principal_atoms;
            if did_full_secondary_fill {
                remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
                // Secondary makers don't bump open_borrow / open_lend
                // counts at placement (they neither borrow nor lend
                // fresh atoms); nothing to decrement here.
            } else {
                let order_mut = get_mut_helper_order(dynamic, current_maker_index).get_mut_value();
                order_mut.principal_atoms = order_mut
                    .principal_atoms
                    .checked_sub(matched_principal)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
                // Par-exit invariant: asking_price stays equal to
                // remaining principal. (Field kept for layout
                // stability; consumers should treat as deprecated.)
                order_mut.asking_price_atoms = order_mut.principal_atoms;
            }

            // Insert SECONDARY-flagged MatchedLoan queue node.
            let sequence = fixed.matched_loan_sequence;
            let mut node: MatchedLoan = Default::default();
            node.sequence = sequence;
            node.principal_atoms = matched_principal;
            node.cash_paid_atoms = cash_paid;
            node.referenced_loan_sequence = maker.loan_sequence_snapshot as u64;
            node.matched_at_unix = args.now_unix_ts;
            node.lender_seat_index = maker.trader_seat_index; // OLD lender (seller)
            node.new_lender_seat_index = args.taker_seat_index; // NEW lender (ask)
            node.lender_rate_bps = ask_rate; // new rate, cranker stamps to loan
            node.borrower_rate_bps = bid_rate; // = loan.borrower_rate_bps snapshot
            node.term_seconds = bid_term;
            node.loan_type = 0; // Fixed
            node.flags = MATCHED_LOAN_FLAG_SECONDARY
                | (if did_full_secondary_fill {
                    0
                } else {
                    MATCHED_LOAN_FLAG_SECONDARY_SPLIT
                });
            // New lender's debt snapshot — the ask taker just sampled
            // the debt bank in their own place_order. Cranker stamps
            // this onto the loan body so the new lender's
            // claim_repayment decrement is byte-symmetric with their
            // place-time encumber. Borrower-side snapshot is left at 0
            // (cranker copies the original from the existing loan).
            node.lender_debt_share_price_snapshot_fp48 = taker_snapshot;

            let node_index = get_free_address_on_market_fixed_for_matched_loan(fixed, dynamic);
            require!(
                is_not_nil!(node_index),
                ProgramError::AccountDataTooSmall,
                "No free block for secondary MatchedLoan — expand market"
            )?;
            let mut matched_tree =
                MatchedLoanTree::new(dynamic, fixed.matched_loans_root_index, NIL);
            matched_tree.insert(node_index, node);
            fixed.matched_loans_root_index = matched_tree.get_root_index();
            drop(matched_tree);

            emit_stack(MatchedLoanCreatedLog {
                market: args.market_pubkey,
                loan_pda: maker.loan_pda,
                sequence,
                lender_seat_index: maker.trader_seat_index,
                borrower_seat_index: NIL, // cranker reads from loan PDA
                principal_atoms: matched_principal,
                collateral_atoms: 0,
                borrower_rate_bps: bid_rate,
                lender_rate_bps: ask_rate,
                term_seconds: bid_term,
                matched_at_unix: args.now_unix_ts,
                loan_type: 0,
                flags: node.flags,
                _padding: [0; 6],
            })?;
            fixed.matched_loan_sequence = fixed.matched_loan_sequence.wrapping_add(1);

            // Reduce taker (ask) capacity by cash paid — its
            // remaining_principal is denominated in cash.
            remaining_principal = remaining_principal
                .checked_sub(cash_paid)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            total_filled = total_filled
                .checked_add(cash_paid)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            num_fills = num_fills.saturating_add(1);

            // Walk: removed maker → re-read max; partial fill →
            // walk to next-lower (skip the now-reduced same bid).
            current_maker_index = if did_full_secondary_fill {
                match args.side {
                    Side::Bid => fixed.asks_best_index,
                    Side::Ask => fixed.bids_best_index,
                }
            } else {
                next_maker_index(fixed, dynamic, args.side, current_maker_index)
            };
            continue;
        }
        // ──────── End secondary cross branch ────────

        let (matched_collateral_taker, matched_collateral_maker) = match args.side {
            Side::Bid => (
                mul_div_u64(
                    matched_principal,
                    args.collateral_atoms,
                    args.principal_atoms,
                )?,
                0u64,
            ),
            Side::Ask => (
                0u64,
                mul_div_u64(
                    matched_principal,
                    maker.collateral_atoms,
                    maker.principal_atoms,
                )?,
            ),
        };
        let total_collateral_for_match = matched_collateral_taker
            .checked_add(matched_collateral_maker)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // LTV check. Refuse the match if the bidder (whichever side
        // is the borrower) hasn't posted enough collateral to cover
        // `matched_principal` at the current oracle prices, scaled by
        // the debt bank's `liability_weight_init` and the collateral
        // bank's `asset_weight_init`, plus
        // `fee_config.ltv_buffer_bps`. `update_order` skips the check
        // (lighter context, no oracles loaded).
        if args.enforce_ltv {
            let required_collateral =
                crate::state::ltv::get_required_quote_collateral_to_back_debt(
                    matched_principal,
                    args.debt_oracle_price_fp48,
                    args.collateral_oracle_price_fp48,
                    args.debt_liability_weight_init_fp48,
                    args.collateral_asset_weight_init_fp48,
                    fixed.fee_config.ltv_buffer_bps,
                )?;
            require!(
                total_collateral_for_match >= required_collateral,
                YdeltaError::CollateralBelowMatchLTV,
                "matched collateral {} < required {} at oracle prices",
                total_collateral_for_match,
                required_collateral
            )?;
        }

        // Detect vault-owned maker. Vault profile orders are
        // open-ended (backed by the vault's profile-level idle pool,
        // not by per-seat shares), so we skip the maker-side seat
        // encumbrance.
        let maker_seat_value = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
        let maker_is_vault = args.side == Side::Bid
            && maker_seat_value.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE;

        // Taker decrement uses the taker's snapshot (carried in
        // MatchArgs from place_order_inner). Maker decrement uses the
        // snapshot recorded on the maker's resting order. Both
        // pre-recorded at place-time so each side's decrement matches
        // the original encumber atom-for-atom in fp48 share units.
        let taker_snapshot = args.taker_share_price_snapshot_fp48;
        let maker_snapshot = maker.share_price_snapshot();
        decrement_encumbrance_on_match(
            dynamic,
            args.taker_seat_index,
            args.side,
            atoms_to_shares_at_snapshot(matched_principal, taker_snapshot),
            atoms_to_shares_at_snapshot(matched_collateral_taker, taker_snapshot),
        )?;
        if !maker_is_vault {
            decrement_encumbrance_on_match(
                dynamic,
                maker.trader_seat_index,
                maker.side,
                atoms_to_shares_at_snapshot(matched_principal, maker_snapshot),
                atoms_to_shares_at_snapshot(matched_collateral_maker, maker_snapshot),
            )?;
        }
        // Vault makers: profile.encumbered_in_orders and
        // seat.deployed_atoms are already written inline at the gate
        // above, so nothing to do here.

        let did_full_fill = matched_principal == maker.principal_atoms;
        if maker_is_vault {
            // Risk-profile orders are non-expiring and only the curator
            // may remove them via cancel_order_for_risk_profile. The
            // order's `principal_atoms` represents the per-market
            // exposure cap, not a depleting depth — the gate
            // (idle_principal_atoms + seat.deployed_atoms vs
            // max_exposure_atoms) governs match admission. Leave the
            // resting order intact; the seat's `deployed_atoms` bump
            // reflects the new commitment.
        } else if did_full_fill {
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            decrement_open_count(dynamic, maker.trader_seat_index, maker.side);
        } else {
            let order_mut = get_mut_helper_order(dynamic, current_maker_index).get_mut_value();
            order_mut.principal_atoms = order_mut
                .principal_atoms
                .checked_sub(matched_principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            if maker.side == Side::Bid {
                order_mut.collateral_atoms = order_mut
                    .collateral_atoms
                    .checked_sub(matched_collateral_maker)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }

        let (lender_seat_index, borrower_seat_index, borrower_rate, lender_rate, term) =
            match args.side {
                Side::Bid => (
                    maker.trader_seat_index,
                    args.taker_seat_index,
                    bid_rate,
                    ask_rate,
                    bid_term,
                ),
                Side::Ask => (
                    args.taker_seat_index,
                    maker.trader_seat_index,
                    bid_rate,
                    ask_rate,
                    bid_term,
                ),
            };
        // ─── Insert a MatchedLoan tree node ───
        //
        // Per-match work stays seat-bookkeeping + tree-mutation only.
        // The P2Pool fallback CPI lands at end-of-pass, not per match.
        let total_collateral = total_collateral_for_match;
        let origination_atoms = (matched_principal as u128)
            .checked_mul(fixed.fee_config.origination_bps as u128)
            .map(|x| x / (crate::state::loan::BPS_PER_UNIT as u128))
            .and_then(|x| u64::try_from(x).ok())
            .unwrap_or(0);

        // Detect vault-owned maker on full fill so the cranker can
        // clean up the stale `RiskProfileOrderRef`. Read the maker's
        // `ClaimedSeat` once; the lender_seat_index is the vault when
        // args.side == Bid (taker borrows, maker lends). Mirrors
        // manifest's global-order awareness in matching, but without
        // JIT token movement (atoms are already on market.lender via
        // the commit-first flow).
        let maker_is_vault_lender = args.side == Side::Bid && {
            let maker_seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            maker_seat.owner_kind == crate::state::OWNER_KIND_RISK_PROFILE
        };
        let mut vault_flag: u8 = 0;
        if did_full_fill && maker_is_vault_lender {
            vault_flag |= crate::state::market::MATCHED_LOAN_FLAG_VAULT_MAKER_FULLY_FILLED;
        }

        // Snapshots for byte-symmetric encumber/release on the
        // promoted loan. The matching engine holds both:
        //   - taker_snapshot: side-relevant bank for the taker
        //   - maker_snapshot: side-relevant bank for the maker
        // Bid taker = borrower (encumbers collateral); maker = lender
        // (encumbered debt). Roles swap when taker is Ask. Stamp the
        // role-correct value onto each leg so the cranker can copy
        // them onto LoanFixed without re-reading bank state.
        let (lender_debt_snapshot, borrower_collateral_snapshot) = match args.side {
            Side::Bid => (maker_snapshot, taker_snapshot),
            Side::Ask => (taker_snapshot, maker_snapshot),
        };

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
        node.loan_type = 0; // LoanType::Fixed
        node.flags = vault_flag;
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
        fixed.matched_loan_sequence = fixed.matched_loan_sequence.wrapping_add(1);

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

        // After settlement: a partial fill exhausts the taker (the
        // resting maker still has remaining principal) so we let the
        // top-of-loop `remaining_principal > 0` check break us out. A
        // full fill removed the maker, so `*_best_index` now points at
        // the next-best maker (hypertree updated `max_index` during
        // `remove_by_index`); re-read it here.
        if did_full_fill {
            current_maker_index = match args.side {
                Side::Bid => fixed.asks_best_index,
                Side::Ask => fixed.bids_best_index,
            };
        } else {
            // Partial fill → taker exhausted. Loop guard exits.
            current_maker_index = NIL;
        }
    }

    Ok(MatchResult {
        remaining_principal,
        remaining_collateral,
        total_filled_principal: total_filled,
        num_fills,
        // place_order_inner overwrites this based on side / order_type
        // / OB_ONLY after the matching pass.
        residual_action: ResidualAction::Rest,
    })
}

/// In-order tree predecessor of `current` on the side opposite the
/// taker. Under our `RestingOrder` Ord direction the predecessor is the
/// next-best maker by rate (and FIFO at equal rates).
fn next_maker_index(
    fixed: &MarketFixed,
    dynamic: &[u8],
    taker_side: Side,
    current: DataIndex,
) -> DataIndex {
    let (root, best) = match taker_side {
        Side::Bid => (fixed.asks_root_index, fixed.asks_best_index),
        Side::Ask => (fixed.bids_root_index, fixed.bids_best_index),
    };
    let tree: super::market::BooksideReadOnly =
        super::market::BooksideReadOnly::new(dynamic, root, best);
    tree.get_next_lower_index::<RestingOrder>(current)
}

/// Remove a `RestingOrder` node from its bid/ask tree and return its slot
/// to the free list. Updates `root` and `best` pointers in `fixed`.
pub fn remove_order_from_tree_and_free(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    order_index: DataIndex,
    side: Side,
) {
    let (root, best) = match side {
        Side::Bid => (fixed.bids_root_index, fixed.bids_best_index),
        Side::Ask => (fixed.asks_root_index, fixed.asks_best_index),
    };
    let mut tree: Bookside = Bookside::new(dynamic, root, best);
    tree.remove_by_index(order_index);
    let new_root = tree.get_root_index();
    let new_best = tree.get_max_index();
    match side {
        Side::Bid => {
            fixed.bids_root_index = new_root;
            fixed.bids_best_index = new_best;
        }
        Side::Ask => {
            fixed.asks_root_index = new_root;
            fixed.asks_best_index = new_best;
        }
    }
    release_address_on_market_fixed(fixed, dynamic, order_index);
}

fn mul_div_u64(a: u64, b: u64, c: u64) -> Result<u64, ProgramError> {
    if c == 0 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    let prod: u128 = (a as u128)
        .checked_mul(b as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let q: u128 = prod / (c as u128);
    if q > u64::MAX as u128 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    Ok(q as u64)
}

// ─────────────────────── MarketRefMut methods ───────────────────────

impl<'a> MarketRefMut<'a> {
    /// Insert a new `ClaimedSeat` for `(owner, owner_kind, risk_profile_id=0)`.
    /// Errors with `AlreadyClaimedSeat` if one already exists.
    /// Wallet path (always uses `risk_profile_id = 0`).
    pub fn claim_seat(&mut self, owner: &Pubkey, owner_kind: u8) -> ProgramResult {
        self.claim_seat_with_profile(owner, owner_kind, 0)
    }

    /// Insert a new `ClaimedSeat` for the full
    /// `(owner, owner_kind, risk_profile_id)` tuple. Vault profiles
    /// claim seats with `owner = global_vault_pda`, `owner_kind = Vault`, and
    /// the profile's `profile_id`. Multiple profiles in the same vault
    /// produce distinct seats in this market.
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
        fixed.position_count = fixed.position_count.saturating_add(1);
        Ok(())
    }

    /// Credit `withdrawable` shares for the given side. `shares` is the
    /// adapter-share quantity returned by the underlying lending protocol's
    /// deposit CPI (u128 fp48).
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

    /// Debit `withdrawable` shares; fails with `InsufficientWithdrawableBalance`
    /// if the seat doesn't hold enough. Encumbered shares are NOT eligible.
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

    /// Insert a `RestingOrder` into the appropriate tree. Caller is
    /// responsible for having already encumbered the maker's seat.
    pub fn rest_order(&mut self, order_index: DataIndex, order: RestingOrder) -> ProgramResult {
        let MarketRefMut { fixed, dynamic } = self;
        let (root, best) = match order.side {
            Side::Bid => (fixed.bids_root_index, fixed.bids_best_index),
            Side::Ask => (fixed.asks_root_index, fixed.asks_best_index),
        };
        let mut tree: Bookside = Bookside::new(dynamic, root, best);
        tree.insert(order_index, order);
        let new_root = tree.get_root_index();
        let new_best = tree.get_max_index();
        match order.side {
            Side::Bid => {
                fixed.bids_root_index = new_root;
                fixed.bids_best_index = new_best;
            }
            Side::Ask => {
                fixed.asks_root_index = new_root;
                fixed.asks_best_index = new_best;
            }
        }
        Ok(())
    }
}

// ─────────────────────── Place-order orchestrator ───────────────────────

#[derive(Clone, Copy)]
pub struct PlaceOrderArgs {
    pub market_pubkey: Pubkey,
    pub taker_seat_index: DataIndex,
    pub side: Side,
    pub kind: OrderKind,
    pub order_type: OrderType,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub last_valid_unix_ts: i64,
    pub flags: u8,
    pub now_unix_ts: i64,
    /// fp48 share-price snapshot for the side-relevant bank at
    /// place-order time. The processor reads it from the bank header in the
    /// `PlaceOrderContext`; pre-Step-7 callers pass
    /// `PLACEHOLDER_SHARE_PRICE_FP48` (1.0) so the math is identity.
    pub share_price_snapshot_fp48: u128,
    // ─── Oracle prices + bank weights, snapshot at place-order time
    // and threaded into `match_order` for the LTV check ───
    pub debt_oracle_price_fp48: u128,
    pub collateral_oracle_price_fp48: u128,
    pub debt_liability_weight_init_fp48: u128,
    pub collateral_asset_weight_init_fp48: u128,
    /// True when the caller has the marginfi/oracle accounts in scope
    /// (i.e. went through `PlaceOrderContext`). `update_order` uses
    /// the lighter `OrderContext` and sets this `false`.
    pub enforce_ltv: bool,
    /// Set by `place_order_for_risk_profile` to mark the resting
    /// order as vault-backed (open-ended profile order). The inner
    /// `encumber_for_order` step is skipped because the vault's
    /// `ClaimedSeat` has no per-seat shares — the profile's
    /// `idle_principal_atoms` pool is the backing, gated and
    /// encumbered inline in the matching loop.
    pub is_vault_lender: bool,
    /// Borrower-side LTV cap (Bids only). Caller resolves the default
    /// (marginfi-init) before calling, so the matching loop sees a
    /// concrete value. The per-maker risk-tier gate skips vault makers
    /// whose seat-cached `max_ltv_bps()` is below this — `max_ltv_bps`
    /// is stamped on the market-side `ClaimedSeat` at
    /// `claim_seat_for_risk_profile` time and is immutable on `RiskProfile`,
    /// so the matching loop never has to read vault state. For Asks:
    /// pass 0 (gate is no-op).
    pub borrower_ltv_bps: u16,
}

#[derive(Clone)]
pub struct PlaceOrderResult {
    pub sequence: u64,
    pub match_result: MatchResult,
    pub rested_order_index: DataIndex,
    pub rested: bool,
    /// When the residual triggers a P2Pool borrow,
    /// `place_order_inner` inserts a `MatchedLoan` with `loan_type =
    /// P2Pool` and returns its node-index here. The processor uses
    /// this index after firing `marginfi.borrow` to patch the node's
    /// `borrower_marginfi_borrow_shares` with the liability-share
    /// delta read from the borrower's marginfi-account post-CPI.
    /// `NIL` means no P2Pool loan was created.
    pub p2pool_loan_index: DataIndex,
    /// Sequence of the P2Pool MatchedLoan, captured here so the
    /// processor can emit the post-CPI patch log without re-reading
    /// the node.
    pub p2pool_loan_sequence: u64,
}

impl Default for PlaceOrderResult {
    fn default() -> Self {
        Self {
            sequence: 0,
            match_result: MatchResult::default(),
            rested_order_index: NIL,
            rested: false,
            p2pool_loan_index: NIL,
            p2pool_loan_sequence: 0,
        }
    }
}

// `vault_ai`: the GlobalVault account when the caller passes one
// (Some); None for paths that don't touch vault liquidity (e.g.,
// update_order's snapshot-only re-place). Forwarded to `match_order`
// for in-loop lazy lookup of vault profile idle.
pub fn place_order_inner(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: PlaceOrderArgs,
    vault_ai: Option<&solana_program::account_info::AccountInfo<'_>>,
) -> Result<PlaceOrderResult, ProgramError> {
    assert_not_already_expired(args.last_valid_unix_ts, args.now_unix_ts)?;
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
    match args.side {
        Side::Bid => require!(
            args.collateral_atoms > 0,
            YdeltaError::CollateralInsufficient,
            "Bid requires collateral atoms"
        )?,
        Side::Ask => require!(
            args.collateral_atoms == 0,
            YdeltaError::CollateralInsufficient,
            "Ask must not carry collateral"
        )?,
    }

    if args.order_type == OrderType::PostOnly {
        // Walk ALL opposite-side makers, not just the rate-best one.
        // If we only check the best maker and it's term-incompatible
        // (bid_term > ask_term), the gate passes — but the matching
        // engine, if it ran, would walk past the term-incompatible
        // best to a deeper compatible maker. PostOnly orders skip
        // the engine, so the order rests; if a deeper maker would
        // have crossed, the resulting book is internally crossed at
        // non-best levels. Walking the full opposite tree applies
        // the same spread+term gate at every level, mirroring
        // match_order's traversal.
        let (root, best) = match args.side {
            Side::Bid => (fixed.asks_root_index, fixed.asks_best_index),
            Side::Ask => (fixed.bids_root_index, fixed.bids_best_index),
        };
        if is_not_nil!(best) {
            let tree = RedBlackTreeReadOnly::<RestingOrder>::new(dynamic, root, best);
            for (_idx, maker) in tree.iter::<RestingOrder>() {
                let (bid_rate, ask_rate, bid_term, ask_term) = match args.side {
                    Side::Bid => (
                        args.rate_bps,
                        maker.rate_bps,
                        args.term_seconds,
                        maker.term_seconds,
                    ),
                    Side::Ask => (
                        maker.rate_bps,
                        args.rate_bps,
                        maker.term_seconds,
                        args.term_seconds,
                    ),
                };
                let spread = (bid_rate as i32) - (ask_rate as i32);
                if spread >= fixed.fee_config.protocol_fee_bps_floor as i32 && bid_term <= ask_term
                {
                    return Err(YdeltaError::PostOnlyWouldCross.into());
                }
            }
        }
    }

    // Encumber/cancel/match all operate on fp48 shares computed from
    // `args.share_price_snapshot_fp48` (the side-relevant bank's
    // `asset_share_value` at place-order time).
    let snapshot = args.share_price_snapshot_fp48;
    let principal_shares = atoms_to_shares_at_snapshot(args.principal_atoms, snapshot);
    let collateral_shares = atoms_to_shares_at_snapshot(args.collateral_atoms, snapshot);
    if !args.is_vault_lender {
        encumber_for_order(
            dynamic,
            args.taker_seat_index,
            args.side,
            principal_shares,
            collateral_shares,
        )?;
    } else {
        // Open-ended vault profile order. No per-seat share-backing
        // is encumbered. The matching engine gates and encumbers
        // against `RiskProfile.idle_principal_atoms` inline at match
        // time. We still bump the open-lend counter so cancel/expire
        // paths' accounting stays balanced.
        let seat = get_mut_helper_seat(dynamic, args.taker_seat_index).get_mut_value();
        seat.open_lend_count = seat
            .open_lend_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    let seq = fixed.order_sequence_number;
    fixed.order_sequence_number = seq.wrapping_add(1);

    emit_stack(OrderPlacedLog {
        market: args.market_pubkey,
        trader: get_helper_seat(dynamic, args.taker_seat_index)
            .get_value()
            .owner,
        trader_seat_index: args.taker_seat_index,
        side: args.side as u8,
        kind: args.kind as u8,
        order_type: args.order_type as u8,
        _padding1: 0,
        rate_bps: args.rate_bps,
        _padding2: 0,
        term_seconds: args.term_seconds,
        principal_atoms: args.principal_atoms,
        collateral_atoms: args.collateral_atoms,
        sequence: seq,
        last_valid_unix_ts: args.last_valid_unix_ts,
    })?;

    let mut match_result = if order_type_can_take(args.order_type) {
        match_order(
            fixed,
            dynamic,
            MatchArgs {
                market_pubkey: args.market_pubkey,
                taker_seat_index: args.taker_seat_index,
                side: args.side,
                rate_bps: args.rate_bps,
                term_seconds: args.term_seconds,
                principal_atoms: args.principal_atoms,
                collateral_atoms: args.collateral_atoms,
                order_type: args.order_type,
                now_unix_ts: args.now_unix_ts,
                fee_floor_bps: fixed.fee_config.protocol_fee_bps_floor,
                taker_share_price_snapshot_fp48: snapshot,
                debt_oracle_price_fp48: args.debt_oracle_price_fp48,
                collateral_oracle_price_fp48: args.collateral_oracle_price_fp48,
                debt_liability_weight_init_fp48: args.debt_liability_weight_init_fp48,
                collateral_asset_weight_init_fp48: args.collateral_asset_weight_init_fp48,
                enforce_ltv: args.enforce_ltv,
                borrower_ltv_bps: args.borrower_ltv_bps,
            },
            vault_ai,
        )?
    } else {
        MatchResult {
            remaining_principal: args.principal_atoms,
            remaining_collateral: args.collateral_atoms,
            total_filled_principal: 0,
            num_fills: 0,
            residual_action: ResidualAction::Rest,
        }
    };

    let mut rested = false;
    let mut rested_order_index = NIL;

    // Route the residual based on side / order_type / OB_ONLY flag.
    // Bids with `OB_ONLY` unset and a residual after the matching
    // pass trigger the P2Pool fallback: the residual auto-borrows
    // from marginfi via the borrower-side marginfi-account.
    let can_rest = super::resting_order::order_type_can_rest(args.order_type);
    let ob_only = (args.flags & FLAG_OB_ONLY) != 0;
    let bid_p2pool_eligible =
        args.side == Side::Bid && !ob_only && match_result.remaining_principal > 0 && can_rest;

    if bid_p2pool_eligible {
        match_result.residual_action = ResidualAction::P2PoolBorrow;
    } else if match_result.remaining_principal > 0 {
        match_result.residual_action = if can_rest {
            ResidualAction::Rest
        } else {
            ResidualAction::Drop
        };
    }

    let mut p2pool_loan_index: DataIndex = NIL;
    let mut p2pool_loan_sequence: u64 = 0;

    match (
        match_result.residual_action,
        match_result.remaining_principal > 0,
    ) {
        (ResidualAction::P2PoolBorrow, true) => {
            // Convert the residual into a P2Pool MatchedLoan.
            // Decrement the encumbrance for the residual (it leaves
            // the open-order column for the loan column), insert a
            // MatchedLoan with `loan_type = P2Pool` and
            // `lender_seat_index = NIL` (marginfi is the funding side
            // — no on-book lender). `borrower_marginfi_borrow_shares`
            // is left at zero here; the processor patches it after
            // the `marginfi.borrow` CPI lands by reading the share
            // delta off the borrower marginfi-account.
            unencumber_for_order(
                dynamic,
                args.taker_seat_index,
                args.side,
                atoms_to_shares_at_snapshot(match_result.remaining_principal, snapshot),
                atoms_to_shares_at_snapshot(match_result.remaining_collateral, snapshot),
            )?;
            decrement_open_count(dynamic, args.taker_seat_index, args.side);

            let origination_atoms = (match_result.remaining_principal as u128)
                .checked_mul(fixed.fee_config.origination_bps as u128)
                .map(|x| x / (crate::state::loan::BPS_PER_UNIT as u128))
                .and_then(|x| u64::try_from(x).ok())
                .unwrap_or(0);

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
            node.loan_type = 1; // LoanType::P2Pool
            node.borrower_marginfi_borrow_shares = 0; // patched post-CPI
                                                      // P2Pool has no human lender; only the borrower's
                                                      // collateral encumbrance needs a release-time snapshot.
                                                      // `snapshot` is `args.share_price_snapshot_fp48` — for a
                                                      // Bid taker that's the collateral-bank read.
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
                flags: 0, // P2Pool: no SECONDARY / VAULT_MAKER bits
                _padding: [0; 6],
            })?;
            fixed.matched_loan_sequence = fixed.matched_loan_sequence.wrapping_add(1);

            p2pool_loan_index = node_index;
            p2pool_loan_sequence = sequence;
        }
        (ResidualAction::Rest, true) => {
            let order_index = match args.side {
                Side::Bid => get_free_address_on_market_fixed_for_bid_order(fixed, dynamic),
                Side::Ask => get_free_address_on_market_fixed_for_ask_order(fixed, dynamic),
            };
            require!(
                is_not_nil!(order_index),
                ProgramError::AccountDataTooSmall,
                "No free block for resting order — expand market"
            )?;
            let resting = RestingOrder::new_primary(
                args.taker_seat_index,
                seq,
                args.side,
                args.order_type,
                args.rate_bps,
                args.term_seconds,
                match_result.remaining_principal,
                match_result.remaining_collateral,
                args.last_valid_unix_ts,
                args.flags,
                snapshot,
                args.borrower_ltv_bps,
            );
            let mut market = MarketRefMut { fixed, dynamic };
            market.rest_order(order_index, resting)?;
            rested = true;
            rested_order_index = order_index;
        }
        (ResidualAction::Drop, true) => {
            // IOC remainder dropped: reverse the encumbrance for the
            // unmatched portion since no live order will hold it. Same
            // snapshot as the original encumber so the math cancels
            // exactly.
            unencumber_for_order(
                dynamic,
                args.taker_seat_index,
                args.side,
                atoms_to_shares_at_snapshot(match_result.remaining_principal, snapshot),
                atoms_to_shares_at_snapshot(match_result.remaining_collateral, snapshot),
            )?;
            decrement_open_count(dynamic, args.taker_seat_index, args.side);
        }
        (_, false) => {
            // Fully filled: the taker's order never rested, so remove
            // the open-counter bump done by `encumber_for_order`.
            decrement_open_count(dynamic, args.taker_seat_index, args.side);
        }
    }

    Ok(PlaceOrderResult {
        sequence: seq,
        match_result,
        rested_order_index,
        rested,
        p2pool_loan_index,
        p2pool_loan_sequence,
    })
}

// ─────────────────────── Cancel / lookup ───────────────────────

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

    for &(root, best) in &[
        (fixed.bids_root_index, fixed.bids_best_index),
        (fixed.asks_root_index, fixed.asks_best_index),
    ] {
        let tree = RedBlackTreeReadOnly::<RestingOrder>::new(dynamic, root, best);
        for (idx, order) in tree.iter::<RestingOrder>() {
            if order.trader_seat_index == trader_seat_index && order.sequence_number == sequence {
                return Ok(idx);
            }
        }
    }
    Err(YdeltaError::OrderNotFound.into())
}

/// Returns `Some(loan_pda)` iff the canceled order was a
/// `SecondaryLoanSale` bid — the caller MUST then clear that loan's
/// `has_resting_secondary_bid` flag (O(1) duplicate-check counterpart).
/// Returns `None` for primary orders.
pub fn cancel_order_by_index(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    signer_seat_index: DataIndex,
    order_index: DataIndex,
) -> Result<Option<Pubkey>, ProgramError> {
    let order: RestingOrder = *get_helper_order(dynamic, order_index).get_value();
    require!(
        order.trader_seat_index == signer_seat_index,
        YdeltaError::OrderNotOwnedBySigner,
        "Order owned by a different seat"
    )?;
    // Secondary bids never encumbered anything (their
    // collateral_atoms is 0 and they don't put up debt as a Bid would
    // for a fresh borrow). Skip the unencumber step entirely.
    if order.kind == OrderKind::SecondaryLoanSale {
        let loan_pda = order.loan_pda;
        remove_order_from_tree_and_free(fixed, dynamic, order_index, order.side);
        return Ok(Some(loan_pda));
    }
    // Vault primary asks skip encumber at place time (place_order_inner
    // gates on is_vault_lender) — bookkeeping happens via the profile's
    // RiskProfile.encumbered_in_orders_atoms instead. Mirror that here
    // by skipping unencumber for vault-owned seats; otherwise the
    // checked_sub on debt_encumbered_shares would error and vault asks
    // would become un-cancellable.
    let owner_kind: u8 = {
        let seat = get_helper_seat(dynamic, order.trader_seat_index).get_value();
        seat.owner_kind
    };
    if owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
        let seat = get_mut_helper_seat(dynamic, order.trader_seat_index).get_mut_value();
        seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
        remove_order_from_tree_and_free(fixed, dynamic, order_index, order.side);
        return Ok(None);
    }
    // Decrement at the order's recorded snapshot — byte-symmetric with
    // the encumber that ran when the order was placed.
    let snapshot = order.share_price_snapshot();
    unencumber_for_order(
        dynamic,
        order.trader_seat_index,
        order.side,
        atoms_to_shares_at_snapshot(order.principal_atoms, snapshot),
        atoms_to_shares_at_snapshot(order.collateral_atoms, snapshot),
    )?;
    remove_order_from_tree_and_free(fixed, dynamic, order_index, order.side);
    Ok(None)
}

// ────────────────── SecondaryLoanSale placement ──────────────────

/// Args for placing a `SecondaryLoanSale` bid. All snapshot fields
/// come from the loan PDA being put up for sale.
pub struct PlaceSecondaryBidArgs {
    pub market_pubkey: Pubkey,
    pub seller_seat_index: DataIndex,
    /// Loan being put up for sale. Used both to validate ownership
    /// (`loan.lender_seat_index == seller_seat_index`) and to
    /// duplicate-check (no other resting secondary bid for this loan).
    pub loan_pda: Pubkey,
    /// Loan's current lender_seat_index — checked against
    /// `seller_seat_index` for ownership.
    pub loan_lender_seat_index: DataIndex,
    /// Loan's `matched_loan_sequence` (read off the loan PDA at
    /// placement) — stamped onto the resting order so the matching
    /// engine can pass it through to the MatchedLoan queue node at
    /// cross time. The cranker derives the loan PDA address from
    /// `[b"loan", market, sequence_le]` to find what to mutate.
    pub loan_sequence_snapshot: u32,
    /// Snapshot from the loan: copied onto the resting bid.
    pub snapshot_rate_bps: u16,
    pub snapshot_term_seconds: u32,
    pub snapshot_principal_atoms: u64,
    /// What the seller wants in cash now.
    pub asking_price_atoms: u64,
    pub last_valid_unix_ts: i64,
    pub flags: u8,
    pub now_unix_ts: i64,
}

/// Result of placing a secondary bid.
#[derive(Clone, Copy)]
pub struct PlaceSecondaryBidResult {
    pub sequence: u64,
    pub order_index: DataIndex,
}

/// Insert a `SecondaryLoanSale` bid into the bids tree. Performs:
/// (1) ownership check (`loan.lender_seat_index == seller_seat_index`),
/// (2) cardinality check (no other resting secondary bid for this loan),
/// (3) slot allocation, (4) sequence assignment, (5) tree insert.
///
/// No encumbrance: secondary bids don't lock up the seller's seat
/// balances. The loan's existing collateral stays attached on the Loan
/// PDA, untouched.
pub fn place_secondary_bid(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: PlaceSecondaryBidArgs,
) -> Result<PlaceSecondaryBidResult, ProgramError> {
    // (1) Ownership.
    require!(
        args.loan_lender_seat_index == args.seller_seat_index,
        YdeltaError::SecondaryNotCurrentLender,
        "signer's seat is not the loan's current lender seat"
    )?;

    // Expiration sanity (mirrors primary placement).
    assert_not_already_expired(args.last_valid_unix_ts, args.now_unix_ts)?;

    // (2) Cardinality is enforced by the caller via
    // `LoanFixed.has_resting_secondary_bid` (O(1) flag). The previous
    // O(N) bids-tree walk is gone — caller checks the flag before
    // calling this helper, sets it to 1 after a successful insert,
    // and clears it on cancel / cranker finalize / staleness sweep.

    // (3) Allocate a free block from the market's shared free list.
    let order_index = get_free_address_on_market_fixed_for_bid_order(fixed, dynamic);
    require!(
        is_not_nil!(order_index),
        ProgramError::AccountDataTooSmall,
        "No free block for secondary bid — expand market"
    )?;

    // (4) Assign sequence.
    let seq = fixed.order_sequence_number;
    fixed.order_sequence_number = seq.wrapping_add(1);

    // (5) Construct + insert. share_price_snapshot is unused for
    // secondary (no encumbrance to reverse) but stamped to 0 for
    // determinism.
    let resting = RestingOrder::new_secondary_bid(
        args.seller_seat_index,
        args.loan_sequence_snapshot,
        seq,
        args.snapshot_rate_bps,
        args.snapshot_term_seconds,
        args.snapshot_principal_atoms,
        args.loan_pda,
        args.asking_price_atoms,
        args.last_valid_unix_ts,
        args.flags,
        /*share_price_snapshot_fp48=*/ 0,
    );

    let mut market = MarketRefMut { fixed, dynamic };
    market.rest_order(order_index, resting)?;

    Ok(PlaceSecondaryBidResult {
        sequence: seq,
        order_index,
    })
}

// ────────── Scenario B: secondary-bid taker ──────────

/// Args for `match_secondary_bid_against_asks`. The caller
/// (place_order's secondary branch) walks the asks tree before
/// resting the bid: any compatible primary ask (rate + term + cash)
/// crosses immediately, mirroring Scenario A but with the bid as
/// taker instead of maker.
pub struct MatchSecondaryBidArgs {
    pub market_pubkey: Pubkey,
    /// Seller's seat (taker side).
    pub seller_seat_index: DataIndex,
    /// Loan being put up for sale.
    pub loan_pda: Pubkey,
    /// Loan's matched_loan_sequence — stamped onto each queue node so
    /// the cranker can derive the loan PDA.
    pub loan_sequence_snapshot: u32,
    /// Loan's snapshot fields. The cross gate uses
    /// `borrower_rate_bps`, not the seller's lender rate.
    pub borrower_rate_bps: u16,
    pub term_remaining_seconds: u32,
    /// Loan's full current principal (also = bid principal at place
    /// time per par-exit invariant).
    pub principal_atoms: u64,
    pub now_unix_ts: i64,
    pub fee_floor_bps: u16,
}

/// Result.
#[derive(Clone, Copy, Default)]
pub struct MatchSecondaryBidResult {
    /// Unmatched principal that should be rested as a SecondaryLoanSale
    /// bid (zero if the taker was fully consumed by crosses).
    pub residual_principal_atoms: u64,
    pub num_fills: u32,
}

/// Walks the asks tree and crosses any primary ask whose
/// `rate_bps + fee_floor_bps <= borrower_rate_bps` AND
/// `term_seconds >= term_remaining_seconds` AND has sufficient
/// remaining principal. Each cross emits a SECONDARY-flagged
/// MatchedLoan queue node and decrements the ask side's debt
/// encumbrance. Returns the residual (zero on full taker fill).
///
/// Risk-profile asks participate: when the maker is a risk_profile
/// (`owner_kind == OWNER_KIND_RISK_PROFILE`), the gate (idle pool +
/// per-market exposure cap) gates the cross and the gate's
/// bookkeeping (encumbered_in_orders, seat.deployed_atoms) updates
/// inline under the vault-account borrow. The cranker's
/// secondary-finalize path then runs `do_vault_settle` to migrate
/// atoms from `vault.integration` into the market for the seller's
/// payout.
pub fn match_secondary_bid_against_asks(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: MatchSecondaryBidArgs,
    vault_ai: Option<&solana_program::account_info::AccountInfo<'_>>,
) -> Result<MatchSecondaryBidResult, ProgramError> {
    let mut remaining_principal = args.principal_atoms;
    let mut num_fills = 0u32;
    let mut current_maker_index: DataIndex = fixed.asks_best_index;

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            // Vault makers skip encumber at place; mirror that on the
            // expired-maker drop here (matches the primary-match path
            // and cancel_order_by_index).
            let maker_owner_kind: u8 = {
                let seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
                seat.owner_kind
            };
            if maker_owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
                let seat = get_mut_helper_seat(dynamic, maker.trader_seat_index).get_mut_value();
                seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
            } else {
                let snap = maker.share_price_snapshot();
                unencumber_for_order(
                    dynamic,
                    maker.trader_seat_index,
                    maker.side,
                    atoms_to_shares_at_snapshot(maker.principal_atoms, snap),
                    atoms_to_shares_at_snapshot(maker.collateral_atoms, snap),
                )?;
            }
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            emit_stack(OrderExpiredLog {
                market: args.market_pubkey,
                owner_seat_index: maker.trader_seat_index,
                side: maker.side as u8,
                _padding: [0; 3],
                sequence: maker.sequence_number,
            })?;
            current_maker_index = fixed.asks_best_index;
            continue;
        }

        // Self-match prevention.
        require!(
            maker.trader_seat_index != args.seller_seat_index,
            YdeltaError::SelfMatchForbidden,
            "secondary-bid taker seat {} matches its own resting ask at index {}",
            args.seller_seat_index,
            current_maker_index
        )?;

        // Skip non-primary makers — resting asks are always primary
        // in yDelta (secondary orders rest on the bids tree).
        if maker.kind != OrderKind::Primary {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        // Detect risk-profile maker. Buyer-side risk-profile bookkeeping
        // happens inline below if the cross is admitted.
        let maker_is_risk_profile: bool = {
            let seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            seat.owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE
        };

        // Cross gate (rate). bid_rate = loan's borrower_rate (the
        // protocol can route this to a new lender at maker.rate iff
        // borrower_rate >= maker.rate + floor).
        let spread_bps = (args.borrower_rate_bps as i32) - (maker.rate_bps as i32);
        if spread_bps < args.fee_floor_bps as i32 {
            // Asks tree is rate-sorted in the taker's favour — once
            // the best ask fails the floor, no later ask satisfies.
            break;
        }
        // Term gate: bid_term <= ask_term.
        if args.term_remaining_seconds > maker.term_seconds {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        let matched_principal = remaining_principal.min(maker.principal_atoms);

        // Par-exit pricing — cash paid to seller = matched_principal.
        let cash_paid = matched_principal;
        if maker.principal_atoms < cash_paid {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        // Risk-profile maker: idle/exposure gate + inline state
        // mutation under the vault account borrow. Mirror of the
        // primary matching engine's gate. Skip the maker silently if
        // either gate fails — risk-profile orders are non-removing.
        if maker_is_risk_profile {
            let Some(vault_ai_ref) = vault_ai else {
                // Caller didn't pass the GlobalVault; can't gate or
                // mutate. Skip rather than fail the whole tx.
                current_maker_index =
                    next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
                continue;
            };
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            let profile_id = lender_seat.risk_profile_id;
            let profile_idle: u64 = {
                let vault_data = vault_ai_ref.try_borrow_data()?;
                let (fixed_bytes, vault_dyn) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
                let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
                let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
                let tree =
                    RiskProfileTreeReadOnly::new(vault_dyn, header.risk_profiles_root_index, NIL);
                let idx = tree.lookup_index(&probe);
                if idx == NIL {
                    0
                } else {
                    let p = get_helper_risk_profile(vault_dyn, idx).get_value();
                    p.total_principal_atoms
                        .saturating_sub(p.deployed_principal_atoms)
                        .saturating_sub(p.encumbered_in_orders_atoms)
                }
            };
            // Same per-market hard cap as the primary-cross gate.
            let max_exposure = lender_seat.max_exposure_atoms();
            let new_seat_deployed = lender_seat
                .deployed_atoms()
                .saturating_add(matched_principal);
            if profile_idle < matched_principal || new_seat_deployed > max_exposure {
                current_maker_index =
                    next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
                continue;
            }
            // Accept: bump encumbered_in_orders inline.
            {
                use crate::state::vault::{
                    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile,
                    RiskProfileTreeReadOnly,
                };
                use crate::state::GLOBAL_VAULT_FIXED_SIZE;
                let mut vault_data = vault_ai_ref.try_borrow_mut_data()?;
                let (fixed_bytes, vault_dyn) = vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
                let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
                let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
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
            let seat_mut = get_mut_helper_seat(dynamic, maker.trader_seat_index).get_mut_value();
            seat_mut.set_deployed_atoms(new_seat_deployed);
        } else {
            // Wallet maker: decrement debt encumbrance at the maker's
            // place-time snapshot.
            let maker_snapshot = maker.share_price_snapshot();
            decrement_encumbrance_on_match(
                dynamic,
                maker.trader_seat_index,
                Side::Ask,
                atoms_to_shares_at_snapshot(cash_paid, maker_snapshot),
                0,
            )?;
        }

        // Reduce or remove maker order. Risk-profile orders persist —
        // their per-market cap is governed by the gate above, not by
        // `principal_atoms` depletion.
        let did_full_ask_fill = matched_principal == maker.principal_atoms;
        if maker_is_risk_profile {
            // Persist the order as-is.
        } else if did_full_ask_fill {
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            decrement_open_count(dynamic, maker.trader_seat_index, Side::Ask);
        } else {
            let order_mut = get_mut_helper_order(dynamic, current_maker_index).get_mut_value();
            order_mut.principal_atoms = order_mut
                .principal_atoms
                .checked_sub(matched_principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        // Insert SECONDARY-flagged MatchedLoan queue node (same shape
        // as Scenario A, lines ~672-720).
        let did_full_taker_fill = matched_principal == remaining_principal;
        let sequence = fixed.matched_loan_sequence;
        // New lender = the resting ask maker. Their debt-side
        // encumbrance was sampled at THEIR place-order time and lives
        // on the maker's resting order.
        let maker_snapshot_fp48 = maker.share_price_snapshot();
        let mut node: MatchedLoan = Default::default();
        node.sequence = sequence;
        node.principal_atoms = matched_principal;
        node.cash_paid_atoms = cash_paid;
        node.referenced_loan_sequence = args.loan_sequence_snapshot as u64;
        node.matched_at_unix = args.now_unix_ts;
        node.lender_seat_index = args.seller_seat_index; // OLD lender (seller, taker)
        node.new_lender_seat_index = maker.trader_seat_index; // NEW lender (resting ask)
        node.lender_rate_bps = maker.rate_bps; // new rate
        node.borrower_rate_bps = args.borrower_rate_bps; // immutable
        node.term_seconds = args.term_remaining_seconds;
        node.loan_type = 0; // Fixed
                            // Split iff taker not fully consumed by THIS match. If the
                            // taker is fully consumed and there's no residual, the cranker
                            // performs a full transfer; otherwise it splits.
        node.flags = MATCHED_LOAN_FLAG_SECONDARY
            | (if did_full_taker_fill {
                0
            } else {
                MATCHED_LOAN_FLAG_SECONDARY_SPLIT
            });
        // New lender's debt snapshot lives on the resting ask maker.
        // Borrower-side snapshot stays 0 (cranker copies from existing loan).
        node.lender_debt_share_price_snapshot_fp48 = maker_snapshot_fp48;

        let node_index = get_free_address_on_market_fixed_for_matched_loan(fixed, dynamic);
        require!(
            is_not_nil!(node_index),
            ProgramError::AccountDataTooSmall,
            "No free block for secondary MatchedLoan (Scenario B) — expand market"
        )?;
        let mut matched_tree = MatchedLoanTree::new(dynamic, fixed.matched_loans_root_index, NIL);
        matched_tree.insert(node_index, node);
        fixed.matched_loans_root_index = matched_tree.get_root_index();
        drop(matched_tree);

        emit_stack(MatchedLoanCreatedLog {
            market: args.market_pubkey,
            loan_pda: args.loan_pda,
            sequence,
            lender_seat_index: args.seller_seat_index,
            borrower_seat_index: NIL, // cranker reads from loan PDA
            principal_atoms: matched_principal,
            collateral_atoms: 0,
            borrower_rate_bps: args.borrower_rate_bps,
            lender_rate_bps: maker.rate_bps,
            term_seconds: args.term_remaining_seconds,
            matched_at_unix: args.now_unix_ts,
            loan_type: 0,
            flags: MATCHED_LOAN_FLAG_SECONDARY
                | (if did_full_taker_fill {
                    0
                } else {
                    MATCHED_LOAN_FLAG_SECONDARY_SPLIT
                }),
            _padding: [0; 6],
        })?;
        fixed.matched_loan_sequence = fixed.matched_loan_sequence.wrapping_add(1);

        remaining_principal = remaining_principal
            .checked_sub(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        num_fills = num_fills.saturating_add(1);

        // Walk: removed maker → re-read best; partial fill → next-lower.
        current_maker_index = if did_full_ask_fill {
            fixed.asks_best_index
        } else {
            next_maker_index(fixed, dynamic, Side::Bid, current_maker_index)
        };
    }

    Ok(MatchSecondaryBidResult {
        residual_principal_atoms: remaining_principal,
        num_fills,
    })
}

// ────────── ConvertP2PoolToFixed: walk asks and emit Fixed nodes ──────────

/// Args for `match_p2pool_residual_against_asks`. Borrower holds an
/// existing P2Pool loan and is refinancing the residual into one or
/// more Fixed loans by walking the asks tree. Each cross emits a
/// regular Fixed `MatchedLoan` queue node — the cranker promotes each
/// into a fresh `LoanFixed` PDA via `process_matched_loan`.
///
/// Pricing: the new Fixed loan's `borrower_rate_bps == lender_rate_bps
/// == ask.rate_bps` (no spread captured, no origination fee — same
/// convention as the P2Pool path).
///
/// Risk-profile ask makers are skipped (deferred).
pub struct MatchP2PoolRefinanceArgs {
    pub market_pubkey: Pubkey,
    /// Original P2Pool loan's borrower seat. Stamped on each new Fixed
    /// MatchedLoan as `borrower_seat_index`.
    pub borrower_seat_index: DataIndex,
    /// Cap on convertible principal — the loan's current
    /// `principal_debt_atoms`. The matcher stops at this limit so the
    /// caller can't overshoot the live P2Pool body.
    pub principal_cap_atoms: u64,
    /// Original P2Pool loan's full collateral — split pro-rata across
    /// crosses. matched_collateral_per_cross =
    /// `loan_collateral × matched_principal / loan_principal`.
    pub loan_collateral_atoms: u64,
    /// Original P2Pool loan's borrower-collateral place-time snapshot.
    /// Propagated onto each new Fixed loan so the borrower's seat
    /// decrement at full repay / liquidation stays byte-symmetric with
    /// the original encumber.
    pub borrower_collateral_share_price_snapshot_fp48: u128,
    /// Term remaining on the P2Pool loan (`matures_at - now`). New
    /// Fixed loans are stamped with this as their `term_seconds`.
    pub term_remaining_seconds: u32,
    pub max_acceptable_rate_bps: u16,
    pub now_unix_ts: i64,
}

#[derive(Default)]
pub struct MatchP2PoolRefinanceResult {
    /// Sum of `matched_principal` over all crosses.
    pub total_filled_principal_atoms: u64,
    /// Sum of pro-rata collateral over all crosses. Caller subtracts
    /// from the loan's `collateral_atoms` field.
    pub total_filled_collateral_atoms: u64,
    pub num_fills: u32,
}

/// Walk the asks tree and cross any compatible primary ask whose
/// `rate_bps <= max_acceptable_rate_bps` AND
/// `term_seconds >= term_remaining_seconds`. Each cross:
///   1. Decrements the maker's `debt_encumbered_shares` at their
///      place-time snapshot (byte-symmetric with their place_order
///      encumber).
///   2. Reduces / removes the maker order from the asks tree.
///   3. Inserts a Fixed `MatchedLoan` queue node with the borrower
///      stamped to `args.borrower_seat_index` and the collateral split
///      pro-rata against the loan's full collateral.
///
/// The caller (`process_convert_p2pool_to_fixed`) handles the
/// consolidated `marginfi.withdraw → marginfi.repay_atoms` CPI pair
/// and the loan-body update / close at the end of the matching pass.
///
/// Risk-profile makers (`owner_kind == OWNER_KIND_RISK_PROFILE`) are
/// silently skipped — the `do_vault_settle` plumbing for migrating
/// atoms out of the vault on a refinance is deferred.
///
/// Self-match prevention: if a maker's seat == args.borrower_seat_index
/// the helper errors `SelfMatchForbidden`. Same invariant the primary
/// matching engine enforces.
pub fn match_p2pool_residual_against_asks(
    fixed: &mut MarketFixed,
    dynamic: &mut [u8],
    args: MatchP2PoolRefinanceArgs,
) -> Result<MatchP2PoolRefinanceResult, ProgramError> {
    let mut remaining_principal = args.principal_cap_atoms;
    let mut total_filled_principal: u64 = 0;
    let mut total_filled_collateral: u64 = 0;
    let mut num_fills: u32 = 0;
    let mut current_maker_index: DataIndex = fixed.asks_best_index;

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            // Same expired-maker handling as the primary matching engine.
            let maker_owner_kind: u8 = {
                let seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
                seat.owner_kind
            };
            if maker_owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
                // Risk-profile orders are non-expiring; reaching this
                // branch for one is corrupt state. Walk past defensively.
                let seat = get_mut_helper_seat(dynamic, maker.trader_seat_index).get_mut_value();
                seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
            } else {
                let snap = maker.share_price_snapshot();
                unencumber_for_order(
                    dynamic,
                    maker.trader_seat_index,
                    maker.side,
                    atoms_to_shares_at_snapshot(maker.principal_atoms, snap),
                    atoms_to_shares_at_snapshot(maker.collateral_atoms, snap),
                )?;
            }
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            emit_stack(OrderExpiredLog {
                market: args.market_pubkey,
                owner_seat_index: maker.trader_seat_index,
                side: maker.side as u8,
                _padding: [0; 3],
                sequence: maker.sequence_number,
            })?;
            current_maker_index = fixed.asks_best_index;
            continue;
        }

        // Self-match prevention.
        require!(
            maker.trader_seat_index != args.borrower_seat_index,
            YdeltaError::SelfMatchForbidden,
            "convert refinance: borrower seat {} matches their own resting ask at index {}",
            args.borrower_seat_index,
            current_maker_index
        )?;

        // Skip non-primary makers (resting asks should always be primary,
        // but guard defensively).
        if maker.kind != OrderKind::Primary {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        // Skip risk-profile makers — refinance into vault-funded Fixed
        // loans needs `do_vault_settle` plumbing that's deferred.
        let maker_is_risk_profile: bool = {
            let seat = get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            seat.owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE
        };
        if maker_is_risk_profile {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        // Rate gate. Asks tree is rate-sorted in the taker's favour —
        // once the best wallet ask exceeds `max_acceptable_rate_bps`,
        // no later (worse-rate) ask will satisfy it either. Break.
        if maker.rate_bps > args.max_acceptable_rate_bps {
            break;
        }

        // Term gate.
        if maker.term_seconds < args.term_remaining_seconds {
            current_maker_index = next_maker_index(fixed, dynamic, Side::Bid, current_maker_index);
            continue;
        }

        let matched_principal = remaining_principal.min(maker.principal_atoms);
        // Pro-rata collateral split. principal_cap_atoms is the loan's
        // total principal; matched_principal is this chunk's share. We
        // can't divide by zero — `principal_cap_atoms > 0` is enforced
        // by the loan body's `outstanding > 0` invariant the caller
        // checks before invoking the matcher.
        let matched_collateral: u64 = ((args.loan_collateral_atoms as u128)
            .checked_mul(matched_principal as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / args.principal_cap_atoms as u128) as u64;

        // Decrement maker encumbrance at the maker's place-time
        // snapshot — byte-symmetric with their place_order encumber.
        let maker_snapshot = maker.share_price_snapshot();
        decrement_encumbrance_on_match(
            dynamic,
            maker.trader_seat_index,
            Side::Ask,
            atoms_to_shares_at_snapshot(matched_principal, maker_snapshot),
            0,
        )?;

        // Reduce / remove the maker order.
        let did_full_ask_fill = matched_principal == maker.principal_atoms;
        if did_full_ask_fill {
            remove_order_from_tree_and_free(fixed, dynamic, current_maker_index, maker.side);
            decrement_open_count(dynamic, maker.trader_seat_index, Side::Ask);
        } else {
            let order_mut = get_mut_helper_order(dynamic, current_maker_index).get_mut_value();
            order_mut.principal_atoms = order_mut
                .principal_atoms
                .checked_sub(matched_principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        // Insert a Fixed MatchedLoan node. No origination fee, no
        // protocol-fee floor capture: borrower_rate == lender_rate ==
        // ask.rate_bps so there's no spread for the protocol to take.
        // Same convention as the existing P2Pool path.
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
        node.borrower_rate_bps = maker.rate_bps;
        node.lender_rate_bps = maker.rate_bps;
        node.loan_type = 0; // Fixed
        node.flags = 0;
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
            borrower_rate_bps: maker.rate_bps,
            lender_rate_bps: maker.rate_bps,
            term_seconds: args.term_remaining_seconds,
            matched_at_unix: args.now_unix_ts,
            loan_type: 0,
            flags: 0, // ConvertP2PoolToFixed: fresh Fixed loan, no flags
            _padding: [0; 6],
        })?;
        fixed.matched_loan_sequence = fixed.matched_loan_sequence.wrapping_add(1);

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

        current_maker_index = if did_full_ask_fill {
            fixed.asks_best_index
        } else {
            // Partial maker fill means the taker is exhausted (we capped
            // matched_principal to `remaining_principal`). The
            // `remaining_principal > 0` loop guard exits next iteration.
            NIL
        };
    }

    Ok(MatchP2PoolRefinanceResult {
        total_filled_principal_atoms: total_filled_principal,
        total_filled_collateral_atoms: total_filled_collateral,
        num_fills,
    })
}
