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

/// Per-cross unmatchable buffer left on every vault profile to absorb
/// marginfi v0.1.8's share-mint rounding tax.
///
/// Marginfi mints asset shares as `floor(amount × 2^48 / asset_share_value)`
/// on deposit and converts back with floor on withdraw, so the round-trip
/// `shares_to_amount(amount_to_asset_shares(N))` is bounded above by `N`
/// — at asv > 1.0, depositing `N` atoms leaves the vault with `N − 1`
/// atoms of marginfi share-equivalent value. If matching were allowed to
/// deploy the full `profile.total_principal_atoms`, the crank's
/// `marginfi.withdraw` of `principal_atoms` would under-fund the loan
/// (marginfi can't deliver the missing atom that was never in shares to
/// begin with), and `do_vault_settle`'s `actual_atoms >= principal_atoms`
/// guard would hard-fail with `ProgramError::ArithmeticOverflow`.
///
/// Reserving 1 atom per profile from matching is the smallest fix that
/// keeps the protocol composable with marginfi's known rounding mode.
/// The reserved atom is NOT locked — depositors can still withdraw it
/// via `global_vault_withdraw` (which uses the actual idle, not the
/// matching idle).
pub const MARGINFI_ROUNDING_RESERVE_ATOMS: u64 = 1;

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
    /// The taker is always a `Side::Bid` borrower — kept as a field so
    /// `RestingOrder`/log stamping stays uniform, but never anything
    /// other than `Bid`.
    pub side: Side,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub order_type: OrderType,
    pub now_unix_ts: i64,
    pub fee_floor_bps: u16,
    /// fp48 share-price snapshot recorded at place-order time for the
    /// taker. The taker's encumber at `match_borrower_bid` used this
    /// same snapshot, so per-match decrement uses it too — keeping
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
}

#[derive(Default, Clone)]
pub struct MatchResult {
    pub remaining_principal: u64,
    pub remaining_collateral: u64,
    pub total_filled_principal: u64,
    pub num_fills: u32,
    /// Fate of any residual after the matching pass. The borrower bid
    /// is IOC — it never rests.
    /// `Drop` — OB_ONLY-flagged residual; the processor unencumbers
    /// and emits `OrderFilledIocLog`.
    /// `P2PoolBorrow` — residual fires a `marginfi.borrow` CPI for
    /// the unfilled atoms; processor records a `MatchedLoan` with
    /// `loan_type = P2Pool` and the resulting
    /// `borrower_marginfi_borrow_shares`.
    pub residual_action: ResidualAction,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResidualAction {
    #[default]
    Drop,
    /// The borrower bid's unfilled residual auto-borrows from the debt
    /// bank via `marginfi.borrow`. The borrower-side and lender-side
    /// marginfi accounts are kept separate per market to avoid the
    /// marginfi constraint that an account cannot simultaneously hold
    /// an asset and a liability on the same bank.
    P2PoolBorrow,
}

/// Bit position 1 in `RestingOrder.flags` / `PlaceOrderArgs.flags`. When
/// set on a borrower bid, the unfilled residual goes to `Drop` instead
/// of triggering the P2Pool fallback. Default OFF.
pub const FLAG_OB_ONLY: u8 = 0b0000_0010;

/// Walk the asks tree from best, applying cross conditions and
/// settlement. The taker is always a borrower `Side::Bid`; the resting
/// makers are unbounded vault risk-profile asks ("quote all idle").
///
/// Iteration shape:
/// - `current_maker_index` starts at `asks_best_index` (= tree's
///   `max_index`, which under our `RestingOrder` Ord direction is the
///   BEST resting ask).
/// - **One cross per maker**: a standing vault ask is never removed by
///   matching — after crossing (or skipping) a maker the loop ALWAYS
///   advances to the in-order predecessor via `next_maker_index`. A
///   borrower bid thus crosses each compatible ask at most once; the
///   loop terminates when `remaining_principal == 0` or no more makers.
// `vault_ai` is the GlobalVault account (Some when a vault account was
// passed to the calling ix). When the matching loop hits a risk-profile
// maker it reads the profile's idle pool inline; each cross is sized at
// `min(remaining_principal, profile_idle)` and bumps
// `RiskProfile.encumbered_in_orders_atoms` inline (the cranker-race
// guard). A maker whose profile has zero idle is silently skipped.
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

    // Share-rounding dust sweep for the taker's collateral
    // encumbrance. The taker's collateral was encumbered as a SINGLE
    // `atoms_to_shares_at_snapshot(args.collateral_atoms, snapshot)`
    // conversion. Decrementing per cross re-converts each cross's atoms
    // separately, and each conversion FLOORS — so `Σ per-cross shares`
    // can fall 0..N−1 units short of the single encumbered total,
    // leaving share dust frozen in the borrower's encumbered bucket even
    // after the atom-level dust sweep below. To guarantee an exact
    // zero, the FINAL cross decrements the EXACT remaining encumbered
    // shares: `total_collateral_shares − Σ(already decremented)`.
    let total_collateral_shares =
        atoms_to_shares_at_snapshot(args.collateral_atoms, args.taker_share_price_snapshot_fp48);
    let mut decremented_collateral_shares: u128 = 0;

