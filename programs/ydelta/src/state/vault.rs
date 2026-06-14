//! `GlobalVaultFixed` is the per-marginfi-bank lending vault account. Its dynamic
//! tail holds two tree regions: a sub_vault region (`SubVault` nodes)
//! and a node region (`SubVaultDepositorSeat` and
//! `SubVaultOrderRef` nodes), each backed by its own free list. A
//! sub-vault aggregates per-curator parameters and per-tick MTM
//! accounting so individual depositor seats can be split / recombined
//! without re-running marginfi reads.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use hypertree::{
    get_helper, get_mut_helper, DataIndex, FreeList, Get, HyperTreeReadOperations,
    HyperTreeWriteOperations, RBNode, RedBlackTree, RedBlackTreeReadOnly, NIL,
};
use shank::{ShankAccount, ShankType};
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::YdeltaAccount;

use super::constants::{
    GLOBAL_VAULT_FIXED_DISCRIMINANT, GLOBAL_VAULT_FIXED_SIZE, SUB_VAULT_BLOCK_PAYLOAD_SIZE,
    SUB_VAULT_BLOCK_SIZE, VAULT_CLAIMED_SEAT_SIZE, VAULT_NODE_BLOCK_PAYLOAD_SIZE,
    VAULT_NODE_BLOCK_SIZE, VAULT_ORDER_REF_SIZE,
};

/// `SubVault.kind` tag: curator-run pool, multiple depositors.
pub const SUB_VAULT_KIND_POOL: u8 = 0;
/// `SubVault.kind` tag: single-owner sub-vault — the owner is the
/// curator and the only allowed depositor; fee forced to 0.
pub const SUB_VAULT_KIND_PRIVATE: u8 = 1;

/// PDA seed prefix for the per-bank `GlobalVault` account (
/// vaults are keyed by marginfi bank, not mint — the idle MTM and all
/// share accounting are only meaningful against one bank).
pub const VAULT_SEED: &[u8] = b"vault";

/// PDA seed prefix for the global vault's signer PDA (the marginfi CPI
/// signer).
pub const GLOBAL_VAULT_SIGNER_SEED: &[u8] = b"global_vault_signer";

/// PDA seed prefix for the global vault's marginfi integration account.
pub const VAULT_INTEGRATION_SEED: &[u8] = b"vault_integration";

/// PDA seed prefix for the vault's transient SPL staging account.
pub const VAULT_STAGING_SEED: &[u8] = b"global_vault_staging";

/// Derives the global vault PDA and bump for the marginfi `bank`
/// (`[b"vault", bank]`; the mint is cached from `bank.mint` at
/// creation).
pub fn global_vault_pda(bank: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, bank.as_ref()], &crate::id())
}

/// Derives the global vault signer PDA and bump for `vault`.
pub fn global_vault_signer_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_VAULT_SIGNER_SEED, vault.as_ref()], &crate::id())
}

/// Derives the global vault's marginfi integration-account PDA and bump.
pub fn global_vault_integration_account_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_INTEGRATION_SEED, vault.as_ref()], &crate::id())
}

/// Derives the global vault's SPL staging-account PDA and bump.
pub fn global_vault_staging_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_STAGING_SEED, vault.as_ref()], &crate::id())
}

/// Fixed-size global vault header. Holds the mint, the marginfi
/// integration accounts the vault deposits through, the two tree roots
/// (sub_vault region and node region), their free-list heads, and the
/// per-vault admin / pause state.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultFixed {
    /// Layout tag; must equal [`GLOBAL_VAULT_FIXED_DISCRIMINANT`].
    pub discriminator: u64,
    /// Mint this vault wraps.
    pub mint: Pubkey,
    /// Vault admin pubkey.
    pub global_vault_admin: Pubkey,
    /// Marginfi pool the vault's integration account sits in.
    pub integration_pool: Pubkey,
    /// Marginfi account the vault holds idle deposits in.
    pub integration_account: Pubkey,
    /// Signer PDA used for marginfi CPIs on behalf of the vault.
    pub global_vault_signer: Pubkey,

    /// Marginfi bank pubkey for this mint.
    pub lending_pool: Pubkey,

    /// Root of the `SubVault` tree (sub_vault region).
    pub sub_vaults_root_index: DataIndex,
    /// Root of the `SubVaultDepositorSeat` tree (node region).
    pub claimed_seats_root_index: DataIndex,
    /// Root of the `SubVaultOrderRef` tree (node region).
    pub market_orders_root_index: DataIndex,

    /// Head of the sub_vault-region free list.
    pub sub_vault_free_list_head_index: DataIndex,
    /// Head of the node-region free list.
    pub node_free_list_head_index: DataIndex,

    /// Total bytes currently allocated across both dynamic regions.
    pub num_bytes_allocated: u32,

    /// Number of live `SubVault` nodes (u16: Private sub-vault creation
    /// is permissionless, so the old u8 ceiling was griefable).
    pub sub_vault_count: u16,
    /// Bump for `global_vault_signer`.
    pub global_vault_signer_bump: u8,

    /// Account layout version; checked by loaders.
    pub version: u8,
    /// Number of live `SubVaultDepositorSeat` nodes.
    pub claimed_seat_count: u32,
    /// Number of live `SubVaultOrderRef` nodes.
    pub open_order_count: u32,
    _pad1: [u8; 4],

    /// Pending vault admin (set by transfer, accepted by accept).
    pub pending_global_vault_admin: Pubkey,

    _reserved_aggregates: [u64; 4],

    /// Per-vault pause switch.
    pub is_paused: u8,
    _pad2a: [u8; 1],

    /// Next sub-vault id to assign (monotonic, never reused). Starts at
    /// 1; 0 is the invalid/sentinel id.
    pub next_sub_vault_id: u16,
    _pad2: [u8; 4],

    _reserved: [u64; 1],
}
const_assert_eq!(size_of::<GlobalVaultFixed>(), GLOBAL_VAULT_FIXED_SIZE);
const_assert_eq!(size_of::<GlobalVaultFixed>() % 8, 0);

