//! Per-user account that mirrors what the wallet has touched across the
//! protocol. The header is followed by a free-list-backed dynamic region
//! that holds three node types: `VaultPosition` (the user's shares in a
//! `GlobalVault` sub-vault), `MarketPosition` (a cached view of the
//! user's market seat balances), and `UserLoanRef` (an open-loan
//! pointer). All three are payload-size identical so they share the
//! same free list.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
#[cfg(debug_assertions)]
use hypertree::HyperTreeValueIteratorTrait;
use hypertree::{
    get_helper, get_mut_helper, DataIndex, Get, HyperTreeReadOperations, RBNode, RedBlackTree,
    RedBlackTreeReadOnly, NIL,
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

/// PDA seed prefix for user accounts. Full seeds: `[USER_ACCOUNT_SEED, owner]`.
pub const USER_ACCOUNT_SEED: &[u8] = b"user";

/// Derives the user-account PDA for `owner` and its bump.
pub fn user_account_pda(owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[USER_ACCOUNT_SEED, owner.as_ref()], &crate::id())
}

/// Fixed-size user-account header. Holds the three tree roots into the
/// dynamic region (vault positions, market positions, open loans), the
/// free-list head, and per-tree counts.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct UserAccountFixed {
    /// Layout tag; must equal [`USER_ACCOUNT_FIXED_DISCRIMINANT`].
    pub discriminator: u64,
    /// Wallet that owns this account.
    pub owner: Pubkey,

    /// Root of the [`VaultPosition`] tree.
    pub vault_positions_root_index: DataIndex,
    /// Root of the [`MarketPosition`] tree.
    pub market_positions_root_index: DataIndex,
    /// Root of the [`UserLoanRef`] tree.
    pub open_loans_root_index: DataIndex,
    /// Head of the free-block list.
    pub free_list_head_index: DataIndex,
    /// Total bytes currently allocated in the dynamic region.
    pub num_bytes_allocated: u32,

    /// Number of live `VaultPosition` nodes.
    pub vault_position_count: u16,
    /// Number of live `MarketPosition` nodes.
    pub market_position_count: u16,
    /// Number of live `UserLoanRef` nodes.
    pub open_loan_count: u32,

    /// PDA bump.
    pub bump: u8,
    /// Account layout version; checked by loaders.
    pub version: u8,
    _padding: [u8; 2],

    _reserved: [u64; 7],
}
const_assert_eq!(size_of::<UserAccountFixed>(), USER_ACCOUNT_FIXED_SIZE);
const_assert_eq!(size_of::<UserAccountFixed>() % 8, 0);

impl UserAccountFixed {
    /// Build a fresh user-account header for `owner` with empty trees
    /// and an empty free list.
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

    /// `true` when the dynamic region has at least one block on the
    /// free list.
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

/// Padding type whose size matches
/// [`super::constants::USER_ACCOUNT_FREE_LIST_BLOCK_SIZE`]; used as the
/// `FreeList` payload so free blocks occupy the same on-disk size as
/// live nodes.
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

/// User's slice of a single sub-vault inside a `GlobalVault`. Mirrors
/// the depositor seat held inside the vault account so off-chain UIs can
/// read shares without loading the vault.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct VaultPosition {
    /// Global vault pubkey this position is inside.
    pub vault: Pubkey,
    /// Sub-vault id within the vault.
    pub sub_vault_id: u8,
    _pad0: [u8; 15],
    /// User's share balance in the profile.
    pub shares: u128,
    /// Snapshot of the profile's `cumulative_supply_yield_index_scaled`
    /// at last touch.
    pub snapshot_supply_yield_index_scaled: u128,
    /// Snapshot of the profile's `cumulative_delta_yield_index_scaled`
    /// at last touch.
    pub snapshot_delta_yield_index_scaled: u128,
    /// Unix-ts of the last update.
    pub last_updated_unix: i64,
    _padding: [u8; 8],

    _reserved: [u64; 4],
}
const_assert_eq!(size_of::<VaultPosition>(), VAULT_POSITION_SIZE);
const_assert_eq!(size_of::<VaultPosition>() % 8, 0);

impl Ord for VaultPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.vault.cmp(&other.vault) {
            std::cmp::Ordering::Equal => self.sub_vault_id.cmp(&other.sub_vault_id),
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
        self.vault == other.vault && self.sub_vault_id == other.sub_vault_id
    }
}
impl Eq for VaultPosition {}
impl Get for VaultPosition {}

impl std::fmt::Display for VaultPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let shares: u128 = self.shares;
        write!(
            f,
            "VaultPosition({},{})={}",
            self.vault, self.sub_vault_id, shares
        )
    }
}

impl VaultPosition {
    /// Build a position with identity fields set and balance fields
    /// zeroed; equality only checks `(vault, sub_vault_id)`.
    pub fn new_empty(vault: Pubkey, sub_vault_id: u8) -> Self {
        Self {
            vault,
            sub_vault_id,
            ..Default::default()
        }
    }
}