    // The taker is always a borrower Bid — it crosses the ASKS tree.
    let mut current_maker_index: DataIndex = fixed.asks_best_index;

    while remaining_principal > 0 && is_not_nil!(current_maker_index) {
        let maker: RestingOrder = *get_helper_order(dynamic, current_maker_index).get_value();

        if maker.is_expired(args.now_unix_ts) {
            // Every resting order is a vault risk-profile ask. Vault
            // asks are placed non-expiring
            // (`last_valid_unix_ts = 0`), so reaching this branch means
            // corrupted state. Walk past defensively rather than
            // removing — only the curator removes risk-profile orders,
            // via `cancel_order_for_risk_profile`. There is no per-seat
            // encumbrance to unwind (vault asks carry none).
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
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

        // Cross condition. The taker is always the borrower Bid; the
        // resting maker is always a vault Ask.
        let (bid_rate, ask_rate, bid_term, ask_term) = (
            args.rate_bps,
            maker.rate_bps,
            args.term_seconds,
            maker.term_seconds,
        );
        if bid_rate < ask_rate {
            // Quote-only rate rule: a bid crosses any ask at or below the
            // bid rate — equal rates included. The protocol floor is not
            // a cross gate; it is added on top of the lender rate when the
            // borrower rate is stamped below. The asks tree is rate-sorted
            // ascending (best/cheapest first), so once the best ask
            // exceeds the bid no later ask crosses either — break.
            break;
        }
        if bid_term > ask_term {
            // Term mismatch — rate ordering is independent of term,
            // so a later (worse-rate) maker with a longer ask_term
            // may still cross. Walk to the in-order predecessor
            // (next-best under our Ord direction) and try again.
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        if !order_type_can_take(args.order_type) {
            assert_can_take(args.order_type)?;
        }

        // ─────────── Match-time vault gate (unbounded ask) ───────────
        //
        // A vault ask is an UNBOUNDED standing quote — it carries a
        // sentinel `principal_atoms = u64::MAX` and is NOT a depth cap.
        // Each cross is sized at `min(remaining_principal, profile_idle)`
        // where `profile_idle = total_principal - deployed - encumbered`
        // read live off the vault. If the profile has zero idle there is
        // nothing to lend — skip this maker.
        //
        // CRITICAL: on accept we bump `RiskProfile.encumbered_in_orders_atoms`
        // synchronously here — before the match-time tx returns. The
        // cranker is a SEPARATE transaction (often many slots later)
        // that runs `do_vault_settle` to physically migrate the
        // principal from `vault.integration` to `market.lender_integration`;
        // until it does, this inline write is the ONLY thing preventing
        // a second bid from seeing the same atoms as idle and
        // double-spending the profile. Subsequent iterations of THIS
        // matching loop re-read the same profile and see the updated
        // state; subsequent transactions re-read on entry. Any future
        // redesign that queues this bookkeeping re-introduces the
        // cranker-race window.
        //
        // OWNER_KIND_RISK_PROFILE denotes "a profile in the vault owns
        // the seat", not "the vault as a whole owns it" — so the live
        // state we care about lives on `RiskProfile`, not the vault
        // header.
        let matched_principal: u64;
        // Curator-set lender LTV cap, read live from the crossed
        // maker's `RiskProfile`. 0 means "no profile cap beyond the
        // marginfi-init weights" and the extra LTV check below is
        // skipped. Captured here under the same vault borrow that reads
        // `profile_idle`.
        let mut profile_max_ltv_bps: u16 = 0;
        {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            debug_assert_eq!(
                lender_seat.owner_kind,
                crate::state::OWNER_KIND_RISK_PROFILE
            );
            let vault_ai_ref = match vault_ai {
                Some(v) => v,
                None => {
                    // No vault account in scope — cannot size or gate a
                    // vault ask. Skip the maker defensively.
                    current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                    continue;
                }
            };
            let profile_id = lender_seat.risk_profile_id;
            // Read profile.idle (and the profile's curator LTV cap) from
            // the vault. Read-only borrow scoped to this block so the
            // mut-borrow below can re-acquire.
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
                // Nothing to lend — skip this maker, keep it resting.
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            matched_principal = remaining_principal.min(profile_idle);
            // Accept: bump profile.encumbered_in_orders_atoms inline so
            // subsequent matches (this loop or later txs) see the lock.
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

        // Taker is the borrower Bid — it carries all the collateral;
        // the vault Ask maker carries none.
        //
        // Collateral-dust sweep. The pro-rata split
        // `mul_div_u64` FLOORS, so across a multi-cross bid the sum of
        // per-cross `matched_collateral` can fall short of the bid's
        // `collateral_atoms` by up to N−1 atoms. On the cross that
        // drives `remaining_principal` to 0 (the FINAL cross — detected
        // by `matched_principal == remaining_principal`, since
        // `matched_principal = min(remaining_principal, idle)`), sweep
        // ALL remaining collateral into this loan instead of the floored
        // pro-rata share. That fully deploys the borrower's intended
        // collateral and leaves zero dust frozen in the borrower's
        // `collateral_encumbered_shares`. The dust rides on the last
        // loan as a benign over-collateralization, released to the
        // borrower at repay. Non-final crosses keep the floored split.
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

            // Profile LTV cap (lender side). The vault profile's
            // curator sets `max_ltv_bps` as the maximum loan-to-value
            // the profile is willing to lend at. The curator cap binds:
            // enforce `actual_ltv <= profile.max_ltv_bps`
            // by re-running the same collateral-requirement helper with
            // the cap expressed as weights — debt liability weight 1.0
            // and collateral asset weight `max_ltv_bps / 10_000`. A
            // `max_ltv_bps` of 0 means the profile sets no cap beyond
            // the marginfi-init weights checked above — skip.
            if profile_max_ltv_bps > 0 {
                // collateral asset weight = max_ltv_bps / 10_000 in
                // fp48. `to_scaled(max_ltv_bps)` lifts the bps integer
                // into fp48, then dividing by 10_000 yields the ratio.
                let collateral_asset_weight_fp48 =
                    crate::math::to_scaled(profile_max_ltv_bps as u128)? / 10_000u128;
                let required_at_profile_cap =
                    crate::state::ltv::get_required_quote_collateral_to_back_debt(
                        matched_principal,
                        args.debt_oracle_price_fp48,
                        args.collateral_oracle_price_fp48,
                        // liability weight = 1.0 in fp48.
                        crate::math::SCALE,
                        collateral_asset_weight_fp48,
                        /*ltv_buffer_bps=*/ 0,
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

        // Every resting maker is a vault risk-profile ask. Vault
        // profile orders are open-ended (backed by the vault's
        // profile-level idle pool, not by per-seat shares), so there is
        // no maker-side seat encumbrance.
        let maker_snapshot = maker.share_price_snapshot();

        // Taker decrement uses the taker's snapshot (carried in
        // MatchArgs from match_borrower_bid), pre-recorded at place-time
        // so the decrement matches the original encumber atom-for-atom
        // in fp48 share units.
        let taker_snapshot = args.taker_share_price_snapshot_fp48;
        // On the FINAL cross (the one that zeroes
        // `remaining_principal`) decrement the EXACT remaining
        // encumbered collateral shares so the borrower's encumbered
        // bucket lands at precisely zero — no per-cross floor dust. Note
        // the collateral-atom sweep above already set
        // `matched_collateral_taker = remaining_collateral` on this same
        // cross, so the atom side is exact too.
        let is_final_cross = matched_principal == remaining_principal;
        let collateral_shares_to_decrement = if is_final_cross {
            total_collateral_shares.saturating_sub(decremented_collateral_shares)
        } else {
            atoms_to_shares_at_snapshot(matched_collateral_taker, taker_snapshot)
        };
        decremented_collateral_shares = decremented_collateral_shares
            .checked_add(collateral_shares_to_decrement)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        decrement_encumbrance_on_match(
            dynamic,
            args.taker_seat_index,
            args.side,
            atoms_to_shares_at_snapshot(matched_principal, taker_snapshot),
            collateral_shares_to_decrement,
        )?;
        // Vault makers: profile.encumbered_in_orders is already written
        // inline at the gate above, so there is nothing to decrement on
        // the maker side.

        // Risk-profile asks are unbounded standing quotes — never
        // "fully filled" by a cross and never removed by matching. Only
        // the curator removes them via `cancel_order_for_risk_profile`.
        // Leave the resting order intact.

        // Taker is the borrower Bid; maker is the lender Ask.
        //
        // Rate stamping: the lender earns exactly their ask rate; the
        // borrower pays `max(bid_rate, ask_rate + floor)` so the
        // protocol always earns at least `protocol_fee_bps_floor` of
        // spread on top of the lender rate — even when bid == ask. The
        // borrower's bid is a ceiling on the *lender* rate, not on total
        // cost; the floor is added above it. This also structurally
        // guarantees `borrower_rate >= lender_rate`.
        let lender_rate = ask_rate;
        // Compute `ask_rate + fee_floor` in u32 so a near-`u16`
        // ceiling ask does NOT silently `saturating_add`-clamp. A clamp
        // would make `borrower_rate − lender_rate < fee_floor`, letting
        // the protocol under-collect its spread floor. If the sum
        // overflows `u16` the cross is economically un-stampable — hard
        // fail rather than quietly under-charge.
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

        // A standing vault ask is never "fully filled" — the
        // `RiskProfileOrderRef` is removed only when the curator cancels
        // the order, not on a cross.
        //
        // Stamp `MATCHED_LOAN_FLAG_VAULT_LENDER` on the node so the
        // cranker (`process_matched_loan`) routes wallet-vs-vault
        // settlement on the MATCH-TIME record, not a live seat re-read.
        // Every primary-cross maker is a vault risk profile (asserted
        // above where the maker seat is read), so the flag is
        // unconditionally set for orderbook-funded Fixed loans.
        let vault_flag: u8 = crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER;

        // Snapshots for byte-symmetric encumber/release on the
        // promoted loan. The matching engine holds both:
        //   - taker_snapshot: collateral bank for the borrower Bid taker
        //   - maker_snapshot: debt bank for the lender Ask maker
        // Bid taker = borrower (encumbers collateral); maker = lender.
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
        // `checked_add` (not `wrapping_add`): `matched_loan_sequence`
        // feeds the loan PDA seed and the `MatchedLoan` tree key. A wrap
        // at u64::MAX would alias an existing loan's address/key — hard
        // fail instead (u64 exhaustion is unreachable in practice).
        fixed.matched_loan_sequence = fixed
            .matched_loan_sequence
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;

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

        // One cross per maker: the vault ask stays resting, so always
        // advance to the in-order predecessor (next-best maker by rate
        // / FIFO). The loop guard exits when `remaining_principal == 0`
        // or the walk runs off the end of the tree.
        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
    }

    Ok(MatchResult {
        remaining_principal,
        remaining_collateral,
        total_filled_principal: total_filled,
        num_fills,
        // match_borrower_bid overwrites this based on the OB_ONLY flag
        // after the matching pass.
        residual_action: ResidualAction::Drop,
    })
}

/// In-order tree predecessor of `current` on the asks tree. Under our
/// `RestingOrder` Ord direction the predecessor is the next-best maker
/// by rate (and FIFO at equal rates). The taker is always a borrower
/// Bid, so the only tree ever walked is the asks tree.
fn next_maker_index(fixed: &MarketFixed, dynamic: &[u8], current: DataIndex) -> DataIndex {
    let tree: super::market::BooksideReadOnly =
        super::market::BooksideReadOnly::new(dynamic, fixed.asks_root_index, fixed.asks_best_index);
    tree.get_next_lower_index::<RestingOrder>(current)
}

/// Remove a `RestingOrder` node from the asks tree and return its slot
/// to the free list. Updates `asks_root_index` / `asks_best_index` in
/// `fixed`. Every resting order is a vault ask, so this only ever
/// touches the asks tree.
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

    /// Read the seat's `withdrawable` share balance for the given side.
    /// Used by the withdraw-all path to drain the seat exactly.
    pub fn withdrawable_shares_for_seat(&self, seat_index: DataIndex, is_debt: bool) -> u128 {
        let MarketRefMut { dynamic, .. } = self;
        let seat = get_helper_seat(dynamic, seat_index).get_value();
        if is_debt {
            seat.debt_withdrawable_shares
        } else {
            seat.collateral_withdrawable_shares
        }
    }

    /// Insert a vault ask `RestingOrder` into the asks tree. The only
    /// resting orders are vault risk-profile asks, so this only ever
    /// touches the asks tree.
    pub fn rest_order(&mut self, order_index: DataIndex, order: RestingOrder) -> ProgramResult {
        let MarketRefMut { fixed, dynamic } = self;
        debug_assert_eq!(order.side, Side::Ask as u8);
        let mut tree: Bookside =
            Bookside::new(dynamic, fixed.asks_root_index, fixed.asks_best_index);
        tree.insert(order_index, order);
        fixed.asks_root_index = tree.get_root_index();
        fixed.asks_best_index = tree.get_max_index();
        Ok(())
    }
}

// ─────────────────────── Place-order orchestrator ───────────────────────

/// Inputs for `match_borrower_bid` — the borrower IOC bid path.
#[derive(Clone, Copy)]
pub struct PlaceOrderArgs {
    pub market_pubkey: Pubkey,
    pub taker_seat_index: DataIndex,
    /// Always `Side::Bid` — kept for uniform `RestingOrder`/log
    /// stamping. The borrower is always the taker.
    pub side: Side,
    /// Always `OrderType::ImmediateOrCancel` — the borrower bid never
    /// rests.
    pub order_type: OrderType,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub flags: u8,
    pub now_unix_ts: i64,
    /// fp48 share-price snapshot for the collateral bank at
    /// place-order time. The processor reads it from the bank header in
    /// the `PlaceOrderContext`.
    pub share_price_snapshot_fp48: u128,
    // ─── Oracle prices + bank weights, snapshot at place-order time
    // and threaded into `match_order` for the LTV check ───
    pub debt_oracle_price_fp48: u128,
    pub collateral_oracle_price_fp48: u128,
    pub debt_liability_weight_init_fp48: u128,
    pub collateral_asset_weight_init_fp48: u128,
    /// True when the caller has the marginfi/oracle accounts in scope
    /// (i.e. went through `PlaceOrderContext`).
    pub enforce_ltv: bool,
}

/// Inputs for `rest_vault_ask` — the vault risk-profile ask path.
#[derive(Clone, Copy)]
pub struct RestVaultAskArgs {
    pub market_pubkey: Pubkey,
    /// The vault profile's market-side seat (`owner_kind == Vault`).
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
    /// When the residual triggers a P2Pool borrow,
    /// `match_borrower_bid` inserts a `MatchedLoan` with `loan_type =
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
            p2pool_loan_index: NIL,
            p2pool_loan_sequence: 0,
        }
    }
}

// ─────────────── Borrower IOC bid / vault ask placement ───────────────

/// Place a borrower IOC bid.
///
/// Encumbers the borrower seat's collateral, runs `match_order` against
/// the resting vault asks, and routes any residual: with `OB_ONLY`
/// unset the residual fires the P2Pool marginfi fallback (a
/// `MatchedLoan` with `loan_type = P2Pool`); with `OB_ONLY` set the
/// residual is dropped. The bid never rests on the book.
///
/// `vault_ai` is the GlobalVault account (Some when the caller passes
/// one). The matching loop reads each crossed risk-profile maker's
/// profile idle inline from this account and writes the match-time
/// commitment under the same borrow.
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

    // Encumber/match operate on fp48 shares computed from
    // `args.share_price_snapshot_fp48` (the collateral bank's
    // `asset_share_value` at place-order time).
    let snapshot = args.share_price_snapshot_fp48;
    let principal_shares = atoms_to_shares_at_snapshot(args.principal_atoms, snapshot);
    let collateral_shares = atoms_to_shares_at_snapshot(args.collateral_atoms, snapshot);
    encumber_for_order(
        dynamic,
        args.taker_seat_index,
        Side::Bid,
        principal_shares,
        collateral_shares,
    )?;

    let seq = fixed.order_sequence_number;
    // `checked_add` (not `wrapping_add`): see `matched_loan_sequence`.
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

    // Route the residual. With `OB_ONLY` unset, a residual after the
    // matching pass triggers the P2Pool fallback (auto-borrow from
    // marginfi). With `OB_ONLY` set, the residual drops.
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
                Side::Bid,
                atoms_to_shares_at_snapshot(match_result.remaining_principal, snapshot),
                atoms_to_shares_at_snapshot(match_result.remaining_collateral, snapshot),
            )?;
            decrement_open_count(dynamic, args.taker_seat_index, Side::Bid);

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
                flags: 0, // P2Pool: no VAULT_MAKER bits
                _padding: [0; 6],
            })?;
            // `checked_add` (not `wrapping_add`): `matched_loan_sequence`
            // feeds the loan PDA seed and the `MatchedLoan` tree key. A wrap
            // at u64::MAX would alias an existing loan's address/key — hard
            // fail instead (u64 exhaustion is unreachable in practice).
            fixed.matched_loan_sequence = fixed
                .matched_loan_sequence
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            p2pool_loan_index = node_index;
            p2pool_loan_sequence = sequence;
        }
        (ResidualAction::Drop, true) => {
            // IOC remainder dropped: reverse the encumbrance for the
            // unmatched portion since no live order will hold it. Same
            // snapshot as the original encumber so the math cancels
            // exactly.
            unencumber_for_order(
                dynamic,
                args.taker_seat_index,
                Side::Bid,
                atoms_to_shares_at_snapshot(match_result.remaining_principal, snapshot),
                atoms_to_shares_at_snapshot(match_result.remaining_collateral, snapshot),
            )?;
            decrement_open_count(dynamic, args.taker_seat_index, Side::Bid);
        }
        (_, false) => {
            // Fully filled: the taker's order never rested, so remove
            // the open-counter bump done by `encumber_for_order`.
            decrement_open_count(dynamic, args.taker_seat_index, Side::Bid);
        }
    }

    Ok(PlaceOrderResult {
        sequence: seq,
        match_result,
        p2pool_loan_index,
        p2pool_loan_sequence,
    })
}