impl GlobalVaultFixed {
    /// Build a fresh vault header for `mint`. Trees and free lists are
    /// empty; `next_sub_vault_id` starts at 1 so the first sub_vault lands
    /// at id 1 (id 0 stays reserved as the sentinel).
    #[allow(clippy::too_many_arguments)]
    pub fn new_empty(
        mint: Pubkey,
        global_vault_admin: Pubkey,
        integration_pool: Pubkey,
        integration_account: Pubkey,
        global_vault_signer: Pubkey,
        global_vault_signer_bump: u8,
        lending_pool: Pubkey,
    ) -> Self {
        Self {
            discriminator: GLOBAL_VAULT_FIXED_DISCRIMINANT,
            mint,
            global_vault_admin,
            integration_pool,
            integration_account,
            global_vault_signer,
            lending_pool,
            sub_vaults_root_index: NIL,
            claimed_seats_root_index: NIL,
            market_orders_root_index: NIL,
            sub_vault_free_list_head_index: NIL,
            node_free_list_head_index: NIL,
            num_bytes_allocated: 0,
            sub_vault_count: 0,
            global_vault_signer_bump,
            version: crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            claimed_seat_count: 0,
            open_order_count: 0,
            _pad1: [0; 4],
            pending_global_vault_admin: Pubkey::default(),
            _reserved_aggregates: [0; 4],
            is_paused: 0,
            _pad2a: [0; 1],
            next_sub_vault_id: 1,
            _pad2: [0; 4],
            _reserved: [0; 1],
        }
    }

    /// `true` when the vault-level pause switch is set.
    pub fn is_paused(&self) -> bool {
        self.is_paused != 0
    }

    /// `true` when the sub_vault-region free list is non-empty.
    pub fn has_free_sub_vault_block(&self) -> bool {
        self.sub_vault_free_list_head_index != NIL
    }

    /// `true` when the node-region free list is non-empty.
    pub fn has_free_node_block(&self) -> bool {
        self.node_free_list_head_index != NIL
    }
}

impl Get for GlobalVaultFixed {}

impl YdeltaAccount for GlobalVaultFixed {
    fn verify_discriminant(&self) -> ProgramResult {
        require!(
            self.discriminator == GLOBAL_VAULT_FIXED_DISCRIMINANT,
            ProgramError::InvalidAccountData,
            "Invalid GlobalVault discriminant: {} (expected {})",
            self.discriminator,
            GLOBAL_VAULT_FIXED_DISCRIMINANT
        )?;
        Ok(())
    }

    fn verify_version(&self) -> ProgramResult {
        require!(
            self.version == crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            ProgramError::InvalidAccountData,
            "Stale GlobalVaultFixed layout: version {} (expected {})",
            self.version,
            crate::state::constants::ACCOUNT_LAYOUT_VERSION
        )?;
        Ok(())
    }
}

/// Padding type sized to [`super::constants::SUB_VAULT_FREE_LIST_BLOCK_SIZE`].
#[repr(C, packed)]
#[derive(Default, Copy, Clone, Pod, Zeroable)]
pub struct SubVaultUnusedFreeListPadding {
    _padding_a: [u64; 32],
    _padding_b: [u64; 31],
    _padding_tail: [u32; 1],
}
const_assert_eq!(
    size_of::<SubVaultUnusedFreeListPadding>(),
    super::constants::SUB_VAULT_FREE_LIST_BLOCK_SIZE
);

/// Padding type sized to [`super::constants::VAULT_NODE_FREE_LIST_BLOCK_SIZE`].
#[repr(C, packed)]
#[derive(Default, Copy, Clone, Pod, Zeroable)]
pub struct VaultNodeUnusedFreeListPadding {
    _padding: [u64; 19],
    _padding2: [u32; 1],
}
const_assert_eq!(
    size_of::<VaultNodeUnusedFreeListPadding>(),
    super::constants::VAULT_NODE_FREE_LIST_BLOCK_SIZE
);

/// Per-curator sub-vault inside a `GlobalVault`. Tracks total
/// principal, MTM-adjusted total assets, atoms-in-flight, the
/// weighted-rate aggregates for instantaneous yield, and the two
/// cumulative yield indices used to settle per-seat NAV at deposit /
/// withdraw boundaries.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct SubVault {
    /// 1-based sub-vault id within the vault (monotonic, never reused).
    pub sub_vault_id: u16,
    /// When non-zero, sub-vault is sunset: deposits, new orders, updates,
    /// and matches reject; withdraw / cancel / fee-claim / repay /
    /// liquidate still operate.
    pub is_sunset: u8,
    /// `SubVaultKind` discriminant: 0 = Pool (curator-run, pooled
    /// deposits), 1 = Private (single-owner). Semantics enforced by the
    /// deposit/withdraw/create processors.
    pub kind: u8,
    /// Quoted spread over the market's live marginfi lending APR, in bps.
    /// Placement computes `bank_apr_bps + spread_bps` and stores the
    /// result in the order.
    pub spread_bps: u16,
    _pad0: [u8; 2],

    /// Curator pubkey; signs curator-only instructions on this sub-vault.
    /// For Private sub-vaults this is the owner.
    pub curator: Pubkey,

    /// Origination LTV cap in bps (lender-side policy, read live at
    /// match time). Required at creation; not bounded by marginfi.
    pub max_ltv_bps: u16,
    /// Liquidation trigger in bps; enforced ≥ `max_ltv_bps +
    /// MIN_LIQ_GAP_BPS` at create/update, stamped onto loans at match.
    pub liquidation_ltv_bps: u16,
    /// Maximum loan term in seconds this sub-vault will fill.
    pub max_term_seconds: u32,

    /// Curator management fee in bps, set by the protocol admin at Pool
    /// creation (≤ MAX_CURATOR_FEE_BPS); 0 for Private. Snapshotted at
    /// match.
    pub curator_fee_bps: u16,
    /// Resting asks across all markets; inc on place, dec on
    /// cancel/admin-cancel.
    pub open_orders_count: u16,
    /// Open loans (queued matches + promoted, all markets); inc at fill,
    /// dec at full close.
    pub open_loans_count: u32,
    _pad2: [u8; 8],

    /// Total depositor shares outstanding in this sub_vault.
    pub total_shares: u128,
    /// Mark-to-market total assets in atoms, inclusive of unrealized
    /// idle supply yield.
    pub total_assets_atoms: u64,
    /// Withdrawable principal basis in atoms (realized deposits +
    /// settled idle yield).
    pub total_principal_atoms: u64,
    /// Atoms currently out on active loans against this sub_vault.
    pub deployed_principal_atoms: u64,
    /// Atoms reserved against open match opportunities (lender side of
    /// resting asks).
    pub encumbered_in_orders_atoms: u64,

    /// `sum(principal × rate_bps)` across active loans; used for the
    /// gross-yield estimate.
    pub total_weighted_rate_bps: u128,

    /// Curator fees accumulated and awaiting `ClaimCuratorFee`.
    pub accumulated_curator_fee_atoms: u64,
    /// Unix-ts of the last `accrue_sub_vault` pass.
    pub last_accrue_unix: i64,

    /// Per-share cumulative idle-supply yield index, scaled by 2^48.
    pub cumulative_supply_yield_index_scaled: u128,

    /// Per-share cumulative loan-delta yield index, scaled by 2^48.
    pub cumulative_delta_yield_index_scaled: u128,

    /// Marginfi asset_share_value snapshot at last accrual, used to
    /// detect idle-side share-value drift.
    pub last_supply_share_value_fp48: crate::math::Fp48,

    /// Pending curator (set by transfer, accepted by accept).
    pub pending_curator: Pubkey,

    /// `sum(principal × net_rate_bps)` — net of curator fee; the source
    /// for the per-tick loan-yield credit.
    pub total_weighted_net_rate_bps: u128,

    /// Atoms that have been retired by repay/liquidation/settle-matured but
    /// not yet swept by the curator's `claim_repayment_for_sub_vault`.
    /// They physically live in the per-market `lender_marginfi_account`,
    /// not in this vault's `global_vault_integration_account`, so they must
    /// be EXCLUDED from the `accrue_sub_vault` idle MTM calc — otherwise
    /// they'd appear to earn the integration-account's share-value drift
    /// while sitting in a different account.
    ///
    /// Lifecycle: `repay` (full-repay close-out) bumps this by the closed
    /// loan's `principal_debt_atoms`. `claim_repayment_for_sub_vault`
    /// decrements it by the atoms it physically moves from the per-market
    /// `lender_marginfi_account` into this vault's integration account.
    pub pending_claim_atoms: u64,

    _reserved_a: [u64; 29],
    _reserved_b: [u64; 2],
}
const_assert_eq!(size_of::<SubVault>(), SUB_VAULT_BLOCK_PAYLOAD_SIZE);
const_assert_eq!(size_of::<SubVault>() % 16, 0);