/// Cached mirror of a user's seat balances in a specific market.
/// Refreshed by the seat-side update paths and by `SyncMarketPosition`.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MarketPosition {
    /// Market pubkey this position tracks.
    pub market: Pubkey,
    /// User's seat index inside the market account.
    pub seat_index_in_market: DataIndex,
    _pad0: [u8; 12],
    /// Mirror of the seat's `debt_withdrawable_shares`.
    pub debt_withdrawable_shares: u128,
    /// Mirror of the seat's `debt_encumbered_shares`.
    pub debt_encumbered_shares: u128,
    /// Mirror of the seat's `collateral_withdrawable_shares`.
    pub collateral_withdrawable_shares: u128,
    /// Mirror of the seat's `collateral_encumbered_shares`.
    pub collateral_encumbered_shares: u128,

    _reserved_padding: [u64; 4],
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
    /// Build a position with identity fields set and balance fields
    /// zeroed; equality only checks `market`.
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

    /// Overwrites the four balance fields with the live values from
    /// `seat`. Identity fields are untouched.
    pub fn sync_from_seat(&mut self, seat: &super::claimed_seat::ClaimedSeat) {
        self.debt_withdrawable_shares = seat.debt_withdrawable_shares;
        self.debt_encumbered_shares = seat.debt_encumbered_shares;
        self.collateral_withdrawable_shares = seat.collateral_withdrawable_shares;
        self.collateral_encumbered_shares = seat.collateral_encumbered_shares;
    }
}

/// Pointer to an open loan PDA the user is on either side of. Lets the
/// UI enumerate the user's loans without scanning every market.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct UserLoanRef {
    /// Loan PDA.
    pub loan: Pubkey,
    /// Market the loan belongs to.
    pub market: Pubkey,
    /// Principal at origination.
    pub principal_atoms: u64,
    /// Loan start unix-ts.
    pub started_at_unix: i64,
    /// Loan maturity unix-ts.
    pub matures_at_unix: i64,
    /// Side rate from the user's perspective.
    pub rate_bps: u16,
    /// One of the [`LoanRole`] variants.
    pub role: u8,
    /// One of the [`CounterpartyKind`] variants.
    pub counterparty_kind: u8,
    /// Counterparty's sub-vault id when applicable.
    pub counterparty_sub_vault_id: u8,
    _padding: [u8; 19],

    _reserved: [u64; 4],
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

/// User's side of a loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanRole {
    /// User borrows on this loan.
    Borrower = 0,
    /// User lends on this loan.
    Lender = 1,
}

/// Identifies what the counterparty is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CounterpartyKind {
    /// Counterparty is another user wallet.
    UserWallet = 0,
    /// Counterparty is a sub-vault inside a `GlobalVault`.
    GlobalVault = 1,
}

/// Mutable view of the `VaultPosition` tree.
pub type VaultPositionTree<'a> = RedBlackTree<'a, VaultPosition>;
/// Read-only view of the `VaultPosition` tree.
pub type VaultPositionTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, VaultPosition>;
/// Mutable view of the `MarketPosition` tree.
pub type MarketPositionTree<'a> = RedBlackTree<'a, MarketPosition>;
/// Read-only view of the `MarketPosition` tree.
pub type MarketPositionTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, MarketPosition>;
/// Mutable view of the open-loan tree.
pub type OpenLoanTree<'a> = RedBlackTree<'a, UserLoanRef>;
/// Read-only view of the open-loan tree.
pub type OpenLoanTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, UserLoanRef>;

/// Owned user-account view backed by a `Vec<u8>` dynamic tail (tests).
pub type UserAccountValue = DynamicAccount<UserAccountFixed, Vec<u8>>;
/// Read-only user-account view borrowed from on-chain account bytes.
pub type UserAccountRef<'a> = DynamicAccount<&'a UserAccountFixed, &'a [u8]>;
/// Mutable user-account view borrowed from on-chain account bytes.
pub type UserAccountRefMut<'a> = DynamicAccount<&'a mut UserAccountFixed, &'a mut [u8]>;

