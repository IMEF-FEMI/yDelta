use std::cmp::Ordering;
use std::mem::size_of;

use borsh::{BorshDeserialize, BorshSerialize};
use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use shank::ShankType;
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use super::constants::{NO_EXPIRATION_LAST_VALID_UNIX_TS, RESTING_ORDER_SIZE};

/// Side of the book a `RestingOrder` is sitting on. Bids = borrower
/// demand, asks = lender supply.
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

unsafe impl Zeroable for Side {}
unsafe impl Pod for Side {}

impl Default for Side {
    fn default() -> Self {
        Side::Bid
    }
}

/// Order kind. `Primary` is a fresh borrow/lend; `SecondaryLoanSale`
/// is the lender-exit path that transfers an existing loan to a new
/// lender.
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
pub enum OrderKind {
    Primary = 0,
    SecondaryLoanSale = 1,
}

unsafe impl Zeroable for OrderKind {}
unsafe impl Pod for OrderKind {}

impl Default for OrderKind {
    fn default() -> Self {
        OrderKind::Primary
    }
}

/// Order type. yDelta supports `Limit | ImmediateOrCancel |
/// PostOnly`. The `Reverse` and `Global` variants from manifest are
/// deliberately not carried over.
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

unsafe impl Zeroable for OrderType {}
unsafe impl Pod for OrderType {}

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