impl SubVault {
    /// Returns the tree key (the 1-based sub_vault id).
    pub fn key(&self) -> u16 {
        self.sub_vault_id
    }

    /// `true` when every accounting field and counter is zero — the gate
    /// `remove_sub_vault` uses to decide whether deletion is safe.
    /// Includes the order/loan counters so a sub-vault with
    /// resting asks or open loans can never be removed.
    pub fn is_empty(&self) -> bool {
        self.total_shares == 0
            && self.total_assets_atoms == 0
            && self.total_principal_atoms == 0
            && self.deployed_principal_atoms == 0
            && self.encumbered_in_orders_atoms == 0
            && self.accumulated_curator_fee_atoms == 0
            && self.pending_claim_atoms == 0
            && self.open_orders_count == 0
            && self.open_loans_count == 0
    }

    /// Build a fresh sub-vault with the supplied identity / cap fields
    /// and all accounting fields zeroed. The v1 fields (`kind`,
    /// `spread_bps`, `liquidation_ltv_bps`, `curator_fee_bps`) start
    /// zeroed; the creation processors stamp them.
    pub fn new_empty(
        sub_vault_id: u16,
        curator: Pubkey,
        max_ltv_bps: u16,
        max_term_seconds: u32,
    ) -> Self {
        Self {
            sub_vault_id,
            is_sunset: 0,
            kind: 0,
            spread_bps: 0,
            _pad0: [0; 2],
            curator,
            max_ltv_bps,
            liquidation_ltv_bps: 0,
            max_term_seconds,
            curator_fee_bps: 0,
            open_orders_count: 0,
            open_loans_count: 0,
            _pad2: [0; 8],
            total_shares: 0,
            total_assets_atoms: 0,
            total_principal_atoms: 0,
            deployed_principal_atoms: 0,
            encumbered_in_orders_atoms: 0,
            total_weighted_rate_bps: 0,
            accumulated_curator_fee_atoms: 0,
            last_accrue_unix: 0,
            cumulative_supply_yield_index_scaled: 0,
            cumulative_delta_yield_index_scaled: 0,
            last_supply_share_value_fp48: crate::math::Fp48::ZERO,
            pending_curator: Pubkey::default(),
            total_weighted_net_rate_bps: 0,
            pending_claim_atoms: 0,
            _reserved_a: [0; 29],
            _reserved_b: [0; 2],
        }
    }
}

impl PartialEq for SubVault {
    fn eq(&self, other: &Self) -> bool {
        self.sub_vault_id == other.sub_vault_id
    }
}
impl Eq for SubVault {}
impl Ord for SubVault {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sub_vault_id.cmp(&other.sub_vault_id)
    }
}
impl PartialOrd for SubVault {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for SubVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SubVault(id={}, curator={})",
            self.sub_vault_id, self.curator
        )
    }
}

/// Per-depositor seat inside a vault, one per `(owner, sub_vault_id)`.
/// Holds the depositor's share balance and snapshots of the sub_vault's
/// two cumulative yield indices so NAV growth between touches can be
/// settled lazily.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct SubVaultDepositorSeat {
    /// Depositor wallet.
    pub owner: Pubkey,
    /// Sub-vault id.
    pub sub_vault_id: u16,
    _pad0: [u8; 14],
    /// Depositor's share balance.
    pub shares: u128,
    /// Snapshot of the sub-vault's supply-yield index at last touch.
    pub snapshot_supply_yield_index_scaled: u128,
    /// Snapshot of the sub-vault's delta-yield index at last touch.
    pub snapshot_delta_yield_index_scaled: u128,
    /// Unix-ts of the last touch.
    pub last_updated_unix: i64,
    _padding: [u8; 8],

    _reserved: [u64; 4],
}
const_assert_eq!(
    size_of::<SubVaultDepositorSeat>(),
    VAULT_CLAIMED_SEAT_SIZE
);
const_assert_eq!(size_of::<SubVaultDepositorSeat>() % 8, 0);

