//! Self-match prevention.
//!
//! T-2: a literal self-match is not expressible from any instruction —
//! the only resting orders are vault risk-profile asks (seat owner_kind
//! = `RISK_PROFILE`); the only takers are borrower IOC bids (seat
//! owner_kind = `USER`). A maker seat and a taker seat can therefore
//! never be the same `ClaimedSeat` via the public API.
//!
//! The `SelfMatchForbidden` guard in `match_order` is defense-in-depth
//! against future state corruption. This module pins THE STRUCTURAL
//! invariant that makes the guard unreachable from instructions:
//!   - The `ClaimedSeat::Ord` keys on `(owner_kind, owner, profile_id)`
//!     so user seats and risk-profile seats are distinct tree entries.
//!   - The matching engine resolves taker_seat from a USER lookup and
//!     maker_seat from a resting order (which only RISK_PROFILE seats
//!     own, per M-5's runtime invariant in `rest_order`).
//! If either invariant ever breaks, the existing M-5 `require!` in
//! the match loop will fire BEFORE the self-match check, which means
//! self-match becomes reachable only through deeper corruption.

use ydelta::state::claimed_seat::{ClaimedSeat, OWNER_KIND_RISK_PROFILE, OWNER_KIND_USER};
use solana_program::pubkey::Pubkey;

/// T-2: pin that USER and RISK_PROFILE seats with the same `owner`
/// pubkey live as distinct tree entries. Cmp ordering on `owner_kind`
/// first makes them non-aliasing. Therefore no instruction can find
/// one seat as both maker and taker.
#[test]
fn user_and_risk_profile_seats_with_same_owner_are_distinct() {
    let owner = Pubkey::new_unique();
    let user_seat = ClaimedSeat::new_empty(owner, OWNER_KIND_USER, 0);
    let rp_seat = ClaimedSeat::new_empty(owner, OWNER_KIND_RISK_PROFILE, 0);
    assert_ne!(
        user_seat, rp_seat,
        "USER and RISK_PROFILE seats with identical owner pubkey must be distinct \
         (matching engine relies on this for self-match safety)",
    );
}

/// T-2: pin that risk-profile seats with different profile_ids are
/// distinct even when owner + owner_kind match. A vault with multiple
/// risk profiles cannot accidentally cross its own profiles via a self-match.
#[test]
fn risk_profile_seats_distinct_by_profile_id() {
    let owner = Pubkey::new_unique();
    let s0 = ClaimedSeat::new_empty(owner, OWNER_KIND_RISK_PROFILE, 0);
    let s1 = ClaimedSeat::new_empty(owner, OWNER_KIND_RISK_PROFILE, 1);
    assert_ne!(s0, s1);
}