/// Returns a shared reference to the `MarketPosition` node at `index`.
pub fn get_helper_market_position(data: &[u8], index: DataIndex) -> &RBNode<MarketPosition> {
    get_helper::<RBNode<MarketPosition>>(data, index)
}
/// Returns a mutable reference to the `MarketPosition` node at `index`.
pub fn get_mut_helper_market_position(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<MarketPosition> {
    get_mut_helper::<RBNode<MarketPosition>>(data, index)
}
/// Returns a shared reference to the `UserLoanRef` node at `index`.
pub fn get_helper_open_loan(data: &[u8], index: DataIndex) -> &RBNode<UserLoanRef> {
    get_helper::<RBNode<UserLoanRef>>(data, index)
}
/// Returns a mutable reference to the `UserLoanRef` node at `index`.
pub fn get_mut_helper_open_loan(data: &mut [u8], index: DataIndex) -> &mut RBNode<UserLoanRef> {
    get_mut_helper::<RBNode<UserLoanRef>>(data, index)
}
/// Returns a shared reference to the `VaultPosition` node at `index`.
pub fn get_helper_vault_position(data: &[u8], index: DataIndex) -> &RBNode<VaultPosition> {
    get_helper::<RBNode<VaultPosition>>(data, index)
}
/// Returns a mutable reference to the `VaultPosition` node at `index`.
pub fn get_mut_helper_vault_position(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<VaultPosition> {
    get_mut_helper::<RBNode<VaultPosition>>(data, index)
}

use hypertree::{FreeList, HyperTreeWriteOperations};

use super::constants::USER_ACCOUNT_BLOCK_PAYLOAD_SIZE;

const _CHECK_USER_ACCOUNT_BLOCK_PAYLOAD: () = {
    assert!(VAULT_POSITION_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
    assert!(MARKET_POSITION_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
    assert!(USER_LOAN_REF_SIZE == USER_ACCOUNT_BLOCK_PAYLOAD_SIZE);
};

/// Pops a free block from the user-account dynamic region, returning
/// its index (or `NIL` when empty).
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

/// Pushes `index` back onto the user-account free list.
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

/// Grows the user-account dynamic region by one block, pushing it onto
/// the free list. Caller is responsible for first realloc'ing the
/// underlying account.
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

/// Inserts a `MarketPosition` for `(market, seat_index_in_market)` if
/// absent; returns the existing node index on hit. Bumps
/// `market_position_count` on insert.
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

    fixed.market_position_count = fixed
        .market_position_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_market_position_count(fixed, dynamic);
    Ok(order_index)
}

/// Inserts a `VaultPosition` for `(vault, sub_vault_id)` if absent;
/// returns the existing node index on hit. Bumps
/// `vault_position_count` on insert.
pub fn upsert_vault_position(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    vault: Pubkey,
    sub_vault_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = VaultPosition::new_empty(vault, sub_vault_id);
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

    fixed.vault_position_count = fixed
        .vault_position_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_vault_position_count(fixed, dynamic);
    Ok(order_index)
}

/// Removes the `VaultPosition` for `(vault, sub_vault_id)` and returns the
/// freed node index, or `NIL` when not present.
pub fn remove_vault_position(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    vault: Pubkey,
    sub_vault_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = VaultPosition::new_empty(vault, sub_vault_id);
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

    fixed.vault_position_count = fixed
        .vault_position_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_vault_position_count(fixed, dynamic);
    release_address_on_user_account_fixed(fixed, dynamic, idx);
    Ok(idx)
}

#[cfg(debug_assertions)]
fn count_tree_nodes<V>(dynamic: &[u8], root_index: DataIndex) -> usize
where
    V: hypertree::Payload,
{
    let tree = RedBlackTreeReadOnly::<V>::new(dynamic, root_index, NIL);
    tree.iter::<V>().count()
}

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

/// Overwrites the `MarketPosition` at `market_position_index` with the
/// live seat balances from `seat`.
pub fn write_market_position_from_seat(
    dynamic: &mut [u8],
    market_position_index: DataIndex,
    seat: &super::claimed_seat::ClaimedSeat,
) {
    let node = get_mut_helper_market_position(dynamic, market_position_index);
    let mp = node.get_mut_value();
    mp.sync_from_seat(seat);
}

/// Inserts a `UserLoanRef` for `loan_pda`. Returns the existing node
/// index on hit; bumps `open_loan_count` on insert.
#[allow(clippy::too_many_arguments)]
pub fn insert_open_loan(
    fixed: &mut UserAccountFixed,
    dynamic: &mut [u8],
    loan_pda: Pubkey,
    market: Pubkey,
    role: LoanRole,
    counterparty_kind: CounterpartyKind,
    counterparty_sub_vault_id: u8,
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
        counterparty_sub_vault_id,
        _padding: [0; 19],
        _reserved: [0; 4],
    };
    let mut tree = OpenLoanTree::new(dynamic, fixed.open_loans_root_index, NIL);
    tree.insert(order_index, new_ref);
    fixed.open_loans_root_index = tree.get_root_index();
    drop(tree);

    fixed.open_loan_count = fixed
        .open_loan_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_open_loan_count(fixed, dynamic);
    Ok(order_index)
}

/// Removes the `UserLoanRef` for `loan_pda` and returns the freed node
/// index, or `NIL` when not present.
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

    fixed.open_loan_count = fixed
        .open_loan_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    assert_open_loan_count(fixed, dynamic);
    release_address_on_user_account_fixed(fixed, dynamic, idx);
    Ok(idx)
}
