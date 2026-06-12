//! Per-trader seat that lives in a market's red-black tree. Holds the
//! seat owner's withdrawable / encumbered balances on both axes (debt and
//! collateral) plus counters for open lend and borrow positions. One seat
//! per `(owner, owner_kind, sub_vault_id)` triple.

use std::cmp::Ordering;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use shank::ShankType;
use solana_program::pubkey::Pubkey;
use static_assertions::const_assert_eq;

use super::constants::CLAIMED_SEAT_SIZE;

/// `owner_kind` tag for seats owned by an end-user wallet.
pub const OWNER_KIND_USER: u8 = 0;
/// `owner_kind` tag for seats owned by a sub-vault inside a
/// `GlobalVault`. The `owner` field then stores the vault PDA.
pub const OWNER_KIND_SUB_VAULT: u8 = 1;

/// Per-trader seat held in a market's claimed-seat red-black tree.
/// Balance fields are I80F48-scaled (`real_shares × 2^48`) so they match
/// the share-price representation used elsewhere.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct ClaimedSeat {
    /// For user seats, the wallet pubkey; for sub-vault seats, the
    /// global-vault pubkey.
    pub owner: Pubkey,

    /// Debt-axis balance available for withdraw or for posting new asks.
    pub debt_withdrawable_shares: u128,
    /// Debt-axis balance locked behind a resting ask or active loan.
    pub debt_encumbered_shares: u128,
    /// Collateral-axis balance available to back new bids.
    pub collateral_withdrawable_shares: u128,
    /// Collateral-axis balance locked behind active loans.
    pub collateral_encumbered_shares: u128,

    /// Number of loans this seat currently borrows on (one per loan,
    /// independent of seat count).
    pub open_borrow_count: u32,
    /// Number of resting asks plus active loans this seat lends on.
    pub open_lend_count: u32,

    /// One of [`OWNER_KIND_USER`] or [`OWNER_KIND_SUB_VAULT`].
    pub owner_kind: u8,
    /// 0 for user seats; the profile id for sub-vault seats.
    pub sub_vault_id: u8,

    _padding: [u8; 6],

    _reserved: [u64; 4],
}

const_assert_eq!(size_of::<ClaimedSeat>(), CLAIMED_SEAT_SIZE);
const_assert_eq!(size_of::<ClaimedSeat>() % 8, 0);

impl ClaimedSeat {
    /// Build a seat with all balance and counter fields zeroed. Used for
    /// both insert and tree-lookup probes; equality only checks the
    /// identity triple, not the balances.
    pub fn new_empty(owner: Pubkey, owner_kind: u8, sub_vault_id: u8) -> Self {
        ClaimedSeat {
            owner,
            owner_kind,
            sub_vault_id,
            ..Default::default()
        }
    }
}

impl Ord for ClaimedSeat {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.owner_kind.cmp(&other.owner_kind) {
            Ordering::Equal => match self.owner.cmp(&other.owner) {
                Ordering::Equal => self.sub_vault_id.cmp(&other.sub_vault_id),
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
            && self.sub_vault_id == other.sub_vault_id
    }
}

impl Eq for ClaimedSeat {}

impl std::fmt::Display for ClaimedSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}#{}", self.owner, self.sub_vault_id)
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
        let b = ClaimedSeat::new_empty(pk, OWNER_KIND_SUB_VAULT, 5);
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
