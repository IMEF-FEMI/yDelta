//! `UserAccount` state used for wallet-level mirrors and dashboard reads.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use hypertree::{
    get_helper, get_mut_helper, DataIndex, Get, HyperTreeReadOperations,
    HyperTreeValueIteratorTrait, RBNode, RedBlackTree, RedBlackTreeReadOnly, NIL,
};
use shank::{ShankAccount, ShankType};
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::YdeltaAccount;

use super::constants::{
    MARKET_POSITION_SIZE, USER_ACCOUNT_FIXED_DISCRIMINANT, USER_ACCOUNT_FIXED_SIZE,
    USER_ACCOUNT_FREE_LIST_BLOCK_SIZE, USER_LOAN_REF_SIZE, VAULT_POSITION_SIZE,
};
use super::dynamic_account::DynamicAccount;

/// PDA seed prefix for `UserAccount` PDAs. Final seeds:
/// `[USER_ACCOUNT_SEED, owner.as_ref()]`.
pub const USER_ACCOUNT_SEED: &[u8] = b"user";

pub fn user_account_pda(owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[USER_ACCOUNT_SEED, owner.as_ref()], &crate::id())
}

// ─────────────────── UserAccountFixed ───────────────────

/// `UserAccountFixed` — 128-byte header. PDA seeds: `[b"user", owner]`.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct UserAccountFixed {
    pub discriminator: u64, // 0..8
    pub owner: Pubkey,      // 8..40

    pub vault_positions_root_index: DataIndex,  // 40..44
    pub market_positions_root_index: DataIndex, // 44..48
    pub open_loans_root_index: DataIndex,       // 48..52
    pub free_list_head_index: DataIndex,        // 52..56
    pub num_bytes_allocated: u32,               // 56..60

    pub vault_position_count: u16,  // 60..62
    pub market_position_count: u16, // 62..64
    pub open_loan_count: u32,       // 64..68

    pub bump: u8,      // 68..69
    pub version: u8,   // 69..70
    _padding: [u8; 2], // 70..72 — align u64 below

    /// Reserved budget. 7 × u64 = 56 bytes; total header = 128.
    _reserved: [u64; 7], // 72..128
}
const_assert_eq!(size_of::<UserAccountFixed>(), USER_ACCOUNT_FIXED_SIZE);
const_assert_eq!(size_of::<UserAccountFixed>() % 8, 0);

impl UserAccountFixed {
    pub fn new_empty(owner: Pubkey, bump: u8) -> Self {
        Self {
            discriminator: USER_ACCOUNT_FIXED_DISCRIMINANT,
            owner,
            vault_positions_root_index: NIL,
            market_positions_root_index: NIL,
            open_loans_root_index: NIL,
            free_list_head_index: NIL,
            num_bytes_allocated: 0,
            vault_position_count: 0,
            market_position_count: 0,
            open_loan_count: 0,
            bump,
            version: crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            _padding: [0; 2],
            _reserved: [0; 7],
        }
    }

    pub fn has_free_block(&self) -> bool {
        self.free_list_head_index != NIL
    }
}

impl Get for UserAccountFixed {}

impl YdeltaAccount for UserAccountFixed {
    fn verify_discriminant(&self) -> ProgramResult {
        require!(
            self.discriminator == USER_ACCOUNT_FIXED_DISCRIMINANT,
            ProgramError::InvalidAccountData,
            "Invalid UserAccount discriminant: {} (expected {})",
            self.discriminator,
            USER_ACCOUNT_FIXED_DISCRIMINANT
        )?;
        Ok(())
    }

    fn verify_version(&self) -> ProgramResult {
        require!(
            self.version == crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            ProgramError::InvalidAccountData,
            "Stale UserAccountFixed layout: version {} (expected {})",
            self.version,
            crate::state::constants::ACCOUNT_LAYOUT_VERSION
        )?;
        Ok(())
    }
}

// ─────────────────── Free-list padding ───────────────────

#[repr(C, packed)]
#[derive(Default, Copy, Clone, Pod, Zeroable)]
pub struct UserAccountUnusedFreeListPadding {
    _padding: [u64; 19],
    _padding2: [u32; 1],
}
const_assert_eq!(
    size_of::<UserAccountUnusedFreeListPadding>(),
    USER_ACCOUNT_FREE_LIST_BLOCK_SIZE
);

// ─────────────────── VaultPosition ───────────────────

