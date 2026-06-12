//! `RestingOrder` is the per-ask node that lives in a market's `Bookside`
//! red-black tree. Bids never rest — they are taker-only and matched at
//! `place_order` time — so every resting order is an ask placed by a
//! sub-vault curator. Ord/Eq are tuned so the tree's max-index points
//! at the best (lowest-rate, lowest-sequence) ask.

use std::cmp::Ordering;
use std::mem::size_of;

use borsh::{BorshDeserialize, BorshSerialize};
use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use shank::ShankType;
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError};
use static_assertions::const_assert_eq;

use super::constants::{NO_EXPIRATION_LAST_VALID_UNIX_TS, RESTING_ORDER_SIZE};

#[derive(
    Debug,
    BorshDeserialize,
    BorshSerialize,
    PartialEq,
    Eq,
    Clone,
    Copy,
    ShankType,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[repr(u8)]
/// Order side: `Bid` = borrower side (takers only), `Ask` = lender side
/// (the only side that rests on book).
pub enum Side {
    /// Borrower-side taker. Bids never rest.
    Bid = 0,
    /// Lender-side ask. The only side that ever rests on book.
    Ask = 1,
}

impl Default for Side {
    fn default() -> Self {
        Side::Bid
    }
}

#[derive(
    Debug,
    BorshDeserialize,
    BorshSerialize,
    PartialEq,
    Eq,
    Clone,
    Copy,
    ShankType,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[repr(u8)]
/// Order semantics at placement time.
pub enum OrderType {
    /// Match what's available; rest the residual.
    Limit = 0,
    /// Match what's available; drop the residual (never rests).
    ImmediateOrCancel = 1,
    /// Rest only; reject if the order would cross.
    PostOnly = 2,
}

impl Default for OrderType {
    fn default() -> Self {
        OrderType::Limit
    }
}

/// `true` for order types that may rest on book (everything except IOC).
pub fn order_type_can_rest(order_type: OrderType) -> bool {
    order_type != OrderType::ImmediateOrCancel
}

/// `true` for order types that may take liquidity (everything except
/// `PostOnly`).
pub fn order_type_can_take(order_type: OrderType) -> bool {
    order_type != OrderType::PostOnly
}

/// Node stored in the market's ask `Bookside` tree. Holds the immutable
/// match terms (rate, term, principal, collateral, expiry) plus the
/// trader's seat index for accounting.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RestingOrder {
    /// Index of the placer's claimed seat in the market's seat tree.
    pub trader_seat_index: DataIndex,
    _pad0: [u8; 4],

    /// Monotonic per-market sequence; FIFO tiebreaker for equal-rate
    /// orders.
    pub sequence_number: u64,
    /// Outstanding principal the maker is willing to lend.
    pub principal_atoms: u64,
    /// Borrower-side collateral attached to the order (always 0 for vault
    /// asks since they are quote-only).
    pub collateral_atoms: u64,
    /// Unix-ts after which the order is considered expired; `0` means
    /// never expires.
    pub last_valid_unix_ts: i64,

    /// Loan term in seconds offered by this order.
    pub term_seconds: u32,
    /// Quoted interest rate in basis points.
    pub rate_bps: u16,

    /// One of [`Side::Bid`] or [`Side::Ask`]; vault asks store
    /// [`Side::Ask`].
    pub side: u8,

    /// One of the [`OrderType`] variants encoded as `u8`.
    pub order_type: u8,
    /// Maker-side flag bits (matches the `MATCHED_LOAN_FLAG_*` family).
    pub flags: u8,
    _pad1: [u8; 1],

    share_price_snapshot_bytes: [u8; 16],

    _pad2: [u8; 6],

    _reserved: [u64; 9],
}

const_assert_eq!(size_of::<RestingOrder>(), RESTING_ORDER_SIZE);
const_assert_eq!(size_of::<RestingOrder>() % 8, 0);

