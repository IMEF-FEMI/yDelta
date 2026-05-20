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

/// Trade side. `Bid` = borrower demand (the IOC taker side; a borrower
/// bid never rests). `Ask` = lender supply (vault risk-profile quotes;
/// the only side that rests on the book).
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

// No `unsafe impl Pod for Side`. A `#[repr(u8)]` enum has invalid
// bit patterns (2..=255), so `Pod` would be unsound. `RestingOrder`
// stores the discriminant as a raw `u8`; use `Side::try_from` to get a
// typed value, which rejects an out-of-range byte instead of UB.

impl Default for Side {
    fn default() -> Self {
        Side::Bid
    }
}

/// Order type. yDelta supports `Limit | ImmediateOrCancel |
/// PostOnly`.
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
    /// Retained only as the zero discriminant so a zeroed `OrderType`
    /// byte (required by the `Pod`/`Zeroable` impls and `Default`)
    /// deserialises to a valid variant. The live order types are
    /// `ImmediateOrCancel` (borrower bids) and `PostOnly` (vault
    /// risk-profile asks).
    Limit = 0,
    ImmediateOrCancel = 1,
    PostOnly = 2,
}

// No `unsafe impl Pod for OrderType` — see the note on `Side`.

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

/// `RestingOrder` payload. Lives in the asks tree of a `Market`.
///
/// The only resting orders are vault risk-profile asks — a borrower bid
/// never rests. Sort key on the ask tree: `rate_bps` ascending then
/// `sequence_number` ascending (FIFO). The `Ord` impl encodes that
/// single comparator.
///
/// `share_price_snapshot_bytes` records the bank's share-price at
/// place-order time so cancel/match can decrement the seat's
/// `*_encumbered_shares` by exactly the same fp48-share quantity that
/// `place_order` added. Stored as `[u8; 16]` (not `u128`) so the
/// struct's 8-byte alignment and the shared 112-byte free-list block
/// stay byte-identical; access via `share_price_snapshot()` /
/// `set_share_price_snapshot()` helpers (bytemuck cast).
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RestingOrder {
    pub trader_seat_index: DataIndex, // @0  4
    _pad0: [u8; 4],                   // @4  4

    pub sequence_number: u64,    // @8  8
    pub principal_atoms: u64,    // @16 8
    pub collateral_atoms: u64,   // @24 8
    pub last_valid_unix_ts: i64, // @32 8

    pub term_seconds: u32, // @40 4
    pub rate_bps: u16,     // @44 2
    /// `Side` discriminant, stored raw. A `#[repr(u8)]` enum has
    /// invalid bit patterns, so `unsafe impl Pod` on the enum is
    /// unsound — a corrupt account byte would be a UB-on-`match` enum.
    /// Stored as `u8`; convert with `Side::try_from` at typed use sites.
    pub side: u8, // @46 1
    /// `OrderType` discriminant, stored raw — see `side`.
    pub order_type: u8, // @47 1
    pub flags: u8,         // @48 1
    _pad1: [u8; 1],        // @49 1

    /// fp48 (`bits / 2^48`) snapshot of the side-relevant bank's
    /// `asset_share_value` at the time of `place_order`. Bid orders
    /// snapshot the collateral bank; ask orders snapshot the debt
    /// bank.
    share_price_snapshot_bytes: [u8; 16], // @50 16
    /// Padding to keep `_reserved` 8-aligned at offset 72.
    _pad2: [u8; 6], // @66 6

    /// Reserved budget. 72 bytes of headroom from the 144-byte payload
    /// (matching engine's snapshot fields land on `MatchedLoan`, not
    /// here — this slot is forward-compat).
    _reserved: [u64; 9], // @72 72
}
// trader_seat_index 4 + _pad0 4 +
// sequence/principal/collateral/last_valid 4 × 8 = 32 +
// term_seconds 4 + rate_bps 2 + side+order_type+flags 3 + _pad1 1 +
// share_price_snapshot_bytes 16 + _pad2 6 +
// _reserved 72
// = 8 + 32 + 4 + 2 + 4 + 16 + 6 + 72 = 144
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

    /// fp48 share-price recorded at `place_order` time. Used by
    /// match/cancel to decrement seat encumbered shares by exactly the
    /// quantity that was added at place time.
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
        // Every resting order is a vault risk-profile Ask — a borrower
        // bid never rests, so there is only one tree (the asks tree)
        // and one comparator.
        //
        // Direction is chosen so that the tree's `max_index` (rightmost
        // node, where hypertree puts the largest-by-Ord) points at the
        // BEST ask — the one the matching engine should hit first.
        //
        // Best ask = lowest rate (a lender accepting less interest).
        // Lower rate → larger Ord-key → rightmost.
        //
        // Within equal rates: FIFO. Earlier sequence number = better
        // priority = larger Ord-key (so earlier orders sit at the right).
        let rate_cmp = other.rate_bps.cmp(&self.rate_bps); // descending
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
        self.trader_seat_index == other.trader_seat_index
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
        // Snapshot share-price = 1.0 fp48 (`1 << 48`) for unit-test
        // ordering invariants — value is irrelevant to the comparator.
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
        // Best ask = lowest rate. Tree's `max_index` must be the
        // lowest-rate ask: cheap > pricey by Ord.
        let cheap = order(Side::Ask, 600, 0);
        let pricey = order(Side::Ask, 800, 1);
        assert!(cheap > pricey, "lowest-rate ask is the best");

        // FIFO at equal rate: earlier sequence wins. Earlier = better =
        // larger by Ord = ends up at max_index.
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
}