/// Rest a vault risk-profile ask on the asks tree.
///
/// Vault asks are PostOnly by design — they never take. This is a pure
/// insert: no matching runs. The vault `ClaimedSeat` carries no per-seat
/// shares, so no encumbrance is taken. The resting order is UNBOUNDED —
/// it carries a sentinel `principal_atoms = u64::MAX` ("quote all
/// idle"). The matching engine ignores that field for sizing; each
/// cross is capped by the profile's live idle pool at match time.
/// Only the open-lend counter is bumped so cancel-path accounting stays
/// balanced. Returns the new order's sequence number.
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

    // Open-ended vault profile order. No per-seat share-backing is
    // encumbered. Bump the open-lend counter so cancel/expire paths'
    // accounting stays balanced.
    {
        let seat = get_mut_helper_seat(dynamic, args.maker_seat_index).get_mut_value();
        seat.open_lend_count = seat
            .open_lend_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    let seq = fixed.order_sequence_number;
    // `checked_add` (not `wrapping_add`): see `matched_loan_sequence`.
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
        // Unbounded ask sentinel — "quote all idle".
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
    // Vault asks are non-expiring; only the curator removes them via
    // cancel_order_for_risk_profile. `share_price_snapshot` is unused
    // on the vault path (no per-seat decrement-by-snapshot ever runs).
    // `principal_atoms = u64::MAX` is the unbounded-ask sentinel — the
    // matching engine ignores it for sizing.
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

    // Only the asks tree holds resting orders — a borrower bid never
    // rests.
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

/// Remove a resting order from the book, reversing its seat
/// encumbrance.
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
    // Vault risk-profile asks skip encumber at place time
    // (`rest_vault_ask` is a pure insert) — bookkeeping happens via the
    // profile's RiskProfile.encumbered_in_orders_atoms instead. Mirror
    // that here by skipping unencumber for vault-owned seats; otherwise
    // the checked_sub on debt_encumbered_shares would error and vault
    // asks would become un-cancellable.
    let owner_kind: u8 = {
        let seat = get_helper_seat(dynamic, order.trader_seat_index).get_value();
        seat.owner_kind
    };
    if owner_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE {
        let seat = get_mut_helper_seat(dynamic, order.trader_seat_index).get_mut_value();
        seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
        remove_order_from_tree_and_free(fixed, dynamic, order_index);
        return Ok(());
    }
    // Decrement at the order's recorded snapshot — byte-symmetric with
    // the encumber that ran when the order was placed.
    let snapshot = order.share_price_snapshot();
    unencumber_for_order(
        dynamic,
        order.trader_seat_index,
        // Every resting order is an Ask — the structural invariant is
        // the source of truth, not the raw `order.side` byte (a corrupt
        // byte must not drive control flow).
        Side::Ask,
        atoms_to_shares_at_snapshot(order.principal_atoms, snapshot),
        atoms_to_shares_at_snapshot(order.collateral_atoms, snapshot),
    )?;
    remove_order_from_tree_and_free(fixed, dynamic, order_index);
    Ok(())
}

