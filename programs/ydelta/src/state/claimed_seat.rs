use std::cmp::Ordering;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankType;
use solana_program::pubkey::Pubkey;
use static_assertions::const_assert_eq;

use super::constants::CLAIMED_SEAT_SIZE;

/// `owner_kind` discriminator on `ClaimedSeat`. A user seat is a
/// trader's own position in the market; a risk-profile seat
/// represents a `(global_vault, profile_id)` pair's position.
pub const OWNER_KIND_USER: u8 = 0;
pub const OWNER_KIND_RISK_PROFILE: u8 = 1;

/// A trader's seat in a market. Two flavours share the same wire shape,
/// distinguished by `owner_kind`:
///
/// - **User seats** (`OWNER_KIND_USER`, default) track four marginfi-share
///   buckets: `debt`/`collateral` × `withdrawable`/`encumbered`. The
///   encumbered bucket holds shares pinned to open orders / active loans.
/// - **Risk-profile seats** (`OWNER_KIND_RISK_PROFILE`) represent a `(global_vault, profile_id)`
///   pair's position in this market. Vaults are debt-side only — the
///   `collateral_*_shares` u128 slots simply stay zero. Deployed
///   principal is tracked solely on `RiskProfile.deployed_principal_atoms`.
///
/// Composite tree key: `(owner, risk_profile_id)`. User seats always
/// carry `risk_profile_id = 0`; vault seats vary it. Markets hold
/// both flavours side-by-side in the single `claimed_seats` tree.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct ClaimedSeat {
    pub owner: Pubkey,

    pub debt_withdrawable_shares: u128,
    pub debt_encumbered_shares: u128,
    pub collateral_withdrawable_shares: u128,
    pub collateral_encumbered_shares: u128,

    pub open_borrow_count: u32,
    pub open_lend_count: u32,

    pub owner_kind: u8,
    pub risk_profile_id: u8,
    /// Reserved. 6 bytes of unused padding.
    _padding: [u8; 6],
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload.
    _reserved: [u64; 4],
}
// owner 32 (offset 0..32; Pubkey is align-1, but 32 is 16-aligned so the
// following u128 has no implicit padding) +
// 4 × u128 64 (32..96) +
// 2 × u32 8 (96..104) +
// owner_kind + risk_profile_id + _padding 8 (104..112) +
// _reserved 32 (112..144) = 144
const_assert_eq!(size_of::<ClaimedSeat>(), CLAIMED_SEAT_SIZE);
const_assert_eq!(size_of::<ClaimedSeat>() % 8, 0);

impl ClaimedSeat {
    pub fn new_empty(owner: Pubkey, owner_kind: u8, risk_profile_id: u8) -> Self {
        ClaimedSeat {
            owner,
            owner_kind,
            risk_profile_id,
            ..Default::default()
        }
    }
}

impl Ord for ClaimedSeat {
    fn cmp(&self, other: &Self) -> Ordering {
        // `owner_kind` is the PRIMARY key. User seats and risk-profile
        // seats share the `claimed_seats` tree; a risk-profile seat with
        // `risk_profile_id == 0` would otherwise alias a user seat that
        // owns the same pubkey. Sorting on `owner_kind` first keeps the
        // two flavours in disjoint key spaces.
        match self.owner_kind.cmp(&other.owner_kind) {
            Ordering::Equal => match self.owner.cmp(&other.owner) {
                Ordering::Equal => self.risk_profile_id.cmp(&other.risk_profile_id),
                ord => ord,
            },
            ord => ord,
        }
    }
}

impl PartialOrd for ClaimedSeat {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ClaimedSeat {
    fn eq(&self, other: &Self) -> bool {
        self.owner_kind == other.owner_kind
            && self.owner == other.owner
            && self.risk_profile_id == other.risk_profile_id
    }
}

impl Eq for ClaimedSeat {}

impl std::fmt::Display for ClaimedSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}#{}", self.owner, self.risk_profile_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_seat_starts_zeroed() {
        let seat = ClaimedSeat::new_empty(Pubkey::new_unique(), OWNER_KIND_USER, 0);
        assert_eq!(seat.debt_withdrawable_shares, 0);
        assert_eq!(seat.debt_encumbered_shares, 0);
        assert_eq!(seat.collateral_withdrawable_shares, 0);
        assert_eq!(seat.collateral_encumbered_shares, 0);
        assert_eq!(seat.open_borrow_count, 0);
        assert_eq!(seat.open_lend_count, 0);
    }

    #[test]
    fn ord_orders_by_owner_then_profile() {
        let pk = Pubkey::new_unique();
        let a = ClaimedSeat::new_empty(pk, OWNER_KIND_USER, 0);
        let b = ClaimedSeat::new_empty(pk, OWNER_KIND_RISK_PROFILE, 5);
        assert!(a < b);
        assert_ne!(a, b);
    }

    #[test]
    fn display_includes_profile() {
        let seat = ClaimedSeat::new_empty(Pubkey::default(), OWNER_KIND_USER, 0);
        let s = format!("{}", seat);
        assert!(s.contains("#0"));
    }
}
