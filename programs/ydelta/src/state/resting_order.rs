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
pub enum Side {
    Bid = 0,
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
pub enum OrderType {
    Limit = 0,
    ImmediateOrCancel = 1,
    PostOnly = 2,
}

impl Default for OrderType {
    fn default() -> Self {
        OrderType::Limit
    }
}

pub fn order_type_can_rest(order_type: OrderType) -> bool {
    order_type != OrderType::ImmediateOrCancel
}

pub fn order_type_can_take(order_type: OrderType) -> bool {
    order_type != OrderType::PostOnly
}

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RestingOrder {
    pub trader_seat_index: DataIndex,
    _pad0: [u8; 4],

    pub sequence_number: u64,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    pub last_valid_unix_ts: i64,

    pub term_seconds: u32,
    pub rate_bps: u16,

    pub side: u8,

    pub order_type: u8,
    pub flags: u8,
    _pad1: [u8; 1],

    share_price_snapshot_bytes: [u8; 16],

    _pad2: [u8; 6],

    _reserved: [u64; 9],
}

const_assert_eq!(size_of::<RestingOrder>(), RESTING_ORDER_SIZE);
const_assert_eq!(size_of::<RestingOrder>() % 8, 0);

impl RestingOrder {
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
        share_price_snapshot_fp48: u128,
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
            share_price_snapshot_bytes: share_price_snapshot_fp48.to_le_bytes(),
            _pad2: [0; 6],
            _reserved: [0; 9],
        }
    }

    pub fn share_price_snapshot(&self) -> u128 {
        u128::from_le_bytes(self.share_price_snapshot_bytes)
    }

    pub fn set_share_price_snapshot(&mut self, value: u128) {
        self.share_price_snapshot_bytes = value.to_le_bytes();
    }

    pub fn is_expired(&self, now_unix_ts: i64) -> bool {
        self.last_valid_unix_ts != NO_EXPIRATION_LAST_VALID_UNIX_TS
            && self.last_valid_unix_ts < now_unix_ts
    }

    pub fn reduce(&mut self, atoms: u64) -> ProgramResult {
        self.principal_atoms = self
            .principal_atoms
            .checked_sub(atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }

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
    // H-2: must mirror `Ord::cmp`'s field set or the `a == b ⇔ cmp == Equal`
    // contract is violated (UB for any std container that assumes it). The
    // ordering key is (rate_bps, sequence_number); `trader_seat_index` is
    // irrelevant because `sequence_number` is already a per-market monotonic
    // counter and uniquely identifies the order in valid state.
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
            1u128 << 48,
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

    /// H-2 regression: `Eq` and `Ord` must agree — `a == b ⇔ a.cmp(b) == Equal`.
    /// Pre-fix, `eq` keyed on `(trader_seat_index, sequence_number)` while
    /// `cmp` keyed on `(rate_bps, sequence_number)`, so the contract was
    /// violable: e.g. two orders with same `(seat, seq)` but different rates
    /// would compare equal yet not cmp-equal — undefined behavior for any
    /// `BTreeSet`/`HashSet`/`std`-algorithm consumer.
    ///
    /// Cross-product the four observable-by-cmp/eq fields over a small set
    /// of values and assert the contract for every ordered pair.
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
                            1u128 << 48,
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