// ────────── ConvertP2PoolToFixed: walk asks and emit Fixed nodes ──────────

/// Args for `match_p2pool_residual_against_asks`. Borrower holds an
/// existing P2Pool loan and is refinancing the variable-rate residual
/// into one or more fixed-rate loans by walking the vault risk-profile
/// asks tree. Each cross emits a regular Fixed `MatchedLoan` queue node
/// — the cranker promotes each into a fresh `LoanFixed` PDA via
/// `process_matched_loan` (the risk-profile variant).
///
/// Pricing mirrors `match_order`'s rate stamping: the new Fixed loan's
/// `lender_rate_bps == ask.rate_bps` and
/// `borrower_rate_bps == max(max_acceptable_rate_bps, ask.rate_bps +
/// fee_floor_bps)`. The borrower's `max_acceptable_rate_bps` acts as the
/// "bid rate" — a ceiling on the LENDER rate; the protocol floor is
/// added on top so `borrower_rate >= lender_rate`.
pub struct MatchP2PoolRefinanceArgs {
    pub market_pubkey: Pubkey,
    /// Original P2Pool loan's borrower seat. Stamped on each new Fixed
    /// MatchedLoan as `borrower_seat_index`.
    pub borrower_seat_index: DataIndex,
    /// Cap on convertible principal — the loan's live marginfi
    /// liability. The matcher stops at this limit so the caller can't
    /// overshoot the live P2Pool debt.
    pub principal_cap_atoms: u64,
    /// Original P2Pool loan's full collateral — split pro-rata across
    /// crosses. matched_collateral_per_cross =
    /// `loan_collateral × matched_principal / principal_cap`.
    pub loan_collateral_atoms: u64,
    /// Original P2Pool loan's borrower-collateral place-time snapshot.
    /// Propagated onto each new Fixed loan so the borrower's seat
    /// decrement at full repay / liquidation stays byte-symmetric with
    /// the original encumber.
    pub borrower_collateral_share_price_snapshot_fp48: u128,
    /// Term remaining on the P2Pool loan (`matures_at - now`). New
    /// Fixed loans are stamped with this as their `term_seconds`.
    pub term_remaining_seconds: u32,
    /// Borrower-supplied ceiling on the crossed ask rate. Acts as the
    /// "bid rate" for the refinance.
    pub max_acceptable_rate_bps: u16,
    /// `market.fee_config.protocol_fee_bps_floor` — added on top of the
    /// lender rate when the converted loan's `borrower_rate` is stamped.
    pub fee_floor_bps: u16,
    pub now_unix_ts: i64,
    // ─── Per-cross LTV-gate inputs ───
    //
    // The refinance matcher must run the SAME per-cross LTV gates the
    // primary `match_order` runs — both the marginfi-init-weight
    // required-collateral check and the crossed profile's curator-set
    // `max_ltv_bps` cap. Without these a borrower can refinance variable
    // debt into a conservative low-LTV curator's quote at an LTV that
    // curator never agreed to. The caller snapshots these from the
    // debt/collateral banks' oracles + init weights.
    /// fp48 USD-per-token from the debt bank's oracle.
    pub debt_oracle_price_fp48: u128,
    /// fp48 USD-per-token from the collateral bank's oracle.
    pub collateral_oracle_price_fp48: u128,
    /// fp48 borrower-side weight from the debt bank's
    /// `liability_weight_init`.
    pub debt_liability_weight_init_fp48: u128,
    /// fp48 lender-side weight from the collateral bank's
    /// `asset_weight_init`.
    pub collateral_asset_weight_init_fp48: u128,
    /// `market.fee_config.ltv_buffer_bps` — safety margin on the
    /// marginfi-init-weight required-collateral check.
    pub ltv_buffer_bps: u16,
    /// Debt mint decimals — fed into the decimal normalization of
    /// `get_required_quote_collateral_to_back_debt`.
    pub debt_mint_decimals: u8,
    /// Collateral mint decimals — see `debt_mint_decimals`.
    pub collateral_mint_decimals: u8,
}

