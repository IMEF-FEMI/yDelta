//! Market account layout: a fixed header (`MarketFixed`) followed by a
//! dynamic region of red-black-tree nodes for resting asks, claimed
//! seats, and pre-settled `MatchedLoan` entries. Also defines the
//! per-market `FeeConfig` and the type aliases used to view the tree
//! regions (`Bookside`, `ClaimedSeatTree`, `MatchedLoanTree`).

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use hypertree::{
    get_helper, get_mut_helper, DataIndex, Get, HyperTreeReadOperations, RBNode, RedBlackTree,
    RedBlackTreeReadOnly, NIL,
};
use shank::ShankType;
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::{get_vault_address, MintAccountInfo, YdeltaAccount};

use super::claimed_seat::ClaimedSeat;
use super::constants::{MARKET_FIXED_DISCRIMINANT, MARKET_FIXED_SIZE, MARKET_FREE_LIST_BLOCK_SIZE};
use super::dynamic_account::{DerefOrBorrow, DynamicAccount};
use super::resting_order::RestingOrder;

/// Per-market fee and risk parameters, updated by the market admin via
/// `SetFeeConfig`. The curator fee moved to the sub-vault in v1.
#[repr(C)]
#[derive(Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct FeeConfig {
    /// Minimum borrower-vs-lender spread the protocol enforces.
    pub protocol_fee_bps_floor: u16,
    /// Origination fee in bps, charged on principal at match time.
    pub origination_bps: u16,
    /// Reserved curator split parameter.
    pub curator_split_bps: u16,
    /// Liquidator-keeper bonus in bps.
    pub liquidation_keeper_bps: u16,
    /// Protocol slice of a liquidation in bps.
    pub liquidation_protocol_bps: u16,

    /// Reserved (was `ltv_buffer_bps` — removed in: origination
    /// LTV is sub-vault-only, no market-level top-up).
    _pad_ltv: [u8; 2],
    _padding_for_u32: [u8; 4],

    /// Grace window in seconds added to `matures_at` before settle and
    /// maturity-liquidation become reachable.
    pub grace_period_seconds: u32,
    _padding_tail: [u8; 4],
}
const_assert_eq!(size_of::<FeeConfig>(), 24);
const_assert_eq!(size_of::<FeeConfig>() % 8, 0);

/// Default grace window stamped onto a fresh market (24 hours).
pub const DEFAULT_GRACE_PERIOD_SECONDS: u32 = 86_400;

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            protocol_fee_bps_floor: 0,
            origination_bps: 0,
            curator_split_bps: 0,
            liquidation_keeper_bps: 0,
            liquidation_protocol_bps: 0,
            _pad_ltv: [0; 2],
            _padding_for_u32: [0; 4],
            grace_period_seconds: DEFAULT_GRACE_PERIOD_SECONDS,
            _padding_tail: [0; 4],
        }
    }
}

/// Padding type whose size equals [`super::constants::MARKET_FREE_LIST_BLOCK_SIZE`].
/// Used as the `FreeList` payload type so the market's free-list nodes
/// occupy the same physical block size as live nodes.
#[repr(C, packed)]
#[derive(Default, Copy, Clone, Pod, Zeroable)]
pub struct MarketUnusedFreeListPadding {
    _padding: [u64; 19],
    _padding2: [u32; 1],
}
const_assert_eq!(
    size_of::<MarketUnusedFreeListPadding>(),
    MARKET_FREE_LIST_BLOCK_SIZE
);