impl SubVaultDepositorSeat {
    /// Builds a probe value with identity fields set and all balance
    /// fields zeroed; used for tree lookups.
    pub fn probe(owner: Pubkey, sub_vault_id: u16) -> Self {
        Self {
            owner,
            sub_vault_id,
            ..Default::default()
        }
    }
}

impl PartialEq for SubVaultDepositorSeat {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner && self.sub_vault_id == other.sub_vault_id
    }
}
impl Eq for SubVaultDepositorSeat {}
impl Ord for SubVaultDepositorSeat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.owner.cmp(&other.owner) {
            std::cmp::Ordering::Equal => self.sub_vault_id.cmp(&other.sub_vault_id),
            ord => ord,
        }
    }
}
impl PartialOrd for SubVaultDepositorSeat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl std::fmt::Display for SubVaultDepositorSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SubVaultDepositorSeat(owner={}, sub_vault_id={})",
            self.owner, self.sub_vault_id
        )
    }
}

/// Vault-side pointer to a resting sub-vault ask on a market. One per
/// `(market, sub_vault_id)`; lets curators enumerate their open orders
/// without scanning every market.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct SubVaultOrderRef {
    /// Market the ask rests on.
    pub market: Pubkey,
    /// Sub-vault id.
    pub sub_vault_id: u16,

    /// Side (always `Side::Ask` for sub-vault orders).
    pub side: u8,
    _pad0: [u8; 1],
    /// Quoted rate.
    pub rate_bps: u16,
    _pad1: [u8; 2],
    /// Term seconds offered.
    pub term_seconds: u32,
    _pad2: [u8; 4],

    /// Per-market order sequence assigned at rest time.
    pub order_sequence_in_market: u64,
    /// Unix-ts the order was placed.
    pub placed_at_unix: i64,

    _reserved: [u64; 10],
}
const_assert_eq!(size_of::<SubVaultOrderRef>(), VAULT_ORDER_REF_SIZE);
const_assert_eq!(size_of::<SubVaultOrderRef>() % 8, 0);

impl SubVaultOrderRef {
    /// Builds a probe value with identity fields set and order detail
    /// fields zeroed; used for tree lookups.
    pub fn probe(market: Pubkey, sub_vault_id: u16) -> Self {
        Self {
            market,
            sub_vault_id,
            side: 0,
            _pad0: [0; 1],
            rate_bps: 0,
            _pad1: [0; 2],
            term_seconds: 0,
            _pad2: [0; 4],
            order_sequence_in_market: 0,
            placed_at_unix: 0,
            _reserved: [0; 10],
        }
    }
}

impl PartialEq for SubVaultOrderRef {
    fn eq(&self, other: &Self) -> bool {
        self.market == other.market && self.sub_vault_id == other.sub_vault_id
    }
}
impl Eq for SubVaultOrderRef {}
impl Ord for SubVaultOrderRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.market.cmp(&other.market) {
            std::cmp::Ordering::Equal => self.sub_vault_id.cmp(&other.sub_vault_id),
            ord => ord,
        }
    }
}
impl PartialOrd for SubVaultOrderRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for SubVaultOrderRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SubVaultOrderRef(market={}, sub_vault_id={}, side={})",
            self.market, self.sub_vault_id, self.side
        )
    }
}

/// Mutable view of the `SubVault` tree.
pub type SubVaultTree<'a> = RedBlackTree<'a, SubVault>;
/// Read-only view of the `SubVault` tree.
pub type SubVaultTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, SubVault>;

/// Mutable view of the depositor-seat tree.
pub type SubVaultDepositorSeatTree<'a> = RedBlackTree<'a, SubVaultDepositorSeat>;
/// Read-only view of the depositor-seat tree.
pub type SubVaultDepositorSeatTreeReadOnly<'a> =
    RedBlackTreeReadOnly<'a, SubVaultDepositorSeat>;

/// Mutable view of the sub-vault order-ref tree.
pub type SubVaultOrderRefTree<'a> = RedBlackTree<'a, SubVaultOrderRef>;
/// Read-only view of the sub-vault order-ref tree.
pub type SubVaultOrderRefTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, SubVaultOrderRef>;

/// Returns a shared reference to the `SubVault` node at `index`.
pub fn get_helper_sub_vault(data: &[u8], index: DataIndex) -> &RBNode<SubVault> {
    get_helper::<RBNode<SubVault>>(data, index)
}

/// Returns a mutable reference to the `SubVault` node at `index`.
pub fn get_mut_helper_sub_vault(data: &mut [u8], index: DataIndex) -> &mut RBNode<SubVault> {
    get_mut_helper::<RBNode<SubVault>>(data, index)
}

/// Returns a shared reference to the `SubVaultDepositorSeat` at
/// `index`.
pub fn get_helper_sub_vault_depositor_seat(
    data: &[u8],
    index: DataIndex,
) -> &RBNode<SubVaultDepositorSeat> {
    get_helper::<RBNode<SubVaultDepositorSeat>>(data, index)
}