/// `RestingOrder` payload. Lives in either the bids or asks tree of a
/// `Market`.
///
/// Sort key on the bid tree: `rate_bps` descending then
/// `sequence_number` ascending (FIFO). On the ask tree: `rate_bps`
/// ascending then `sequence_number` ascending. The `Ord` impl folds
/// `side` so the same payload type slots into both trees with the
/// correct comparator.
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
    pub trader_seat_index: DataIndex,
    /// For `kind == SecondaryLoanSale`, this is the referenced loan's
    /// `matched_loan_sequence` (read off the Loan PDA at placement).
    /// The matching engine carries it onto the MatchedLoan queue node
    /// at cross time so the cranker can derive the loan PDA address
    /// (`[b"loan", market, sequence_le]`). Zero for primary orders.
    /// 32-bit because `matched_loan_sequence` is a per-market u64 in
    /// the on-disk layout but in practice will fit in u32 for the
    /// foreseeable future (4B loans per market).
    pub loan_sequence_snapshot: u32,

    pub sequence_number: u64,
    pub principal_atoms: u64,
    pub collateral_atoms: u64,
    /// **DEPRECATED** — par-exit invariant: for `SecondaryLoanSale`
    /// orders this field always equals `principal_atoms` (no
    /// per-seller pricing). Kept for layout stability. Off-chain
    /// readers should treat as informational; the matching engine
    /// uses `principal_atoms` directly for cash settlement.
    pub asking_price_atoms: u64,
    pub last_valid_unix_ts: i64,

    pub loan_pda: Pubkey,

    pub term_seconds: u32,
    pub rate_bps: u16,
    pub side: Side,
    pub kind: OrderKind,
    pub order_type: OrderType,
    pub flags: u8,

    /// fp48 (`bits / 2^48`) snapshot of the side-relevant bank's
    /// `asset_share_value` at the time of `place_order`. Bid orders
    /// snapshot the collateral bank; ask orders snapshot the debt
    /// bank.
    share_price_snapshot_bytes: [u8; 16],
    /// Borrower-side LTV cap declared at bid placement. Meaningful only
    /// for Bids; Asks ignore (set to 0). Default: marginfi's init LTV
    /// (every bid that satisfies marginfi-init also satisfies this).
    /// The matching loop skips vault makers whose
    /// `RiskProfile.max_ltv_bps < borrower_ltv_bps` — this is the
    /// risk-tier transitivity that lets borrowers and lenders each
    /// declare their LTV preferences explicitly.
    pub borrower_ltv_bps: u16,
    _padding2: [u8; 4],
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload
    /// (matching engine's snapshot fields land on `MatchedLoan`, not
    /// here — this slot is forward-compat).
    _reserved: [u64; 4],
}
// trader_seat_index 4 + _padding1 4 +
// sequence/principal/collateral/asking_price/last_valid 5 × 8 = 40 +
// loan_pda 32 +
// term_seconds 4 + rate_bps 2 + side+kind+order_type+flags 4 +
// share_price_snapshot_bytes 16 + borrower_ltv_bps 2 + _padding2 4 +
// _reserved 32
// = 8 + 40 + 32 + 10 + 16 + 2 + 4 + 32 = 144
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
        borrower_ltv_bps: u16,
    ) -> Self {
        RestingOrder {
            trader_seat_index,
            loan_sequence_snapshot: 0,
            sequence_number,
            principal_atoms,
            collateral_atoms,
            asking_price_atoms: 0,
            last_valid_unix_ts,
            loan_pda: Pubkey::default(),
            term_seconds,
            rate_bps,
            side,
            kind: OrderKind::Primary,
            order_type,
            flags,
            share_price_snapshot_bytes: share_price_snapshot_fp48.to_le_bytes(),
            // Bids: caller-resolved cap (defaults to marginfi-init at
            // place_order). Asks: meaningless — pass 0.
            borrower_ltv_bps: if matches!(side, Side::Bid) {
                borrower_ltv_bps
            } else {
                0
            },
            _padding2: [0; 4],
            _reserved: [0; 4],
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

    /// Construct a `SecondaryLoanSale` Bid for a lender who's
    /// putting their existing loan up for sale. `rate_bps` /
    /// `term_seconds` / `principal_atoms` are snapshots from the loan
    /// (caller is responsible for reading them off the Loan PDA at
    /// placement). `collateral_atoms` is always 0 — the loan's
    /// existing collateral stays attached. `share_price_snapshot_fp48`
    /// is unused for secondary bids (no encumbrance) but stamped for
    /// uniformity.
    #[allow(clippy::too_many_arguments)]
    pub fn new_secondary_bid(
        trader_seat_index: DataIndex,
        loan_sequence_snapshot: u32,
        sequence_number: u64,
        rate_bps: u16,
        term_seconds: u32,
        principal_atoms: u64,
        loan_pda: Pubkey,
        asking_price_atoms: u64,
        last_valid_unix_ts: i64,
        flags: u8,
        share_price_snapshot_fp48: u128,
    ) -> Self {
        RestingOrder {
            trader_seat_index,
            loan_sequence_snapshot,
            sequence_number,
            principal_atoms,
            collateral_atoms: 0,
            asking_price_atoms,
            last_valid_unix_ts,
            loan_pda,
            term_seconds,
            rate_bps,
            side: Side::Bid,
            kind: OrderKind::SecondaryLoanSale,
            order_type: OrderType::Limit,
            flags,
            share_price_snapshot_bytes: share_price_snapshot_fp48.to_le_bytes(),
            // Secondary bids transfer an existing loan; the buyer is the
            // new lender, the borrower keeps their original LTV. No
            // borrower-side declaration applies — leave 0 (sentinel for
            // "no per-maker gate").
            borrower_ltv_bps: 0,
            _padding2: [0; 4],
            _reserved: [0; 4],
        }
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
        // Comparator only well-defined within one side's tree.
        debug_assert_eq!(self.side, other.side);

        // Direction is chosen so that the tree's `max_index` (rightmost
        // node, where hypertree puts the largest-by-Ord) points at the
        // BEST maker — the one the matching engine should hit first.
        // Same convention numa uses (`reference/numa/.../resting_order.rs`).
        //
        // - Bid: best = highest rate (a borrower paying more interest).
        //   Higher rate → larger Ord-key → rightmost.
        // - Ask: best = lowest rate (a lender accepting less interest).
        //   Lower rate → larger Ord-key → rightmost.
        //
        // Within equal rates: FIFO. Earlier sequence number = better
        // priority = larger Ord-key (so earlier orders sit at the right).
        let rate_cmp = match self.side {
            Side::Bid => self.rate_bps.cmp(&other.rate_bps), // ascending
            Side::Ask => other.rate_bps.cmp(&self.rate_bps), // descending
        };
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
            0,
        )
    }

    #[test]
    fn defaults_yield_valid_variants() {
        assert_eq!(Side::default(), Side::Bid);
        assert_eq!(OrderKind::default(), OrderKind::Primary);
        assert_eq!(OrderType::default(), OrderType::Limit);
    }

    #[test]
    fn bid_tree_puts_best_at_max_index() {
        // Best bid = highest rate. Tree's `max_index` (rightmost / largest
        // by Ord) must therefore be the highest-rate bid: high > low.
        let high = order(Side::Bid, 800, 0);
        let low = order(Side::Bid, 600, 1);
        assert!(high > low, "highest-rate bid is the best");

        // FIFO at equal rate: earlier sequence wins. Earlier = better =
        // larger by Ord = ends up at max_index.
        let same_rate_first = order(Side::Bid, 700, 0);
        let same_rate_later = order(Side::Bid, 700, 1);
        assert!(same_rate_first > same_rate_later, "FIFO breaks ties");
    }

    #[test]
    fn ask_tree_puts_best_at_max_index() {
        // Best ask = lowest rate. Tree's `max_index` must be the
        // lowest-rate ask: cheap > pricey by Ord.
        let cheap = order(Side::Ask, 600, 0);
        let pricey = order(Side::Ask, 800, 1);
        assert!(cheap > pricey, "lowest-rate ask is the best");
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