/// In-market record of a match that has not yet been turned into a
/// `LoanFixed` PDA. `process_matched_loan` reads one of these, allocates
/// the loan PDA, moves atoms through marginfi, and frees this node.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MatchedLoan {
    /// Per-market match sequence; becomes the loan PDA seed component.
    pub sequence: u64,
    _padding0: [u8; 8],
    /// Marginfi liability-shares assigned to the borrower for this loan
    /// (P2Pool path) or 0 for fixed.
    pub borrower_marginfi_borrow_shares: u128,
    /// Principal the borrower receives.
    pub principal_atoms: u64,
    /// Origination fee captured at match time.
    pub origination_atoms: u64,
    /// Collateral pledged for this match.
    pub collateral_atoms: u64,
    /// Unix-ts the match happened.
    pub matched_at_unix: i64,
    /// Lender's seat index (NIL for P2Pool residuals).
    pub lender_seat_index: hypertree::DataIndex,
    /// Borrower's seat index.
    pub borrower_seat_index: hypertree::DataIndex,
    /// Loan term in seconds.
    pub term_seconds: u32,
    /// Effective borrower rate at match time.
    pub borrower_rate_bps: u16,
    /// Effective lender rate at match time.
    pub lender_rate_bps: u16,
    /// `LoanType` discriminant: 0 = Fixed, 1 = P2Pool.
    pub loan_type: u8,
    /// Match-time flags (`MATCHED_LOAN_FLAG_*`).
    pub flags: u8,

    _pad_before_p5: [u8; 6],

    /// Curator-fee bps captured at match time; carried to `LoanFixed`.
    pub curator_fee_bps_snapshot: u16,
    /// Origination LTV cap stamped from the sub-vault at match.
    pub origination_ltv_bps: u16,
    /// Liquidation trigger stamped from the sub-vault at match;
    /// carried to `LoanFixed` at promotion.
    pub liquidation_ltv_bps: u16,
    _reserved: [u8; 18],

    /// Lender-side share price snapshot used at loan PDA creation.
    pub lender_debt_share_price_snapshot_fp48: crate::math::Fp48,

    /// Borrower-side collateral share price snapshot.
    pub borrower_collateral_share_price_snapshot_fp48: crate::math::Fp48,
}

const_assert_eq!(
    size_of::<MatchedLoan>(),
    super::constants::MATCHED_LOAN_SIZE
);
const_assert_eq!(size_of::<MatchedLoan>() % 16, 0);

/// Match-time flag: the lender is a `SubVault` inside a `GlobalVault`
/// (not a user wallet).
pub const MATCHED_LOAN_FLAG_VAULT_LENDER: u8 = 0b0000_0001;

/// Match-time flag: the matched loan has already had its principal moved
/// through marginfi inside the same instruction (used by
/// `convert_p2pool_to_fixed`).
pub const MATCHED_LOAN_FLAG_VAULT_PRESETTLED: u8 = 0b0000_1000;

impl Ord for MatchedLoan {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sequence.cmp(&other.sequence)
    }
}
impl PartialOrd for MatchedLoan {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for MatchedLoan {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
    }
}
impl Eq for MatchedLoan {}
impl std::fmt::Display for MatchedLoan {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "MatchedLoan#{} {}atoms@{}bps/{}bps {}s",
            self.sequence,
            self.principal_atoms,
            self.borrower_rate_bps,
            self.lender_rate_bps,
            self.term_seconds
        )
    }
}