/// User's stake in a vault profile. Keyed by `(vault, profile_id)`.
/// u128 fields require 16-byte alignment so the explicit padding
/// lands `shares` at offset 48 (not 40).
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct VaultPosition {
    pub vault: Pubkey,                            // 0..32
    pub profile_id: u8,                           // 32
    _pad0: [u8; 15],                              // 33..48 — align u128 to 16
    pub shares: u128,                             // 48..64
    pub snapshot_supply_yield_index_scaled: u128, // 64..80
    pub snapshot_delta_yield_index_scaled: u128,  // 80..96
    pub last_updated_unix: i64,                   // 96..104
    _padding: [u8; 8],                            // 104..112
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload.
    _reserved: [u64; 4], // 112..144
}
const_assert_eq!(size_of::<VaultPosition>(), VAULT_POSITION_SIZE);
const_assert_eq!(size_of::<VaultPosition>() % 8, 0);

impl Ord for VaultPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.vault.cmp(&other.vault) {
            std::cmp::Ordering::Equal => self.profile_id.cmp(&other.profile_id),
            ord => ord,
        }
    }
}
impl PartialOrd for VaultPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for VaultPosition {
    fn eq(&self, other: &Self) -> bool {
        self.vault == other.vault && self.profile_id == other.profile_id
    }
}
impl Eq for VaultPosition {}
impl Get for VaultPosition {}

impl std::fmt::Display for VaultPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // Pod alignment: read u128 via copy to avoid potential
        // unaligned-ref clippy gripe.
        let shares: u128 = self.shares;
        write!(
            f,
            "VaultPosition({},{})={}",
            self.vault, self.profile_id, shares
        )
    }
}

impl VaultPosition {
    pub fn new_empty(vault: Pubkey, profile_id: u8) -> Self {
        Self {
            vault,
            profile_id,
            ..Default::default()
        }
    }
}

// ─────────────────── MarketPosition ───────────────────

/// Mirror of the user's `ClaimedSeat` in a market. Write-through on
/// every signer-side ix that touches the seat. Keyed by `market`.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MarketPosition {
    pub market: Pubkey,                       // 0..32
    pub seat_index_in_market: DataIndex,      // 32..36
    _pad0: [u8; 12],                          // 36..48 — align u128
    pub debt_withdrawable_shares: u128,       // 48..64
    pub debt_encumbered_shares: u128,         // 64..80
    pub collateral_withdrawable_shares: u128, // 80..96
    pub collateral_encumbered_shares: u128,   // 96..112
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload.
    _reserved_padding: [u64; 4], // 112..144
}
const_assert_eq!(size_of::<MarketPosition>(), MARKET_POSITION_SIZE);
const_assert_eq!(size_of::<MarketPosition>() % 8, 0);

impl Ord for MarketPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.market.cmp(&other.market)
    }
}
impl PartialOrd for MarketPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for MarketPosition {
    fn eq(&self, other: &Self) -> bool {
        self.market == other.market
    }
}
impl Eq for MarketPosition {}
impl Get for MarketPosition {}

impl std::fmt::Display for MarketPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "MarketPosition({},seat={})",
            self.market, self.seat_index_in_market
        )
    }
}

impl MarketPosition {
    pub fn new_empty(market: Pubkey, seat_index_in_market: DataIndex) -> Self {
        Self {
            market,
            seat_index_in_market,
            _pad0: [0; 12],
            debt_withdrawable_shares: 0,
            debt_encumbered_shares: 0,
            collateral_withdrawable_shares: 0,
            collateral_encumbered_shares: 0,
            _reserved_padding: [0; 4],
        }
    }

    /// Copy the four balance fields off a `ClaimedSeat`. Used both
    /// on signer-side write-through and by `sync_market_position`.
    pub fn sync_from_seat(&mut self, seat: &super::claimed_seat::ClaimedSeat) {
        self.debt_withdrawable_shares = seat.debt_withdrawable_shares;
        self.debt_encumbered_shares = seat.debt_encumbered_shares;
        self.collateral_withdrawable_shares = seat.collateral_withdrawable_shares;
        self.collateral_encumbered_shares = seat.collateral_encumbered_shares;
    }
}

// ─────────────────── UserLoanRef ───────────────────

/// Active loan reference for borrower or lender. Keyed by `loan_pda`.
/// `counterparty: Pubkey` is omitted (UI re-derives via the Loan PDA
/// → seat lookup); the saved 32 bytes leave room for forward-compat
/// fields without growing the block.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct UserLoanRef {
    pub loan: Pubkey,                // 0..32 — primary key
    pub market: Pubkey,              // 32..64 — for fast UI grouping
    pub principal_atoms: u64,        // 64..72
    pub started_at_unix: i64,        // 72..80
    pub matures_at_unix: i64,        // 80..88
    pub rate_bps: u16,               // 88..90
    pub role: u8,                    // 90 — 0 = Borrower, 1 = Lender
    pub counterparty_kind: u8,       // 91 — 0 = User wallet, 1 = GlobalVault
    pub counterparty_profile_id: u8, // 92 — only meaningful for vault counterparties
    _padding: [u8; 19],              // 93..112
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload.
    _reserved: [u64; 4], // 112..144
}
const_assert_eq!(size_of::<UserLoanRef>(), USER_LOAN_REF_SIZE);
const_assert_eq!(size_of::<UserLoanRef>() % 8, 0);