/// Returns a mutable reference to the `SubVaultDepositorSeat` at
/// `index`.
pub fn get_mut_helper_sub_vault_depositor_seat(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<SubVaultDepositorSeat> {
    get_mut_helper::<RBNode<SubVaultDepositorSeat>>(data, index)
}

/// Returns a shared reference to the `SubVaultOrderRef` at `index`.
pub fn get_helper_sub_vault_order_ref(
    data: &[u8],
    index: DataIndex,
) -> &RBNode<SubVaultOrderRef> {
    get_helper::<RBNode<SubVaultOrderRef>>(data, index)
}

/// Returns a mutable reference to the `SubVaultOrderRef` at `index`.
pub fn get_mut_helper_sub_vault_order_ref(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<SubVaultOrderRef> {
    get_mut_helper::<RBNode<SubVaultOrderRef>>(data, index)
}

const ACCRUE_INDEX_SCALE: u128 = 1u128 << 48;

/// Reads marginfi's current `asset_share_value` from `bank_ai` and wraps
/// it as an `Fp48`.
pub fn read_bank_asset_share_value_fp48(
    bank_ai: &solana_program::account_info::AccountInfo,
) -> Result<crate::math::Fp48, ProgramError> {
    let data = bank_ai
        .try_borrow_data()
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    let raw = crate::protocol::marginfi::wrapped_i80f48_to_u128(bank.asset_share_value)?;
    Ok(crate::math::Fp48::from_raw(raw))
}

/// Walks the sub_vault forward to `now`, applying two yield sources:
/// (1) idle-supply MTM from the marginfi share-value delta against the
/// stored snapshot, and (2) loan-yield from
/// `total_weighted_net_rate_bps × elapsed`. Updates the two cumulative
/// indices, the asset/principal pair, and `last_supply_share_value_fp48`,
/// then calls [`restore_assets_principal_invariant`].
pub fn accrue_sub_vault(
    sub_vault: &mut SubVault,
    now: i64,
    current_share_value: crate::math::Fp48,
) -> ProgramResult {
    if now <= sub_vault.last_accrue_unix {
        return Ok(());
    }
    if sub_vault.total_principal_atoms == 0 {
        sub_vault.last_accrue_unix = now;
        if !current_share_value.is_zero() {
            sub_vault.last_supply_share_value_fp48 = current_share_value;
        }
        return Ok(());
    }

    let elapsed: u128 = now
        .checked_sub(sub_vault.last_accrue_unix)
        .filter(|d| *d >= 0)
        .map(|d| d as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let denom: u128 = (super::loan::BPS_PER_UNIT as u128)
        .checked_mul(super::loan::SECONDS_PER_YEAR as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Idle = atoms physically in this vault's integration account,
    // i.e. total_principal MINUS what's out in active loans (deployed)
    // MINUS what's been repaid-but-not-yet-claimed (pending_claim).
    // Pending-claim atoms live on the per-market lender_marginfi_account,
    // not here — including them in the MTM would credit this vault with
    // the OTHER account's share-value drift.
    let idle: u64 = sub_vault
        .total_principal_atoms
        .checked_sub(sub_vault.deployed_principal_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?
        .checked_sub(sub_vault.pending_claim_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let idle_delta_atoms: i128 = if idle > 0 && !current_share_value.is_zero() {
        let snapshot = sub_vault.last_supply_share_value_fp48;
        if snapshot.is_zero() {
            0
        } else {
            // `idle × (current / snapshot)` — the two fp48 factors cancel,
            // so the integer ratio `idle_atoms × current_raw / snapshot_raw`
            // yields plain atoms. Routed through `mul_div` (U256 inside) so
            // the `idle × current_raw` intermediate cannot overflow u128.
            // The `.raw()` extraction is the one Fp48-boundary in this
            // function; flagged with a comment because every escape to raw
            // u128 should be audit-visible.
            let current_idle_value = crate::math::mul_div(
                idle as u128,
                current_share_value.raw(),
                snapshot.raw(),
                false,
            )?;
            if current_idle_value > u64::MAX as u128 {
                return Err(crate::program::YdeltaError::MathOverflow.into());
            }
            (current_idle_value as i128) - (idle as i128)
        }
    } else {
        0
    };

    let loan_yield_atoms: u128 = crate::math::mul_div(
        sub_vault.total_weighted_net_rate_bps,
        elapsed,
        denom,
        false,
    )?;

    if idle_delta_atoms >= 0 {
        // Gain path: i128 → u64 must be in range. The earlier
        // `current_idle_value > u64::MAX` guard caps current_idle_value;
        // since `idle` is already u64, the delta also fits — but use
        // try_from for defense-in-depth.
        let idle_gain: u64 = u64::try_from(idle_delta_atoms)
            .map_err(|_| crate::program::YdeltaError::MathOverflow)?;
        sub_vault.total_assets_atoms = sub_vault
            .total_assets_atoms
            .checked_add(idle_gain)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        sub_vault.total_principal_atoms = sub_vault
            .total_principal_atoms
            .checked_add(idle_gain)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    } else {
        let idle_loss: u64 = idle_delta_atoms
            .checked_neg()
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(crate::program::YdeltaError::MathOverflow)?;
        sub_vault.total_assets_atoms = sub_vault.total_assets_atoms.saturating_sub(idle_loss);
        sub_vault.total_principal_atoms = sub_vault.total_principal_atoms.saturating_sub(idle_loss);
    }
    if loan_yield_atoms > u64::MAX as u128 {
        return Err(crate::program::YdeltaError::MathOverflow.into());
    }
    sub_vault.total_assets_atoms = sub_vault
        .total_assets_atoms
        .checked_add(loan_yield_atoms as u64)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    if sub_vault.total_shares > 0 {
        if idle_delta_atoms > 0 {
            let supply_growth = crate::math::mul_div(
                idle_delta_atoms as u128,
                ACCRUE_INDEX_SCALE,
                sub_vault.total_shares,
                false,
            )?;
            sub_vault.cumulative_supply_yield_index_scaled = sub_vault
                .cumulative_supply_yield_index_scaled
                .checked_add(supply_growth)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        let delta_growth = crate::math::mul_div(
            loan_yield_atoms,
            ACCRUE_INDEX_SCALE,
            sub_vault.total_shares,
            false,
        )?;
        sub_vault.cumulative_delta_yield_index_scaled = sub_vault
            .cumulative_delta_yield_index_scaled
            .checked_add(delta_growth)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    sub_vault.last_accrue_unix = now;
    if !current_share_value.is_zero() {
        sub_vault.last_supply_share_value_fp48 = current_share_value;
    }
    restore_assets_principal_invariant(sub_vault);
    Ok(())
}

/// Enforces `total_assets_atoms >= total_principal_atoms` by floor-ing
/// the assets to the principal basis. Corrects the per-tick rounding
/// drift between the two fields at deposit / withdraw / close boundaries.
pub fn restore_assets_principal_invariant(sub_vault: &mut SubVault) {
    if sub_vault.total_assets_atoms < sub_vault.total_principal_atoms {
        sub_vault.total_assets_atoms = sub_vault.total_principal_atoms;
    }
}

const _: () = {
    assert!(VAULT_CLAIMED_SEAT_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);
    assert!(VAULT_ORDER_REF_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);

    assert!(SUB_VAULT_BLOCK_PAYLOAD_SIZE != VAULT_NODE_BLOCK_PAYLOAD_SIZE);
};

/// Pops a free block off the sub_vault region's free list.
pub fn get_free_sub_vault_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    let mut free_list: FreeList<SubVaultUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.sub_vault_free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.sub_vault_free_list_head_index = free_list.get_head();
    free_address
}

/// Pushes `index` back onto the sub_vault region's free list.
pub fn release_sub_vault_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<SubVaultUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.sub_vault_free_list_head_index);
    free_list.add(index);
    fixed.sub_vault_free_list_head_index = free_list.get_head();
}

/// Pops a free block off the node region's free list (used for
/// depositor seats and order refs).
pub fn get_free_node_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    let mut free_list: FreeList<VaultNodeUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.node_free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.node_free_list_head_index = free_list.get_head();
    free_address
}

/// Pushes `index` back onto the node region's free list.
pub fn release_node_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<VaultNodeUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.node_free_list_head_index);
    free_list.add(index);
    fixed.node_free_list_head_index = free_list.get_head();
}

/// Grows the vault's dynamic tail by one sub_vault-sized block, pushing
/// it onto the sub_vault free list. Caller must realloc the underlying
/// account first.
pub fn vault_expand_sub_vault_block(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
) -> ProgramResult {
    let mut free_list: FreeList<SubVaultUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.sub_vault_free_list_head_index);
    free_list.add(fixed.num_bytes_allocated);
    fixed.num_bytes_allocated = fixed
        .num_bytes_allocated
        .checked_add(SUB_VAULT_BLOCK_SIZE as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    fixed.sub_vault_free_list_head_index = free_list.get_head();
    Ok(())
}

/// Grows the vault's dynamic tail by one node-sized block, pushing it
/// onto the node free list. Caller must realloc the underlying account
/// first.
pub fn vault_expand_node_block(fixed: &mut GlobalVaultFixed, dynamic: &mut [u8]) -> ProgramResult {
    let mut free_list: FreeList<VaultNodeUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.node_free_list_head_index);
    free_list.add(fixed.num_bytes_allocated);
    fixed.num_bytes_allocated = fixed
        .num_bytes_allocated
        .checked_add(VAULT_NODE_BLOCK_SIZE as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    fixed.node_free_list_head_index = free_list.get_head();
    Ok(())
}

/// Inserts a `SubVaultDepositorSeat` for `(owner, sub_vault_id)` if
/// absent; returns the existing node index on hit. Bumps
/// `claimed_seat_count` on insert.
pub fn upsert_sub_vault_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    sub_vault_id: u16,
) -> Result<DataIndex, ProgramError> {
    let probe = SubVaultDepositorSeat::probe(owner, sub_vault_id);
    let existing_idx = {
        let tree =
            SubVaultDepositorSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx != NIL {
        return Ok(existing_idx);
    }

    let order_index = get_free_node_address_on_vault_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "no free vault-node block (vault_expand_node_block should have run)"
    )?;
    let mut tree = SubVaultDepositorSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
    tree.insert(order_index, probe);
    fixed.claimed_seats_root_index = tree.get_root_index();
    drop(tree);
    fixed.claimed_seat_count = fixed
        .claimed_seat_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(order_index)
}

/// Removes the depositor seat for `(owner, sub_vault_id)` and returns the
/// freed node index, or `NIL` when not present.
pub fn remove_sub_vault_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    sub_vault_id: u16,
) -> Result<DataIndex, ProgramError> {
    let probe = SubVaultDepositorSeat::probe(owner, sub_vault_id);
    let existing_idx = {
        let tree =
            SubVaultDepositorSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx == NIL {
        return Ok(NIL);
    }
    let mut tree = SubVaultDepositorSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
    tree.remove_by_index(existing_idx);
    fixed.claimed_seats_root_index = tree.get_root_index();
    drop(tree);
    fixed.claimed_seat_count = fixed
        .claimed_seat_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    release_node_address_on_vault_fixed(fixed, dynamic, existing_idx);
    Ok(existing_idx)
}

/// Inserts a `SubVaultOrderRef` keyed on `(market, sub_vault_id)`.
/// Errors with `SubVaultOrderExists` when one already exists — the
/// vault enforces at most one resting ask per sub_vault per market.
#[allow(clippy::too_many_arguments)]
/// Adjusts the per-sub-vault `open_orders_count`. `delta` is
/// +1 on place, -1 on cancel/admin-cancel. Errors when the sub-vault is
/// missing or the counter would underflow — both indicate accounting
/// drift that must surface, not be masked.
fn bump_sub_vault_order_count(
    fixed: &GlobalVaultFixed,
    dynamic: &mut [u8],
    sub_vault_id: u16,
    delta: i32,
) -> ProgramResult {
    let probe = SubVault::new_empty(sub_vault_id, Pubkey::default(), 1, 1);
    let idx = {
        let tree = SubVaultTreeReadOnly::new(dynamic, fixed.sub_vaults_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        idx != NIL,
        crate::program::YdeltaError::SubVaultNotFound,
        "order-count bump: sub_vault_id {} not found",
        sub_vault_id
    )?;
    let sv = get_mut_helper_sub_vault(dynamic, idx).get_mut_value();
    sv.open_orders_count = if delta >= 0 {
        sv.open_orders_count
            .checked_add(delta as u16)
            .ok_or(ProgramError::ArithmeticOverflow)?
    } else {
        sv.open_orders_count
            .checked_sub((-delta) as u16)
            .ok_or(ProgramError::ArithmeticOverflow)?
    };
    Ok(())
}

pub fn insert_sub_vault_order_ref(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    sub_vault_id: u16,
    side: u8,
    rate_bps: u16,
    term_seconds: u32,
    order_sequence_in_market: u64,
    placed_at_unix: i64,
) -> Result<DataIndex, ProgramError> {
    let probe = SubVaultOrderRef {
        market,
        sub_vault_id,
        ..Default::default()
    };
    let existing_idx = {
        let tree =
            SubVaultOrderRefTreeReadOnly::new(dynamic, fixed.market_orders_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        existing_idx == NIL,
        crate::program::YdeltaError::SubVaultOrderExists,
        "SubVaultOrderRef already exists for (market={}, sub_vault_id={})",
        market,
        sub_vault_id
    )?;

    let order_index = get_free_node_address_on_vault_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "no free vault-node block"
    )?;

    let order = SubVaultOrderRef {
        market,
        sub_vault_id,
        side,
        _pad0: [0; 1],
        rate_bps,
        _pad1: [0; 2],
        term_seconds,
        _pad2: [0; 4],
        order_sequence_in_market,
        placed_at_unix,
        _reserved: [0; 10],
    };
    let mut tree = SubVaultOrderRefTree::new(dynamic, fixed.market_orders_root_index, NIL);
    tree.insert(order_index, order);
    fixed.market_orders_root_index = tree.get_root_index();
    drop(tree);
    fixed.open_order_count = fixed
        .open_order_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    bump_sub_vault_order_count(fixed, dynamic, sub_vault_id, 1)?;
    Ok(order_index)
}

/// Removes the `SubVault` with `sub_vault_id` and returns the freed
/// node index. Errors with `InvalidArgument` when the sub_vault is not
/// empty (i.e. it still has shares, principal, deployed atoms, fees, or
/// pending claims).
pub fn remove_sub_vault(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    sub_vault_id: u16,
) -> Result<DataIndex, ProgramError> {
    let probe = SubVault::new_empty(sub_vault_id, Pubkey::default(), 1, 1);
    let idx = {
        let tree = SubVaultTreeReadOnly::new(dynamic, fixed.sub_vaults_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return Ok(NIL);
    }
    {
        let sub_vault = get_helper_sub_vault(dynamic, idx).get_value();
        require!(
            sub_vault.is_empty(),
            crate::program::YdeltaError::InvalidArgument,
            "remove_sub_vault: sub_vault {} not empty (deployed={}, shares={}, principal={})",
            sub_vault_id,
            sub_vault.deployed_principal_atoms,
            sub_vault.total_shares,
            sub_vault.total_principal_atoms,
        )?;
    }
    let mut tree = SubVaultTree::new(dynamic, fixed.sub_vaults_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.sub_vaults_root_index = tree.get_root_index();
    drop(tree);
    fixed.sub_vault_count = fixed
        .sub_vault_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    release_sub_vault_address_on_vault_fixed(fixed, dynamic, idx);
    Ok(idx)
}

/// Removes the `SubVaultOrderRef` for `(market, sub_vault_id)` and
/// returns the freed node index, or `NIL` when not present.
pub fn remove_sub_vault_order_ref(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    sub_vault_id: u16,
) -> Result<DataIndex, ProgramError> {
    let probe = SubVaultOrderRef {
        market,
        sub_vault_id,
        ..Default::default()
    };
    let idx = {
        let tree =
            SubVaultOrderRefTreeReadOnly::new(dynamic, fixed.market_orders_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return Ok(NIL);
    }
    let mut tree = SubVaultOrderRefTree::new(dynamic, fixed.market_orders_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.market_orders_root_index = tree.get_root_index();
    drop(tree);
    fixed.open_order_count = fixed
        .open_order_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    release_node_address_on_vault_fixed(fixed, dynamic, idx);
    bump_sub_vault_order_count(fixed, dynamic, sub_vault_id, -1)?;
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Fp48;

    #[test]
    fn vault_fixed_size_locked() {
        assert_eq!(size_of::<GlobalVaultFixed>(), GLOBAL_VAULT_FIXED_SIZE);
    }

    #[test]
    fn sub_vault_size_locked() {
        assert_eq!(size_of::<SubVault>(), SUB_VAULT_BLOCK_PAYLOAD_SIZE);
    }

    #[test]
    fn sub_vault_order_ref_size_locked() {
        assert_eq!(size_of::<SubVaultOrderRef>(), VAULT_ORDER_REF_SIZE);
    }

    #[test]
    fn global_vault_pda_is_deterministic() {
        let mint = Pubkey::new_unique();
        let (a, ba) = global_vault_pda(&mint);
        let (b, bb) = global_vault_pda(&mint);
        assert_eq!(a, b);
        assert_eq!(ba, bb);
        let (c, _) = global_vault_pda(&Pubkey::new_unique());
        assert_ne!(a, c);
    }

    #[test]
    fn global_vault_signer_pda_is_deterministic() {
        let vault = Pubkey::new_unique();
        let (a, _) = global_vault_signer_pda(&vault);
        let (b, _) = global_vault_signer_pda(&vault);
        assert_eq!(a, b);
    }

    fn fresh_sub_vault() -> SubVault {
        SubVault::new_empty(7, Pubkey::default(), 5_000, 30 * 86_400)
    }

    const SHARE_VALUE_ONE: u128 = 1u128 << 48;

    #[test]
    fn accrue_zero_elapsed_is_noop() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 1_000;
        accrue_sub_vault(&mut p, 1_000, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        assert_eq!(p.total_assets_atoms, 1_000_000);
        assert_eq!(p.last_accrue_unix, 1_000);
    }

    #[test]
    fn accrue_supply_yield_from_share_value_delta() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);

        let current = SHARE_VALUE_ONE + (SHARE_VALUE_ONE / 100);
        accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(current)).unwrap();

        let yield_atoms = p.total_assets_atoms - 1_000_000;
        assert!(yield_atoms >= 9_990 && yield_atoms <= 10_010);
        assert_eq!(
            p.total_principal_atoms - 1_000_000,
            yield_atoms,
            "idle supply yield must lift the withdrawable principal basis too"
        );
    }

    #[test]
    fn accrue_supply_value_retrace_marks_down_assets_and_principal() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);
        let current = SHARE_VALUE_ONE - (SHARE_VALUE_ONE / 100);
        accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(current)).unwrap();
        let loss_atoms = 1_000_000 - p.total_assets_atoms;
        assert!(loss_atoms >= 9_990 && loss_atoms <= 10_010);
        assert_eq!(
            1_000_000 - p.total_principal_atoms,
            loss_atoms,
            "idle share-value losses must hit the withdrawable principal basis too"
        );
    }

    #[test]
    fn accrue_loan_yield_uses_total_weighted_net_rate_aggregate() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_weighted_net_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        accrue_sub_vault(&mut p, 86_400, Fp48::ZERO).unwrap();
        let expected = (100_000u128 * 800 * 86_400) / (10_000 * 31_536_000);
        assert_eq!(p.total_assets_atoms - 1_000_000, expected as u64);
    }

    #[test]
    fn accrue_loan_yield_is_net_of_curator_fee_not_double_counted() {
        let curator_fee_bps: u128 = 2_000;
        let gross_weighted: u128 = 100_000u128 * 800;
        let net_weighted: u128 = gross_weighted * (10_000 - curator_fee_bps) / 10_000;

        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = gross_weighted;
        p.total_weighted_net_rate_bps = net_weighted;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        accrue_sub_vault(&mut p, 86_400, Fp48::ZERO).unwrap();

        let credited = p.total_assets_atoms - 1_000_000;
        let expected_net = (net_weighted * 86_400) / (10_000 * 31_536_000);
        assert_eq!(credited as u128, expected_net);

        let gross_yield = (gross_weighted * 86_400) / (10_000 * 31_536_000);
        assert!(
            (credited as u128) < gross_yield,
            "net loan-yield {} should be < gross {} (curator fee excluded)",
            credited,
            gross_yield
        );
    }

    #[test]
    fn accrue_idempotent_when_run_again_at_same_now() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_weighted_net_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);
        accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        let assets_after_first = p.total_assets_atoms;

        accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        assert_eq!(p.total_assets_atoms, assets_after_first);
    }

    #[test]
    fn accrue_two_calls_match_one_call_within_tolerance() {
        let mut a = fresh_sub_vault();
        a.total_shares = 1_000_000;
        a.total_principal_atoms = 1_000_000;
        a.deployed_principal_atoms = 100_000;
        a.total_weighted_rate_bps = 100_000u128 * 800;
        a.total_weighted_net_rate_bps = 100_000u128 * 800;
        a.total_assets_atoms = 1_000_000;
        a.last_accrue_unix = 0;
        a.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);
        let mut b = a;
        accrue_sub_vault(&mut a, 30 * 86_400, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        accrue_sub_vault(&mut b, 15 * 86_400, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        accrue_sub_vault(&mut b, 30 * 86_400, Fp48::from_raw(SHARE_VALUE_ONE)).unwrap();
        let diff = (a.total_assets_atoms as i64 - b.total_assets_atoms as i64).abs();
        assert!(
            diff <= 1,
            "single-call vs two-call diverge by {} atoms",
            diff
        );
    }

    #[test]
    fn accrue_supply_yield_includes_encumbered_atoms() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 0;
        p.encumbered_in_orders_atoms = 400_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);
        let current = SHARE_VALUE_ONE + (SHARE_VALUE_ONE / 100);
        accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(current)).unwrap();
        let yield_atoms = p.total_assets_atoms - 1_000_000;
        assert!(
            yield_atoms >= 9_990 && yield_atoms <= 10_010,
            "encumbered atoms must earn supply yield: idle base should be \
             total_principal − deployed (1M), got yield {}",
            yield_atoms
        );
        assert_eq!(
            p.total_principal_atoms - 1_000_000,
            yield_atoms,
            "encumbered idle-side yield must stay withdrawable by the sub_vault"
        );
    }

    #[test]
    fn accrue_hard_fails_when_deployed_exceeds_principal() {
        let mut p = fresh_sub_vault();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;

        p.deployed_principal_atoms = 1_500_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = Fp48::from_raw(SHARE_VALUE_ONE);
        let result = accrue_sub_vault(&mut p, 86_400, Fp48::from_raw(SHARE_VALUE_ONE));
        assert!(
            result.is_err(),
            "accrue_sub_vault must hard-fail when deployed > total_principal"
        );
    }

    #[test]
    fn fresh_vault_has_expected_initial_state() {
        let vault = GlobalVaultFixed::new_empty(
            Pubkey::default(),
            Pubkey::default(),
            Pubkey::default(),
            Pubkey::default(),
            Pubkey::default(),
            255,
            Pubkey::default(),
        );
        assert_eq!(vault.discriminator, GLOBAL_VAULT_FIXED_DISCRIMINANT);
        assert_eq!(vault.sub_vaults_root_index, NIL);
        assert_eq!(vault.claimed_seats_root_index, NIL);
        assert_eq!(vault.market_orders_root_index, NIL);
        assert_eq!(vault.sub_vault_free_list_head_index, NIL);
        assert_eq!(vault.node_free_list_head_index, NIL);
        assert_eq!(vault.sub_vault_count, 0);
        assert_eq!(vault.claimed_seat_count, 0);
        assert_eq!(vault.open_order_count, 0);
        assert_eq!(vault.global_vault_signer_bump, 255);
        assert!(!vault.has_free_sub_vault_block());
        assert!(!vault.has_free_node_block());
    }

    fn sub_vault_with(total_assets: u64, total_principal: u64) -> SubVault {
        let mut p = SubVault::new_empty(0, Pubkey::default(), 1, 1);
        p.total_assets_atoms = total_assets;
        p.total_principal_atoms = total_principal;
        p
    }

    #[test]
    fn restore_assets_principal_invariant_floor_bumps_assets_to_principal() {
        let mut p = sub_vault_with(99, 100);
        restore_assets_principal_invariant(&mut p);
        assert_eq!(p.total_assets_atoms, 100);
        assert_eq!(p.total_principal_atoms, 100);
    }

    #[test]
    fn restore_assets_principal_invariant_leaves_healthy_sub_vault_alone() {
        let mut p = sub_vault_with(150, 100);
        restore_assets_principal_invariant(&mut p);
        assert_eq!(p.total_assets_atoms, 150, "open-loan estimate must not be clipped");
        assert_eq!(p.total_principal_atoms, 100);
    }

    #[test]
    fn restore_assets_principal_invariant_handles_equal_values() {
        let mut p = sub_vault_with(100, 100);
        restore_assets_principal_invariant(&mut p);
        assert_eq!(p.total_assets_atoms, 100);
        assert_eq!(p.total_principal_atoms, 100);
    }

    #[test]
    fn restore_assets_principal_invariant_handles_zero() {
        let mut p = sub_vault_with(0, 0);
        restore_assets_principal_invariant(&mut p);
        assert_eq!(p.total_assets_atoms, 0);
        assert_eq!(p.total_principal_atoms, 0);
    }

    #[test]
    fn restore_assets_principal_invariant_handles_large_drift() {
        let mut p = sub_vault_with(0, u64::MAX);
        restore_assets_principal_invariant(&mut p);
        assert_eq!(p.total_assets_atoms, u64::MAX);
    }
}