/// Fixed-size market header. Carries the mints, the SPL vault PDAs, the
/// fee config, the tree roots into the dynamic region, and the marginfi
/// integration accounts the market borrows / lends through.
#[repr(C)]
#[derive(Default, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MarketFixed {
    /// Layout tag; must equal [`MARKET_FIXED_DISCRIMINANT`].
    pub discriminator: u64,

    /// Account-layout version; checked by loaders.
    pub version: u8,
    /// SPL decimals of the debt mint.
    pub debt_mint_decimals: u8,
    /// SPL decimals of the collateral mint.
    pub collateral_mint_decimals: u8,
    /// Bump for the market's debt SPL vault PDA.
    pub debt_vault_bump: u8,
    /// Bump for the market's collateral SPL vault PDA.
    pub collateral_vault_bump: u8,
    _padding1: [u8; 3],

    /// Debt mint pubkey.
    pub debt_mint: Pubkey,
    /// Collateral mint pubkey.
    pub collateral_mint: Pubkey,
    /// SPL token account for debt held by the market.
    pub debt_vault: Pubkey,
    /// SPL token account for collateral held by the market.
    pub collateral_vault: Pubkey,

    /// Protocol-fee tally in marginfi shares (debt-side).
    pub accumulated_protocol_fee_shares: u128,

    /// Monotonic per-market order sequence counter.
    pub order_sequence_number: u64,

    /// Monotonic per-market matched-loan sequence counter; becomes the
    /// loan PDA seed.
    pub matched_loan_sequence: u64,

    /// Total bytes currently allocated in the dynamic region.
    pub num_bytes_allocated: u32,

    /// Root of the bid `Bookside` red-black tree (restored in v1, D6 —
    /// borrower residuals may rest here).
    pub bids_root_index: DataIndex,

    /// Max-index of the bid tree (points at the best — highest-rate —
    /// bid).
    pub bids_best_index: DataIndex,
    /// Root of the ask `Bookside` red-black tree.
    pub asks_root_index: DataIndex,
    /// Max-index of the ask tree (points at the best ask).
    pub asks_best_index: DataIndex,
    /// Root of the claimed-seat red-black tree.
    pub claimed_seats_root_index: DataIndex,

    /// Root of the matched-loan red-black tree.
    pub matched_loans_root_index: DataIndex,
    /// Head of the free-block list for the dynamic region.
    pub free_list_head_index: DataIndex,

    /// Number of claimed seats in this market.
    pub position_count: u32,

    /// Fee and risk parameters.
    pub fee_config: FeeConfig,

    _padding_after_fee: [u8; 4],

    /// Marginfi account through which the market lends.
    pub lender_integration_account: Pubkey,

    /// Marginfi account through which the market sources borrower
    /// liquidity for P2Pool residuals.
    pub borrower_integration_account: Pubkey,

    /// Marginfi debt-side bank pubkey.
    pub debt_lending_pool: Pubkey,

    /// Marginfi collateral-side bank pubkey.
    pub collateral_lending_pool: Pubkey,

    /// Marginfi group pubkey.
    pub marginfi_group: Pubkey,

    /// Signer PDA used for marginfi CPIs on behalf of the market.
    pub market_signer: Pubkey,
    /// Bump for `market_signer`.
    pub market_signer_bump: u8,

    /// Bump for `lender_integration_account`.
    pub lender_integration_account_bump: u8,

    /// Bump for `borrower_integration_account`.
    pub borrower_integration_account_bump: u8,
    _padding_bumps: [u8; 5],

    /// Market admin; signs admin-only instructions on this market.
    pub admin: Pubkey,

    /// Pending market admin (set by `TransferMarketAdmin`,
    /// accepted by `AcceptMarketAdmin`).
    pub pending_admin: Pubkey,

    /// Per-market pause switch. When set, instructions that guard on it
    /// reject with `Paused`.
    pub is_paused: u8,
    _padding_pause: [u8; 7],
}

const_assert_eq!(size_of::<MarketFixed>(), MARKET_FIXED_SIZE);
const_assert_eq!(size_of::<MarketFixed>() % 8, 0);

impl Get for MarketFixed {}

impl MarketFixed {
    /// Build a fresh market header for `(debt_mint, collateral_mint)`
    /// with default fee config, both vault PDAs derived, and empty trees.
    /// Admin and marginfi integration pubkeys are zeroed and patched in
    /// later during `process_create_market`.
    pub fn new_empty(
        debt_mint: &MintAccountInfo,
        collateral_mint: &MintAccountInfo,
        market_key: &Pubkey,
    ) -> Self {
        let (debt_vault, debt_vault_bump) = get_vault_address(market_key, debt_mint.info.key);
        let (collateral_vault, collateral_vault_bump) =
            get_vault_address(market_key, collateral_mint.info.key);
        MarketFixed {
            discriminator: MARKET_FIXED_DISCRIMINANT,
            version: crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            debt_mint_decimals: debt_mint.mint.decimals,
            collateral_mint_decimals: collateral_mint.mint.decimals,
            debt_vault_bump,
            collateral_vault_bump,
            _padding1: [0; 3],
            debt_mint: *debt_mint.info.key,
            collateral_mint: *collateral_mint.info.key,
            debt_vault,
            collateral_vault,
            accumulated_protocol_fee_shares: 0,
            order_sequence_number: 0,
            matched_loan_sequence: 0,
            num_bytes_allocated: 0,
            bids_root_index: NIL,
            bids_best_index: NIL,
            asks_root_index: NIL,
            asks_best_index: NIL,
            claimed_seats_root_index: NIL,
            matched_loans_root_index: NIL,
            free_list_head_index: NIL,
            position_count: 0,
            fee_config: FeeConfig::default(),
            _padding_after_fee: [0; 4],
            lender_integration_account: Pubkey::default(),
            borrower_integration_account: Pubkey::default(),
            debt_lending_pool: Pubkey::default(),
            collateral_lending_pool: Pubkey::default(),
            marginfi_group: Pubkey::default(),
            market_signer: Pubkey::default(),
            market_signer_bump: 0,
            lender_integration_account_bump: 0,
            borrower_integration_account_bump: 0,
            _padding_bumps: [0; 5],
            admin: Pubkey::default(),
            pending_admin: Pubkey::default(),

            is_paused: 0,
            _padding_pause: [0; 7],
        }
    }