impl Ord for UserLoanRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.loan.cmp(&other.loan)
    }
}
impl PartialOrd for UserLoanRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for UserLoanRef {
    fn eq(&self, other: &Self) -> bool {
        self.loan == other.loan
    }
}
impl Eq for UserLoanRef {}
impl Get for UserLoanRef {}

impl std::fmt::Display for UserLoanRef {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "UserLoanRef({}, role={})", self.loan, self.role)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanRole {
    Borrower = 0,
    Lender = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CounterpartyKind {
    UserWallet = 0,
    GlobalVault = 1,
}

// ─────────────────── Tree typedefs + helpers ───────────────────

pub type VaultPositionTree<'a> = RedBlackTree<'a, VaultPosition>;
pub type VaultPositionTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, VaultPosition>;
pub type MarketPositionTree<'a> = RedBlackTree<'a, MarketPosition>;
pub type MarketPositionTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, MarketPosition>;
pub type OpenLoanTree<'a> = RedBlackTree<'a, UserLoanRef>;
pub type OpenLoanTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, UserLoanRef>;

pub type UserAccountValue = DynamicAccount<UserAccountFixed, Vec<u8>>;
pub type UserAccountRef<'a> = DynamicAccount<&'a UserAccountFixed, &'a [u8]>;
pub type UserAccountRefMut<'a> = DynamicAccount<&'a mut UserAccountFixed, &'a mut [u8]>;

pub fn get_helper_market_position(data: &[u8], index: DataIndex) -> &RBNode<MarketPosition> {
    get_helper::<RBNode<MarketPosition>>(data, index)
}
pub fn get_mut_helper_market_position(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<MarketPosition> {
    get_mut_helper::<RBNode<MarketPosition>>(data, index)
}
pub fn get_helper_open_loan(data: &[u8], index: DataIndex) -> &RBNode<UserLoanRef> {
    get_helper::<RBNode<UserLoanRef>>(data, index)
}
pub fn get_mut_helper_open_loan(data: &mut [u8], index: DataIndex) -> &mut RBNode<UserLoanRef> {
    get_mut_helper::<RBNode<UserLoanRef>>(data, index)
}
pub fn get_helper_vault_position(data: &[u8], index: DataIndex) -> &RBNode<VaultPosition> {
    get_helper::<RBNode<VaultPosition>>(data, index)
}
pub fn get_mut_helper_vault_position(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<VaultPosition> {
    get_mut_helper::<RBNode<VaultPosition>>(data, index)
}

// ─────────────────── Free-list helpers ───────────────────
//
// Mirrors the market's `get_free_address_on_market_fixed` and
// `release_address_on_market_fixed`. The three trees share this one
// free list — see UserAccountFixed.free_list_head_index.

use hypertree::{FreeList, HyperTreeWriteOperations};

use super::constants::USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;

const _CHECK_USER_ACCOUNT_BLOCK_PAYLOAD: () = {
    // Compile-time sanity: tree-node payloads all share the same size.
    assert!(VAULT_POSITION_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
    assert!(MARKET_POSITION_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
    assert!(USER_LOAN_REF_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
};

/// Pop a free block off the user-account's shared free list.
pub fn get_free_address_on_user_account_fixed(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    let mut free_list: FreeList<UserAccountUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.free_list_head_index = free_list.get_head();
    free_address
}

/// Return a freed block to the shared free list.
pub fn release_address_on_user_account_fixed(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<UserAccountUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    free_list.add(index);
    fixed.free_list_head_index = free_list.get_head();
}

/// Grow the dynamic region by one block + link a fresh payload-sized
/// chunk onto the free list. Mirrors `market_expand`.
pub fn user_account_expand(fixed: &mut UserAccountFixed, dynamic: &mut [u8]) -> ProgramResult {
    let mut free_list: FreeList<UserAccountUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.free_list_head_index);
    free_list.add(fixed.num_bytes_allocated);
    fixed.num_bytes_allocated = fixed
        .num_bytes_allocated
        .checked_add(super::constants::USER_ACCOUNT_BLOCK_SIZE as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    fixed.free_list_head_index = free_list.get_head();
    Ok(())
}

/// Upsert a `MarketPosition` for `(market, seat_index)`.
/// If a node already exists for `market`, it's a no-op (the caller
/// can mutate the returned mut-ref for balance updates). If missing,
/// allocates a free block, inserts a fresh entry, and bumps
/// `market_position_count`.
///
/// Returns the tree-node index of the upserted entry.
pub fn upsert_market_position(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    seat_index_in_market: DataIndex,
) -> Result<DataIndex, ProgramError> {
    let probe = MarketPosition::new_empty(market, seat_index_in_market);
    let existing_idx: DataIndex = {
        let tree = MarketPositionTreeReadOnly::new(dynamic, fixed.market_positions_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx != NIL {
        return Ok(existing_idx);
    }

    let order_index = get_free_address_on_user_account_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "No free block for MarketPosition — expand user_account"
    )?;
    let mut tree = MarketPositionTree::new(dynamic, fixed.market_positions_root_index, NIL);
    tree.insert(order_index, probe);
    fixed.market_positions_root_index = tree.get_root_index();
    drop(tree);
    // `checked_add` (not `saturating_add`): a saturated count silently
    // desyncs from the tree and the desync assertion is debug-only /
    // stripped from the SBF release build. Hard-fail instead.
    fixed.market_position_count = fixed
        .market_position_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_market_position_count(fixed, dynamic);
    Ok(order_index)
}

/// Upsert a `VaultPosition` for `(vault, profile_id)`. If a node
/// already exists, returns its index (caller mutates `shares` /
/// `snapshot_*` fields directly). If missing, allocates a free block
/// and inserts a fresh zero-share entry.
pub fn upsert_vault_position(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    vault: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = VaultPosition::new_empty(vault, profile_id);
    let existing_idx: DataIndex = {
        let tree = VaultPositionTreeReadOnly::new(dynamic, fixed.vault_positions_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx != NIL {
        return Ok(existing_idx);
    }

    let order_index = get_free_address_on_user_account_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "No free block for VaultPosition — expand user_account"
    )?;
    let mut tree = VaultPositionTree::new(dynamic, fixed.vault_positions_root_index, NIL);
    tree.insert(order_index, probe);
    fixed.vault_positions_root_index = tree.get_root_index();
    drop(tree);
    // `checked_add` — see `upsert_market_position`.
    fixed.vault_position_count = fixed
        .vault_position_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_vault_position_count(fixed, dynamic);
    Ok(order_index)
}

/// Remove a `VaultPosition` for `(vault, profile_id)`. Used by
/// `global_vault_withdraw` when the depositor's shares hit zero.
/// Returns the data-index that was freed, or `NIL` if no entry
/// existed.
pub fn remove_vault_position(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    vault: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = VaultPosition::new_empty(vault, profile_id);
    let idx: DataIndex = {
        let tree = VaultPositionTreeReadOnly::new(dynamic, fixed.vault_positions_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return Ok(NIL);
    }
    let mut tree = VaultPositionTree::new(dynamic, fixed.vault_positions_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.vault_positions_root_index = tree.get_root_index();
    drop(tree);
    // `checked_sub` (not `saturating_sub`): an underflow here means the
    // count already desynced from the tree (a double-remove or a
    // remove-without-insert) — a real accounting bug. Hard-fail.
    fixed.vault_position_count = fixed
        .vault_position_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_vault_position_count(fixed, dynamic);
    release_address_on_user_account_fixed(fixed, dynamic, idx);
    Ok(idx)
}

/// Walk a read-only tree and count its live nodes. Used by the
/// debug-only count-vs-tree-size assertions below — the position/loan
/// `*_count` fields use saturating arithmetic, which would silently
/// absorb a desync from the real tree size. This makes any desync
/// detectable in debug/test builds (the SBF release build strips
/// `debug_assert!`, so this is a development invariant check, not an
/// on-chain gate).
#[cfg(debug_assertions)]
fn count_tree_nodes<V>(dynamic: &[u8], root_index: DataIndex) -> usize
where
    V: hypertree::Payload,
{
    let tree = RedBlackTreeReadOnly::<V>::new(dynamic, root_index, NIL);
    tree.iter::<V>().count()
}

/// Debug-assert the stored `market_position_count` matches the
/// actual `MarketPosition` tree size.
#[cfg(debug_assertions)]
fn assert_market_position_count(fixed: &UserAccountFixed, dynamic: &[u8]) {
    let actual = count_tree_nodes::<MarketPosition>(dynamic, fixed.market_positions_root_index);
    debug_assert_eq!(
        actual, fixed.market_position_count as usize,
        "market_position_count desynced from tree size"
    );
}
#[cfg(not(debug_assertions))]
fn assert_market_position_count(_fixed: &UserAccountFixed, _dynamic: &[u8]) {}

/// Debug-assert the stored `vault_position_count` matches the
/// actual `VaultPosition` tree size.
#[cfg(debug_assertions)]
fn assert_vault_position_count(fixed: &UserAccountFixed, dynamic: &[u8]) {
    let actual = count_tree_nodes::<VaultPosition>(dynamic, fixed.vault_positions_root_index);
    debug_assert_eq!(
        actual, fixed.vault_position_count as usize,
        "vault_position_count desynced from tree size"
    );
}
#[cfg(not(debug_assertions))]
fn assert_vault_position_count(_fixed: &UserAccountFixed, _dynamic: &[u8]) {}

/// Debug-assert the stored `open_loan_count` matches the actual
/// `UserLoanRef` tree size.
#[cfg(debug_assertions)]
fn assert_open_loan_count(fixed: &UserAccountFixed, dynamic: &[u8]) {
    let actual = count_tree_nodes::<UserLoanRef>(dynamic, fixed.open_loans_root_index);
    debug_assert_eq!(
        actual, fixed.open_loan_count as usize,
        "open_loan_count desynced from tree size"
    );
}
#[cfg(not(debug_assertions))]
fn assert_open_loan_count(_fixed: &UserAccountFixed, _dynamic: &[u8]) {}

/// Apply the four balance fields from a `ClaimedSeat` onto the
/// user's `MarketPosition` mirror. Caller has already located /
/// upserted the node.
pub fn write_market_position_from_seat(
    dynamic: &mut [u8],
    market_position_index: DataIndex,
    seat: &super::claimed_seat::ClaimedSeat,
) {
    let node = get_mut_helper_market_position(dynamic, market_position_index);
    let mp = node.get_mut_value();
    mp.sync_from_seat(seat);
}

/// Insert a `UserLoanRef` keyed by `loan_pda`. Idempotent on
/// duplicate insert (returns the existing index).
#[allow(clippy::too_many_arguments)]
pub fn insert_open_loan(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    loan_pda: Pubkey,
    market: Pubkey,
    role: LoanRole,
    counterparty_kind: CounterpartyKind,
    counterparty_profile_id: u8,
    principal_atoms: u64,
    rate_bps: u16,
    started_at_unix: i64,
    matures_at_unix: i64,
) -> Result<DataIndex, ProgramError> {
    let probe = UserLoanRef {
        loan: loan_pda,
        ..Default::default()
    };
    let existing_idx: DataIndex = {
        let tree = OpenLoanTreeReadOnly::new(dynamic, fixed.open_loans_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx != NIL {
        return Ok(existing_idx);
    }

    let order_index = get_free_address_on_user_account_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "No free block for UserLoanRef — expand user_account"
    )?;
    let new_ref = UserLoanRef {
        loan: loan_pda,
        market,
        principal_atoms,
        started_at_unix,
        matures_at_unix,
        rate_bps,
        role: role as u8,
        counterparty_kind: counterparty_kind as u8,
        counterparty_profile_id,
        _padding: [0; 19],
        _reserved: [0; 4],
    };
    let mut tree = OpenLoanTree::new(dynamic, fixed.open_loans_root_index, NIL);
    tree.insert(order_index, new_ref);
    fixed.open_loans_root_index = tree.get_root_index();
    drop(tree);
    // `checked_add` — see `upsert_market_position`.
    fixed.open_loan_count = fixed
        .open_loan_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_open_loan_count(fixed, dynamic);
    Ok(order_index)
}

/// Remove a `UserLoanRef` for `loan_pda`. Used on full repay
/// (borrower side) or full claim (lender side). Returns the
/// data-index that was freed, or `NIL` if no entry existed.
pub fn remove_open_loan(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    loan_pda: Pubkey,
) -> Result<DataIndex, ProgramError> {
    let probe = UserLoanRef {
        loan: loan_pda,
        ..Default::default()
    };
    let idx: DataIndex = {
        let tree = OpenLoanTreeReadOnly::new(dynamic, fixed.open_loans_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return Ok(NIL);
    }
    let mut tree = OpenLoanTree::new(dynamic, fixed.open_loans_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.open_loans_root_index = tree.get_root_index();
    drop(tree);
    // `checked_sub` — see `remove_vault_position`.
    fixed.open_loan_count = fixed
        .open_loan_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_open_loan_count(fixed, dynamic);
    release_address_on_user_account_fixed(fixed, dynamic, idx);
    Ok(idx)
}
