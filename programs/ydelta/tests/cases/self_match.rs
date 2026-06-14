//! Self-match prevention.
//!
//! T-2: a literal self-match is not expressible from any instruction —
//! the only resting orders are vault sub-vault asks (seat owner_kind
//! = `SUB_VAULT`); the only takers are borrower IOC bids (seat
//! owner_kind = `USER`). A maker seat and a taker seat can therefore
//! never be the same `ClaimedSeat` via the public API.
//!
//! The seat-equality skip in `match_order` (v1 D9: skip, don't abort)
//! is defense-in-depth against future state corruption; the live D9
//! gate compares OWNERS (bid wallet vs ask sub-vault curator) and is
//! covered behaviorally in `self_cross.rs`. This module pins THE
//! STRUCTURAL invariant that makes the seat-level case unreachable
//! from instructions:
//!   - The `ClaimedSeat::Ord` keys on `(owner_kind, owner, sub_vault_id)`
//!     so user seats and sub-vault seats are distinct tree entries.
//!   - The matching engine resolves taker_seat from a USER lookup and
//!     maker_seat from a resting order (which only SUB_VAULT seats
//!     own, per M-5's runtime invariant in `rest_order`).
//! If either invariant ever breaks, the existing M-5 `require!` in
//! the match loop will fire BEFORE the self-match check, which means
//! self-match becomes reachable only through deeper corruption.

use ydelta::state::claimed_seat::{ClaimedSeat, OWNER_KIND_SUB_VAULT, OWNER_KIND_USER};
use solana_program::pubkey::Pubkey;

#[test]
fn user_and_sub_vault_seats_with_same_owner_are_distinct() {
    let owner = Pubkey::new_unique();
    let user_seat = ClaimedSeat::new_empty(owner, OWNER_KIND_USER, 0);
    let rp_seat = ClaimedSeat::new_empty(owner, OWNER_KIND_SUB_VAULT, 0);
    assert_ne!(
        user_seat, rp_seat,
        "USER and SUB_VAULT seats with identical owner pubkey must be distinct \
         (matching engine relies on this for self-match safety)",
    );
}

#[test]
fn sub_vault_seats_distinct_by_sub_vault_id() {
    let owner = Pubkey::new_unique();
    let s0 = ClaimedSeat::new_empty(owner, OWNER_KIND_SUB_VAULT, 0);
    let s1 = ClaimedSeat::new_empty(owner, OWNER_KIND_SUB_VAULT, 1);
    assert_ne!(s0, s1);
}