    /// `true` when the dynamic region has at least one block on the
    /// free list (caller does not need to expand before allocating).
    pub fn has_free_block(&self) -> bool {
        self.free_list_head_index != NIL
    }
}

impl YdeltaAccount for MarketFixed {
    fn verify_discriminant(&self) -> ProgramResult {
        require!(
            self.discriminator == MARKET_FIXED_DISCRIMINANT,
            ProgramError::InvalidAccountData,
            "Invalid market discriminant actual:{} expected:{}",
            self.discriminator,
            MARKET_FIXED_DISCRIMINANT
        )?;
        Ok(())
    }

    fn verify_version(&self) -> ProgramResult {
        require!(
            self.version == crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            ProgramError::InvalidAccountData,
            "Stale MarketFixed layout: version {} (expected {})",
            self.version,
            crate::state::constants::ACCOUNT_LAYOUT_VERSION
        )?;
        Ok(())
    }
}

/// Owned market view backed by a `Vec<u8>` dynamic tail (tests only).
pub type MarketValue = DynamicAccount<MarketFixed, Vec<u8>>;

/// Read-only market view borrowed from on-chain account bytes.
pub type MarketRef<'a> = DynamicAccount<&'a MarketFixed, &'a [u8]>;

/// Mutable market view borrowed from on-chain account bytes.
pub type MarketRefMut<'a> = DynamicAccount<&'a mut MarketFixed, &'a mut [u8]>;

/// Mutable view of the seat tree.
pub type ClaimedSeatTree<'a> = RedBlackTree<'a, ClaimedSeat>;
/// Read-only view of the seat tree.
pub type ClaimedSeatTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, ClaimedSeat>;
/// Mutable view of the matched-loan tree.
pub type MatchedLoanTree<'a> = RedBlackTree<'a, MatchedLoan>;
/// Read-only view of the matched-loan tree.
pub type MatchedLoanTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, MatchedLoan>;
/// Mutable view of the ask bookside.
pub type Bookside<'a> = RedBlackTree<'a, RestingOrder>;
/// Read-only view of the ask bookside.
pub type BooksideReadOnly<'a> = RedBlackTreeReadOnly<'a, RestingOrder>;

/// Returns a shared reference to the `ClaimedSeat` node at `index`.
pub fn get_helper_seat(data: &[u8], index: DataIndex) -> &RBNode<ClaimedSeat> {
    get_helper::<RBNode<ClaimedSeat>>(data, index)
}
/// Returns a mutable reference to the `ClaimedSeat` node at `index`.
pub fn get_mut_helper_seat(data: &mut [u8], index: DataIndex) -> &mut RBNode<ClaimedSeat> {
    get_mut_helper::<RBNode<ClaimedSeat>>(data, index)
}
/// Returns a shared reference to the `RestingOrder` node at `index`.
pub fn get_helper_order(data: &[u8], index: DataIndex) -> &RBNode<RestingOrder> {
    get_helper::<RBNode<RestingOrder>>(data, index)
}
/// Returns a mutable reference to the `RestingOrder` node at `index`.
pub fn get_mut_helper_order(data: &mut [u8], index: DataIndex) -> &mut RBNode<RestingOrder> {
    get_mut_helper::<RBNode<RestingOrder>>(data, index)
}
/// Returns a shared reference to the `MatchedLoan` node at `index`.
pub fn get_helper_matched_loan(data: &[u8], index: DataIndex) -> &RBNode<MatchedLoan> {
    get_helper::<RBNode<MatchedLoan>>(data, index)
}