/// One crossed vault ask, captured so the convert processor can run the
/// per-profile `encumbered_in_orders → deployed` bookkeeping after the
/// consolidated vault-migration CPI.
#[derive(Clone, Copy)]
pub struct P2PoolRefinanceCross {
    /// Vault risk-profile id of the crossed ask's maker seat.
    pub lender_profile_id: u8,
    /// `lender_rate_bps` stamped on the converted Fixed loan (the ask
    /// rate). The processor folds `principal × rate` into the profile's
    /// weighted-rate aggregates.
    pub lender_rate_bps: u16,
    /// Atoms crossed against this profile.
    pub filled_principal_atoms: u64,
}

#[derive(Default)]
pub struct MatchP2PoolRefinanceResult {
    /// Sum of `matched_principal` over all crosses.
    pub total_filled_principal_atoms: u64,
    /// Sum of pro-rata collateral over all crosses. Caller subtracts
    /// from the loan's `collateral_atoms` field.
    pub total_filled_collateral_atoms: u64,
    pub num_fills: u32,
    /// Per-cross detail — the convert processor replays each entry to
    /// run the crossed profile's vault bookkeeping.
    pub crosses: Vec<P2PoolRefinanceCross>,
}

/// Walk the asks tree and cross compatible vault risk-profile asks to
/// convert a P2Pool residual into fixed-rate loans. Every resting ask
/// is a vault risk-profile quote. Mirrors `match_order`:
///
///   - For each resting ask, size the cross at
///     `min(remaining_residual, profile_idle)` where `profile_idle` is
///     read live off the vault `RiskProfile`.
///   - On accept, bump `RiskProfile.encumbered_in_orders_atoms` inline
///     (the cranker-race guard — identical to `match_order`).
///   - Rate rule: cross when `ask_rate <= max_acceptable_rate_bps`;
///     stamp `lender_rate = ask_rate`, `borrower_rate =
///     max(max_acceptable_rate_bps, ask_rate + fee_floor_bps)`.
///   - Term rule: cross only when `ask.term_seconds >=
///     term_remaining_seconds`.
///   - One cross per maker — the standing vault ask is never removed.
///   - Emit a Fixed `MatchedLoan` per cross.
///
/// The caller (`process_convert_p2pool_to_fixed`) handles the
/// consolidated `marginfi.withdraw → marginfi.repay_atoms` CPI pair and
/// the loan-body update / close at the end of the matching pass.
///
/// `vault_ai` is the GlobalVault account — required, because every ask
/// is a vault risk-profile quote. When it is `None` the helper crosses
/// nothing (it cannot size a vault ask without the profile's idle
/// pool).
///
/// Self-match prevention: if a maker's seat == args.borrower_seat_index
/// the helper errors `SelfMatchForbidden`. Same invariant the primary
/// matching engine enforces.
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
            // Vault asks are placed non-expiring; reaching this branch
            // means corrupted state. Walk past defensively rather than
            // removing — only the curator removes risk-profile orders.
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
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

        // Rate gate. Asks tree is rate-sorted in the taker's favour —
        // once the best ask exceeds `max_acceptable_rate_bps`, no later
        // (worse-rate) ask satisfies it either. Break.
        if maker.rate_bps > args.max_acceptable_rate_bps {
            break;
        }

        // Term gate. Rate ordering is independent of term, so a later
        // (worse-rate) maker with a longer term may still cross — walk
        // to the next maker rather than breaking.
        if maker.term_seconds < args.term_remaining_seconds {
            current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
            continue;
        }

        // ─────────── Match-time vault gate (unbounded ask) ───────────
        //
        // Identical to the inline gate in `match_order`: a vault ask is
        // an UNBOUNDED standing quote. Each cross is sized at
        // `min(remaining_principal, profile_idle)` where `profile_idle =
        // total_principal - deployed - encumbered` read live off the
        // vault. On accept we bump `RiskProfile.encumbered_in_orders_atoms`
        // synchronously so subsequent crosses (this loop or a later tx)
        // see the lock — the cranker-race guard.
        let matched_principal: u64;
        let lender_profile_id: u8;
        // Curator-set lender LTV cap, read live from the crossed
        // maker's `RiskProfile`. 0 means "no profile cap beyond the
        // marginfi-init weights". Captured under the same vault borrow
        // that reads `profile_idle`, mirroring `match_order`.
        let mut profile_max_ltv_bps: u16 = 0;
        {
            let lender_seat = *get_helper_seat(dynamic, maker.trader_seat_index).get_value();
            debug_assert_eq!(
                lender_seat.owner_kind,
                crate::state::OWNER_KIND_RISK_PROFILE
            );
            let vault_ai_ref = match vault_ai {
                Some(v) => v,
                None => {
                    // No vault account in scope — cannot size a vault
                    // ask. Skip the maker defensively.
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
                // Nothing to lend — skip this maker, keep it resting.
                current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                continue;
            }
            matched_principal = remaining_principal.min(profile_idle);

            // ─── Per-cross LTV gates (mirror `match_order`) ───
            //
            // Size the pro-rata collateral for this cross, then run the
            // SAME two gates `match_order` runs per cross:
            //   (a) the marginfi-init-weight required-collateral check,
            //   (b) the crossed profile's curator-set `max_ltv_bps` cap.
            // A cross that breaches the profile cap SKIPS the maker
            // (continue to next) — and crucially this runs BEFORE the
            // `encumbered_in_orders_atoms` bump below, so a rejected
            // maker's profile is left untouched.
            let matched_collateral_for_gate: u64 = ((args.loan_collateral_atoms as u128)
                .checked_mul(matched_principal as u128)
                .ok_or(ProgramError::ArithmeticOverflow)?
                / args.principal_cap_atoms as u128)
                as u64;
            {
                // (a) marginfi-init-weight required-collateral check.
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

                // (b) profile `max_ltv_bps` cap. 0 means no cap beyond
                // the marginfi-init weights — skip. A breach SKIPS the
                // maker rather than hard-failing the whole convert: a
                // later, less conservative profile may still cross.
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
                            /*ltv_buffer_bps=*/ 0,
                            args.debt_mint_decimals,
                            args.collateral_mint_decimals,
                        )?;
                    if matched_collateral_for_gate < required_at_profile_cap {
                        // Cross would breach the curator's LTV cap —
                        // skip this maker, leave its profile untouched.
                        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
                        continue;
                    }
                }
            }

            // Accept: bump profile.encumbered_in_orders_atoms inline.
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

        // Pro-rata collateral split. principal_cap_atoms is the loan's
        // live debt; matched_principal is this chunk's share. Non-zero
        // divisor — the caller enforces `principal_cap_atoms > 0`.
        let matched_collateral: u64 = ((args.loan_collateral_atoms as u128)
            .checked_mul(matched_principal as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / args.principal_cap_atoms as u128) as u64;

        // Risk-profile asks are unbounded standing quotes — never
        // "fully filled" by a cross and never removed by matching. Only
        // the curator removes them. Leave the resting order intact.
        // Vault makers carry no per-seat encumbrance: the
        // profile.encumbered_in_orders bump above is the only maker-side
        // bookkeeping.
        let maker_snapshot = maker.share_price_snapshot();

        // Rate stamping (mirrors `match_order`): the lender earns
        // exactly their ask rate; the borrower pays
        // `max(max_acceptable_rate, ask_rate + fee_floor)`. This
        // structurally guarantees `borrower_rate >= lender_rate`.
        let lender_rate = maker.rate_bps;
        // See `match_order`: compute `ask_rate + fee_floor` in u32
        // and hard-fail on a `u16` overflow rather than `saturating_add`-
        // clamping and under-collecting the protocol spread floor.
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

        // Insert a Fixed MatchedLoan node. Stamped `VAULT_PRESETTLED`:
        // the convert processor migrates the vault principal and runs
        // the profile bookkeeping inline, so the cranker
        // (`process_matched_loan`) skips `do_vault_settle`.
        //
        // `origination_atoms = 0` is INTENTIONAL. A convert is a
        // *refinance* of an existing loan, not a fresh borrow. The
        // borrower already paid `origination_bps` when the principal
        // was first borrowed — the original P2Pool `MatchedLoan` (see
        // the P2Pool-fallback branch in `match_borrower_bid`) and a
        // fresh orderbook cross both charge origination at place-order
        // time. Charging it again on conversion would bill origination
        // TWICE for the same borrowed principal. There is no
        // origination "dodge": a borrower cannot reach this code path
        // without having already been charged origination on the
        // P2Pool loan being converted.
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
        node.loan_type = 0; // Fixed
                            // The refinance maker is always a vault risk-profile ask;
                            // stamp `VAULT_LENDER` alongside `VAULT_PRESETTLED` so the
                            // cranker routes on the match-time record.
        node.flags = crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED
            | crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER;
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
        // `checked_add` (not `wrapping_add`): `matched_loan_sequence`
        // feeds the loan PDA seed and the `MatchedLoan` tree key. A wrap
        // at u64::MAX would alias an existing loan's address/key — hard
        // fail instead (u64 exhaustion is unreachable in practice).
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

        // One cross per maker — the vault ask stays resting, so always
        // advance to the in-order predecessor (next-best maker by rate).
        current_maker_index = next_maker_index(fixed, dynamic, current_maker_index);
    }

    Ok(MatchP2PoolRefinanceResult {
        total_filled_principal_atoms: total_filled_principal,
        total_filled_collateral_atoms: total_filled_collateral,
        num_fills,
        crosses,
    })
}
