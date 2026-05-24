use std::cmp::Ordering;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankType;
use solana_program::pubkey::Pubkey;
use static_assertions::const_assert_eq;

use super::constants::CLAIMED_SEAT_SIZE;

pub const OWNER_KIND_USER: u8 = 0;
pub const OWNER_KIND_RISK_PROFILE: u8 = 1;

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

    _padding: [u8; 6],

    _reserved: [u64; 4],
}

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