/// Sanity-check that `seat_index` resolves to a live entry in the
/// `ClaimedSeatTree` with the expected `owner_kind`. Used by
/// `process_matched_loan` to reject seat indices that point at freed
/// blocks or wrong-kind seats.
pub fn verify_live_seat(
    dynamic: &[u8],
    claimed_seats_root_index: DataIndex,
    seat_index: DataIndex,
    expected_owner_kind: u8,
) -> Result<ClaimedSeat, ProgramError> {
    require!(
        seat_index != NIL,
        crate::program::YdeltaError::IncorrectAccount,
        "MatchedLoan references a NIL seat index"
    )?;
    let seat: ClaimedSeat = *get_helper_seat(dynamic, seat_index).get_value();
    let tree = ClaimedSeatTreeReadOnly::new(dynamic, claimed_seats_root_index, NIL);
    let resolved = tree.lookup_index(&ClaimedSeat::new_empty(
        seat.owner,
        seat.owner_kind,
        seat.sub_vault_id,
    ));
    require!(
        resolved == seat_index,
        crate::program::YdeltaError::IncorrectAccount,
        "MatchedLoan seat index {} does not resolve to a live claimed seat",
        seat_index
    )?;
    require!(
        seat.owner_kind == expected_owner_kind,
        crate::program::YdeltaError::IncorrectAccount,
        "MatchedLoan seat {} has owner_kind {} (expected {})",
        seat_index,
        seat.owner_kind,
        expected_owner_kind
    )?;
    Ok(seat)
}
/// Returns a mutable reference to the `MatchedLoan` node at `index`.
pub fn get_mut_helper_matched_loan(data: &mut [u8], index: DataIndex) -> &mut RBNode<MatchedLoan> {
    get_mut_helper::<RBNode<MatchedLoan>>(data, index)
}

impl<Fixed: DerefOrBorrow<MarketFixed>, Dynamic: DerefOrBorrow<[u8]>>
    DynamicAccount<Fixed, Dynamic>
{
    fn borrow_market(&self) -> MarketRef {
        MarketRef {
            fixed: self.fixed.deref_or_borrow(),
            dynamic: self.dynamic.deref_or_borrow(),
        }
    }

    /// Returns the market's debt mint pubkey.
    pub fn get_debt_mint(&self) -> &Pubkey {
        &self.borrow_market().fixed.debt_mint
    }
    /// Returns the market's collateral mint pubkey.
    pub fn get_collateral_mint(&self) -> &Pubkey {
        &self.borrow_market().fixed.collateral_mint
    }
    /// Returns the market's debt SPL vault pubkey.
    pub fn get_debt_vault(&self) -> &Pubkey {
        &self.borrow_market().fixed.debt_vault
    }
    /// Returns the market's collateral SPL vault pubkey.
    pub fn get_collateral_vault(&self) -> &Pubkey {
        &self.borrow_market().fixed.collateral_vault
    }

    /// `true` when the market's dynamic region has at least one block on
    /// the free list.
    pub fn has_free_block(&self) -> bool {
        self.borrow_market().fixed.free_list_head_index != NIL
    }

    /// Looks up the seat tree by `(owner, OWNER_KIND_USER, 0)` and
    /// returns the node index, or `NIL` when not present.
    pub fn lookup_user_seat_index(&self, owner: &Pubkey) -> DataIndex {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        let tree: ClaimedSeatTreeReadOnly =
            ClaimedSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&ClaimedSeat::new_empty(
            *owner,
            crate::state::claimed_seat::OWNER_KIND_USER,
            0,
        ))
    }

    /// Looks up the seat tree by `(global_vault, OWNER_KIND_SUB_VAULT,
    /// sub_vault_id)` and returns the node index, or `NIL`.
    pub fn lookup_sub_vault_seat_index(
        &self,
        global_vault: &Pubkey,
        sub_vault_id: u16,
    ) -> DataIndex {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        let tree: ClaimedSeatTreeReadOnly =
            ClaimedSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&ClaimedSeat::new_empty(
            *global_vault,
            crate::state::claimed_seat::OWNER_KIND_SUB_VAULT,
            sub_vault_id,
        ))
    }

    /// Returns a read-only view of the ask bookside.
    pub fn get_asks(&self) -> BooksideReadOnly {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        BooksideReadOnly::new(dynamic, fixed.asks_root_index, fixed.asks_best_index)
    }
}