impl RestingOrder {
    /// Build a fully-populated `RestingOrder`. The fp48 snapshot is
    /// stored as raw 16 little-endian bytes so the struct stays `Pod`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_primary(
        trader_seat_index: DataIndex,
        sequence_number: u64,
        side: Side,
        order_type: OrderType,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        collateral_atoms: u64,
        last_valid_unix_ts: i64,
        flags: u8,
        share_price_snapshot_fp48: crate::math::Fp48,
    ) -> Self {
        RestingOrder {
            trader_seat_index,
            _pad0: [0; 4],
            sequence_number,
            principal_atoms,
            collateral_atoms,
            last_valid_unix_ts,
            term_seconds,
            rate_bps,
            side: side as u8,
            order_type: order_type as u8,
            flags,
            _pad1: [0; 1],
            share_price_snapshot_bytes: share_price_snapshot_fp48.raw().to_le_bytes(),
            _pad2: [0; 6],
            _reserved: [0; 9],
        }
    }

    /// Decode the stored 16-byte snapshot into an `Fp48`.
    pub fn share_price_snapshot(&self) -> crate::math::Fp48 {
        crate::math::Fp48::from_raw(u128::from_le_bytes(self.share_price_snapshot_bytes))
    }

    /// Re-encode `value` into the 16-byte snapshot field.
    pub fn set_share_price_snapshot(&mut self, value: crate::math::Fp48) {
        self.share_price_snapshot_bytes = value.raw().to_le_bytes();
    }

    /// `true` if the order has an expiry set and `now_unix_ts` is past
    /// it. Treats [`NO_EXPIRATION_LAST_VALID_UNIX_TS`] as "no expiry".
    pub fn is_expired(&self, now_unix_ts: i64) -> bool {
        self.last_valid_unix_ts != NO_EXPIRATION_LAST_VALID_UNIX_TS
            && self.last_valid_unix_ts < now_unix_ts
    }

    /// Subtract `atoms` from `principal_atoms`; errors on underflow.
    pub fn reduce(&mut self, atoms: u64) -> ProgramResult {
        self.principal_atoms = self
            .principal_atoms
            .checked_sub(atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }

    /// Add `atoms` to `principal_atoms`; errors on overflow.
    pub fn increase(&mut self, atoms: u64) -> ProgramResult {
        self.principal_atoms = self
            .principal_atoms
            .checked_add(atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }
}

impl Ord for RestingOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        let rate_cmp = other.rate_bps.cmp(&self.rate_bps);
        match rate_cmp {
            Ordering::Equal => other.sequence_number.cmp(&self.sequence_number),
            ord => ord,
        }
    }
}

impl PartialOrd for RestingOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RestingOrder {
    fn eq(&self, other: &Self) -> bool {
        self.rate_bps == other.rate_bps
            && self.sequence_number == other.sequence_number
    }
}

impl Eq for RestingOrder {}

impl std::fmt::Display for RestingOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{:?}#{} {}@{}bps/{}s",
            self.side, self.sequence_number, self.principal_atoms, self.rate_bps, self.term_seconds
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(side: Side, rate: u16, seq: u64) -> RestingOrder {
        RestingOrder::new_primary(
            0,
            seq,
            side,
            OrderType::Limit,
            rate,
            30,
            100,
            0,
            0,
            0,
            crate::math::Fp48::ONE,
        )
    }

    #[test]
    fn defaults_yield_valid_variants() {
        assert_eq!(Side::default(), Side::Bid);
        assert_eq!(OrderType::default(), OrderType::Limit);
    }

    #[test]
    fn ask_tree_puts_best_at_max_index() {
        let cheap = order(Side::Ask, 600, 0);
        let pricey = order(Side::Ask, 800, 1);
        assert!(cheap > pricey, "lowest-rate ask is the best");

        let same_rate_first = order(Side::Ask, 700, 0);
        let same_rate_later = order(Side::Ask, 700, 1);
        assert!(same_rate_first > same_rate_later, "FIFO breaks ties");
    }

    #[test]
    fn expiry_respects_sentinel() {
        let mut o = order(Side::Bid, 700, 0);
        o.last_valid_unix_ts = NO_EXPIRATION_LAST_VALID_UNIX_TS;
        assert!(!o.is_expired(i64::MAX));
        o.last_valid_unix_ts = 100;
        assert!(o.is_expired(101));
        assert!(!o.is_expired(100));
    }

    #[test]
    fn reduce_then_increase_round_trips() {
        let mut o = order(Side::Ask, 600, 0);
        o.reduce(40).unwrap();
        assert_eq!(o.principal_atoms, 60);
        o.increase(10).unwrap();
        assert_eq!(o.principal_atoms, 70);
    }

    #[test]
    fn order_type_helpers() {
        assert!(order_type_can_rest(OrderType::Limit));
        assert!(!order_type_can_rest(OrderType::ImmediateOrCancel));
        assert!(order_type_can_take(OrderType::Limit));
        assert!(!order_type_can_take(OrderType::PostOnly));
    }

    #[test]
    fn eq_and_cmp_agree_for_all_field_permutations() {
        let trader_seats: &[u32] = &[0, 1];
        let seqs: &[u64] = &[10, 20];
        let rates: &[u16] = &[600, 800];
        let sides = [Side::Ask, Side::Bid];

        let mut orders = Vec::new();
        for &seat in trader_seats {
            for &seq in seqs {
                for &rate in rates {
                    for side in sides {
                        orders.push(RestingOrder::new_primary(
                            seat,
                            seq,
                            side,
                            OrderType::Limit,
                            rate,
                            30,
                            100,
                            0,
                            0,
                            0,
                            crate::math::Fp48::ONE,
                        ));
                    }
                }
            }
        }

        for (i, a) in orders.iter().enumerate() {
            for (j, b) in orders.iter().enumerate() {
                let eq = a == b;
                let cmp_eq = a.cmp(b) == Ordering::Equal;
                assert_eq!(
                    eq, cmp_eq,
                    "Eq/Ord contract violated: orders[{i}]={a} vs orders[{j}]={b} \
                     — a == b is {eq} but cmp == Equal is {cmp_eq}",
                );
                // Also pin reflexivity: every order compares equal to itself.
                if i == j {
                    assert!(eq && cmp_eq, "reflexivity broken for orders[{i}]={a}");
                }
            }
        }
    }
}
