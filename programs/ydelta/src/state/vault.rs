//! Global vault state. One debt-side vault per mint, with multiple
//! risk profiles sharing a single integration account.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use hypertree::{
    get_helper, get_mut_helper, DataIndex, Get, RBNode, RedBlackTree, RedBlackTreeReadOnly, NIL,
};
use shank::{ShankAccount, ShankType};
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::require;
use crate::validation::YdeltaAccount;

use super::constants::{
    GLOBAL_VAULT_FIXED_DISCRIMINANT, GLOBAL_VAULT_FIXED_SIZE, RISK_PROFILE_BLOCK_PAYLOAD_SIZE,
    VAULT_CLAIMED_SEAT_SIZE, VAULT_NODE_BLOCK_PAYLOAD_SIZE, VAULT_ORDER_REF_SIZE,
};

// ─────────────────── PDA seeds + derivation ───────────────────

/// PDA seed prefix for the per-mint `GlobalVault`. Final seeds:
/// `[VAULT_SEED, mint.as_ref()]`.
pub const VAULT_SEED: &[u8] = b"vault";

/// PDA seed prefix for the vault's signer (authority over the
/// integration_account). Final seeds: `[GLOBAL_VAULT_SIGNER_SEED, vault.as_ref()]`.
pub const GLOBAL_VAULT_SIGNER_SEED: &[u8] = b"global_vault_signer";

/// PDA seed prefix for the vault's marginfi integration account.
/// Final seeds: `[VAULT_INTEGRATION_SEED, vault.as_ref()]`.
pub const VAULT_INTEGRATION_SEED: &[u8] = b"vault_integration";

/// PDA seed prefix for the vault's per-mint staging SPL token account.
/// Owned by `global_vault_signer`; sits between user wallets and the marginfi
/// integration account on `global_vault_deposit` / `global_vault_withdraw`.
/// Final seeds: `[VAULT_STAGING_SEED, vault.as_ref()]`.
pub const VAULT_STAGING_SEED: &[u8] = b"global_vault_staging";

pub fn global_vault_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, mint.as_ref()], &crate::id())
}

pub fn global_vault_signer_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_VAULT_SIGNER_SEED, vault.as_ref()], &crate::id())
}

pub fn global_vault_integration_account_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_INTEGRATION_SEED, vault.as_ref()], &crate::id())
}

pub fn global_vault_staging_pda(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_STAGING_SEED, vault.as_ref()], &crate::id())
}

// ─────────────────── GlobalVaultFixed ───────────────────

/// `GlobalVaultFixed` — 320-byte header. PDA seeds:
/// `[b"vault", mint]`.
///
/// Holds two free-list heads — the `RiskProfile` blocks are 512 bytes
/// each, while the smaller `RiskProfileDepositorSeat` and
/// `RiskProfileOrderRef` blocks share a 128-byte allocator.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultFixed {
    pub discriminator: u64,          // 0..8
    pub mint: Pubkey,                // 8..40
    pub global_vault_admin: Pubkey,  // 40..72
    pub integration_pool: Pubkey,    // 72..104   marginfi group
    pub integration_account: Pubkey, // 104..136
    pub global_vault_signer: Pubkey, // 136..168
    /// The marginfi bank for this vault's mint. Pinned at
    /// `create_vault` time — vault atoms always deposit into / withdraw
    /// from this exact bank, so depositor balances stay coherent.
    pub lending_pool: Pubkey, // 168..200

    // Tree roots
    pub risk_profiles_root_index: DataIndex, // 200..204
    pub claimed_seats_root_index: DataIndex, // 204..208
    pub market_orders_root_index: DataIndex, // 208..212

    // Two free-list heads
    pub profile_free_list_head_index: DataIndex, // 212..216  (512-byte blocks)
    pub node_free_list_head_index: DataIndex,    // 216..220  (128-byte blocks)

    pub num_bytes_allocated: u32, // 220..224

    // Counters
    pub risk_profile_count: u8,       // 224..225
    pub global_vault_signer_bump: u8, // 225..226
    /// Layout version. Bumped when fields are added or repurposed so
    /// off-chain decoders can distinguish layouts. New accounts ship
    /// with version=0; future migrations bump in lockstep with the
    /// schema change.
    pub version: u8, // 226..227
    _pad0: [u8; 1],                   // 227..228
    pub claimed_seat_count: u32,      // 228..232
    pub open_order_count: u32,        // 232..236
    _pad1: [u8; 4],                   // 236..240 (align u64 below)

    /// Staged successor admin set by `transfer_global_vault_admin`.
    /// `accept_global_vault_admin` (signer = pending_global_vault_admin) finalizes
    /// the transfer. `Pubkey::default()` means "no pending transfer".
    pub pending_global_vault_admin: Pubkey, // 240..272

    /// Reserved space. Original 16 bytes plus the 64-byte header
    /// growth (`GLOBAL_VAULT_FIXED_SIZE`: 256 → 320). After
    /// `pending_global_vault_admin: Pubkey` (32 bytes) we have 16 + 64 − 32
    /// = 48 bytes = 6 u64s left.
    _reserved: [u64; 6], // 272..320
}
const_assert_eq!(size_of::<GlobalVaultFixed>(), GLOBAL_VAULT_FIXED_SIZE);
const_assert_eq!(size_of::<GlobalVaultFixed>() % 8, 0);

impl GlobalVaultFixed {
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
            risk_profiles_root_index: NIL,
            claimed_seats_root_index: NIL,
            market_orders_root_index: NIL,
            profile_free_list_head_index: NIL,
            node_free_list_head_index: NIL,
            num_bytes_allocated: 0,
            risk_profile_count: 0,
            global_vault_signer_bump,
            version: 0,
            _pad0: [0; 1],
            claimed_seat_count: 0,
            open_order_count: 0,
            _pad1: [0; 4],
            pending_global_vault_admin: Pubkey::default(),
            _reserved: [0; 6],
        }
    }

    pub fn has_free_profile_block(&self) -> bool {
        self.profile_free_list_head_index != NIL
    }

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
}

// ─────────────────── Free-list padding ───────────────────

/// Padding type for empty 512-byte profile blocks on the
/// `profile_free_list`. Sized to `RISK_PROFILE_FREE_LIST_BLOCK_SIZE`
/// (block - 4 bytes for the free-list-node header). Split into 32-element
/// arrays because `[T; N]` only auto-derives `Default`/`Pod`/`Zeroable`
/// for N <= 32 in our toolchain.
#[repr(C, packed)]
#[derive(Default, Copy, Clone, Pod, Zeroable)]
pub struct ProfileUnusedFreeListPadding {
    _padding_a: [u64; 32],
    _padding_b: [u64; 31],
    _padding_tail: [u32; 1],
}
const_assert_eq!(
    size_of::<ProfileUnusedFreeListPadding>(),
    super::constants::RISK_PROFILE_FREE_LIST_BLOCK_SIZE
);

/// Padding type for empty 160-byte node blocks on the
/// `node_free_list` (shared between `RiskProfileDepositorSeat` and
/// `RiskProfileOrderRef`).
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

// ─────────────────── RiskProfile (tree node) ───────────────────

/// `RiskProfile` — one per (vault, profile_id). Lives in the vault's
/// `risk_profiles` tree; keyed by `profile_id` (sort order:
/// ascending).
///
/// **Size: 496 bytes (= `RISK_PROFILE_BLOCK_PAYLOAD_SIZE`).** Profiles
/// get their own 512-byte free list (vs the 128-byte block other
/// vault node types share) to leave headroom for future fields.
///
/// Two aggregate variables drive O(1) `accrue_risk_profile`:
/// - `deployed_principal_atoms` — `Σ(loan.principal)` across open loans.
/// - `total_weighted_rate_bps` — `Σ(loan.principal × loan.lender_rate_bps)`.
///
/// Updated at match (`process_matched_loan` vault path) and close
/// (the vault branch of `process_repay`).
///
/// **Markets-this-profile-can-quote-in is a counter, not a whitelist.**
/// `allowed_market_count` = how many markets currently host a
/// vault-owned `ClaimedSeat` for this profile; `allowed_market_max` =
/// the cap set at profile creation. Incremented in
/// `claim_seat_for_risk_profile`, decremented when a profile closes its
/// market seat.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfile {
    /// Tree key. u8 → cap of 256 profiles per vault.
    pub profile_id: u8, // 0..1
    _pad0: [u8; 7], // 1..8
    /// Curator pubkey — gates `place_order_for_risk_profile` /
    /// `cancel_order_for_risk_profile` / `update_order_for_risk_profile` /
    /// `claim_curator_fee`. Set at profile creation, immutable.
    pub curator: Pubkey, // 8..40

    // ─── Risk policy (admin-set at create_risk_profile time) ───
    pub max_ltv_bps: u16,      // 40..42
    _pad1: [u8; 2],            // 42..44
    pub max_term_seconds: u32, // 44..48
    /// Live count of markets that hold a vault-owned `ClaimedSeat`
    /// for this profile. Incremented on `claim_seat_for_risk_profile`,
    /// decremented on profile-removes-seat. `< allowed_market_max`.
    pub allowed_market_count: u8, // 48..49
    /// Cap set at profile creation. `claim_seat_for_risk_profile` rejects
    /// when `allowed_market_count >= allowed_market_max`.
    pub allowed_market_max: u8, // 49..50
    _pad2: [u8; 14],           // 50..64  (push to 16-align for u128 below)

    // ─── Principal decomposition (running aggregates) ───
    pub total_shares: u128,              // 64..80
    pub total_assets_atoms: u64,         // 80..88
    pub total_principal_atoms: u64,      // 88..96
    pub deployed_principal_atoms: u64,   // 96..104
    pub encumbered_in_orders_atoms: u64, // 104..112

    /// Aggregate weighted rate for O(1) `accrue_risk_profile`.
    /// `Σ(loan_principal × loan_rate_bps)` across open loans.
    pub total_weighted_rate_bps: u128, // 112..128

    pub accumulated_curator_fee_atoms: u64, // 128..136
    pub last_accrue_unix: i64,              // 136..144

    // ─── Cumulative yield indices (Aave-style; × 2^48) ───
    /// Cumulative supply yield index — drives the supply-component of
    /// per-depositor crystallisation in `global_vault_deposit` / `global_vault_withdraw`.
    pub cumulative_supply_yield_index_scaled: u128, // 144..160
    /// Cumulative delta yield index — drives the lender-rate-component.
    pub cumulative_delta_yield_index_scaled: u128, // 160..176

    /// Snapshot of marginfi's `Bank.asset_share_value` (fp48) at
    /// `last_accrue_unix`. `accrue_risk_profile` reads the bank's
    /// current share value, computes the ratio delta vs this
    /// snapshot, and uses it to credit supply-side yield onto idle
    /// principal. Initialized at first accrue_risk_profile call
    /// (genesis); zero means "uninitialized — set on first call
    /// without computing yield."
    pub last_supply_share_value_fp48: u128, // 176..192

    /// Staged successor curator set by `transfer_curator`. Finalized by
    /// `accept_curator` (signer = pending_curator). Per-profile —
    /// `Pubkey::default()` means "no pending transfer".
    pub pending_curator: Pubkey, // 192..224

    /// Markets this profile currently holds a seat in. Indexed by
    /// `(0..allowed_market_count)`; trailing slots default-initialised.
    /// Bounded at 8 by the design's `allowed_market_max` cap. Populated
    /// by `claim_seat_for_risk_profile`, drained by `release_seat_for_risk_profile`.
    /// Used by `sync_market_seats_for_risk_profile` to enumerate the
    /// market-side seats that need re-stamping after a curator runs
    /// `update_risk_profile` (e.g. tightens `max_ltv_bps`).
    pub active_markets: [Pubkey; 8], // 224..480

    /// Reserved budget. Split into chunks of ≤32 so `Default`/`Pod`
    /// auto-derive on `[T; N]` works (the toolchain caps at N=32).
    _reserved_a: [u64; 2], // 480..496
}
const_assert_eq!(size_of::<RiskProfile>(), RISK_PROFILE_BLOCK_PAYLOAD_SIZE);
const_assert_eq!(size_of::<RiskProfile>() % 16, 0);

impl RiskProfile {
    /// Tree-key getter. The `risk_profiles` tree sorts by `profile_id`
    /// ascending, so this is the comparison key.
    pub fn key(&self) -> u8 {
        self.profile_id
    }

    /// Fresh profile constructor used by `create_risk_profile`.
    /// Initializes risk policy + zeros all financial / yield-index
    /// state. `allowed_market_max` caps how many markets this profile
    /// can simultaneously hold seats in.
    pub fn new_empty(
        profile_id: u8,
        curator: Pubkey,
        max_ltv_bps: u16,
        max_term_seconds: u32,
        allowed_market_max: u8,
    ) -> Self {
        Self {
            profile_id,
            _pad0: [0; 7],
            curator,
            max_ltv_bps,
            _pad1: [0; 2],
            max_term_seconds,
            allowed_market_count: 0,
            allowed_market_max,
            _pad2: [0; 14],
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
            last_supply_share_value_fp48: 0,
            pending_curator: Pubkey::default(),
            active_markets: [Pubkey::default(); 8],
            _reserved_a: [0; 2],
        }
    }

    /// Hard cap on the active_markets array.
    pub const ACTIVE_MARKETS_MAX: u8 = 8;

    /// Append `market` to `active_markets`. Idempotent — a no-op if the
    /// market is already present. Returns Err when the array is full
    /// (`allowed_market_count == ACTIVE_MARKETS_MAX`).
    pub fn push_active_market(&mut self, market: Pubkey) -> ProgramResult {
        if self.active_markets[..self.allowed_market_count as usize].contains(&market) {
            return Ok(());
        }
        require!(
            self.allowed_market_count < Self::ACTIVE_MARKETS_MAX,
            crate::program::YdeltaError::VaultProfileAllowedMarketsExceeded,
            "risk_profile.active_markets is full ({} markets)",
            self.allowed_market_count
        )?;
        self.active_markets[self.allowed_market_count as usize] = market;
        self.allowed_market_count += 1;
        Ok(())
    }

    /// Remove `market` from `active_markets` (swap-remove with the
    /// trailing slot). Idempotent — a no-op if the market isn't
    /// present.
    pub fn remove_active_market(&mut self, market: &Pubkey) {
        let count = self.allowed_market_count as usize;
        if let Some(idx) = self.active_markets[..count]
            .iter()
            .position(|m| m == market)
        {
            // Swap with the last filled slot, then default the tail.
            self.active_markets[idx] = self.active_markets[count - 1];
            self.active_markets[count - 1] = Pubkey::default();
            self.allowed_market_count -= 1;
        }
    }
}

impl PartialEq for RiskProfile {
    fn eq(&self, other: &Self) -> bool {
        self.profile_id == other.profile_id
    }
}
impl Eq for RiskProfile {}
impl Ord for RiskProfile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.profile_id.cmp(&other.profile_id)
    }
}
impl PartialOrd for RiskProfile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for RiskProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RiskProfile(id={}, curator={})",
            self.profile_id, self.curator
        )
    }
}

// ─────────────────── RiskProfileDepositorSeat (tree node) ───────────────────

/// A user's stake in a single vault profile. One
/// `RiskProfileDepositorSeat` per `(owner=user, profile_id)` — a
/// single user can hold seats in multiple profiles inside the same
/// vault. Lives in the vault's `claimed_seats` tree.
///
/// **Authoritative balance for the user's stake.** `global_vault_deposit`
/// writes here as primary; `UserAccountFixed.VaultPosition` carries
/// a downstream mirror so user-account-side dashboards stay coherent
/// without reading the vault account.
///
/// Yield-index snapshots crystallize the user's per-share growth
/// against `RiskProfile.cumulative_*_yield_index_scaled` at each
/// deposit/withdraw boundary.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfileDepositorSeat {
    pub owner: Pubkey,                            // 0..32
    pub profile_id: u8,                           // 32
    _pad0: [u8; 15],                              // 33..48  — align u128 to 16
    pub shares: u128,                             // 48..64
    pub snapshot_supply_yield_index_scaled: u128, // 64..80
    pub snapshot_delta_yield_index_scaled: u128,  // 80..96
    pub last_updated_unix: i64,                   // 96..104
    _padding: [u8; 8],                            // 104..112
    /// Reserved budget. 32 bytes of headroom from the 144-byte payload.
    _reserved: [u64; 4], // 112..144
}
const_assert_eq!(
    size_of::<RiskProfileDepositorSeat>(),
    VAULT_CLAIMED_SEAT_SIZE
);
const_assert_eq!(size_of::<RiskProfileDepositorSeat>() % 8, 0);

impl RiskProfileDepositorSeat {
    /// Probe constructor for tree lookups by `(owner, profile_id)`.
    pub fn probe(owner: Pubkey, profile_id: u8) -> Self {
        Self {
            owner,
            profile_id,
            ..Default::default()
        }
    }
}

impl PartialEq for RiskProfileDepositorSeat {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner && self.profile_id == other.profile_id
    }
}
impl Eq for RiskProfileDepositorSeat {}
impl Ord for RiskProfileDepositorSeat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.owner.cmp(&other.owner) {
            std::cmp::Ordering::Equal => self.profile_id.cmp(&other.profile_id),
            ord => ord,
        }
    }
}
impl PartialOrd for RiskProfileDepositorSeat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl std::fmt::Display for RiskProfileDepositorSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RiskProfileDepositorSeat(owner={}, profile_id={})",
            self.owner, self.profile_id
        )
    }
}

// ─────────────────── RiskProfileOrderRef (tree node) ───────────────────

/// `RiskProfileOrderRef` — one per live `(vault, profile_id, market)` order.
/// Composite key `(market, profile_id)` enforces one live order per
/// profile per market.
///
/// There is no per-order remaining-principal field. Matching reads
/// profile liquidity and market exposure limits at fill time.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfileOrderRef {
    pub market: Pubkey, // 0..32   composite key part 1
    pub profile_id: u8, // 32..33  composite key part 2
    /// `0 = Bid, 1 = Ask`. Vaults always Ask in v1 (debt-side only),
    /// but the field stays for forward compat / log clarity.
    pub side: u8, // 33..34
    _pad0: [u8; 2],     // 34..36
    pub rate_bps: u16,  // 36..38
    _pad1: [u8; 2],     // 38..40
    pub term_seconds: u32, // 40..44
    _pad2: [u8; 4],     // 44..48 (align i64 below)
    /// `RestingOrder.sequence_number` for cross-referencing the
    /// market-side order. Renewed on `update_order_for_risk_profile`.
    pub order_sequence_in_market: u64, // 48..56
    pub placed_at_unix: i64, // 56..64

    /// Reserved budget; ten u64 of headroom from the 144-byte payload.
    _reserved: [u64; 10], // 64..144
}
const_assert_eq!(size_of::<RiskProfileOrderRef>(), VAULT_ORDER_REF_SIZE);
const_assert_eq!(size_of::<RiskProfileOrderRef>() % 8, 0);

impl RiskProfileOrderRef {
    /// Probe constructor for tree lookups by `(market, profile_id)`.
    pub fn probe(market: Pubkey, profile_id: u8) -> Self {
        Self {
            market,
            profile_id,
            side: 0,
            _pad0: [0; 2],
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

impl PartialEq for RiskProfileOrderRef {
    fn eq(&self, other: &Self) -> bool {
        self.market == other.market && self.profile_id == other.profile_id
    }
}
impl Eq for RiskProfileOrderRef {}
impl Ord for RiskProfileOrderRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.market.cmp(&other.market) {
            std::cmp::Ordering::Equal => self.profile_id.cmp(&other.profile_id),
            ord => ord,
        }
    }
}
impl PartialOrd for RiskProfileOrderRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for RiskProfileOrderRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RiskProfileOrderRef(market={}, profile_id={}, side={})",
            self.market, self.profile_id, self.side
        )
    }
}

// ─────────────────── Tree typedefs ───────────────────

/// `risk_profiles` tree — keyed by `profile_id`, payload `RiskProfile`.
pub type RiskProfileTree<'a> = RedBlackTree<'a, RiskProfile>;
pub type RiskProfileTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, RiskProfile>;

/// `claimed_seats` tree — keyed by `(owner, profile_id)`, payload
/// `RiskProfileDepositorSeat`. Holds the authoritative per-user stake
/// in each profile.
pub type RiskProfileDepositorSeatTree<'a> = RedBlackTree<'a, RiskProfileDepositorSeat>;
pub type RiskProfileDepositorSeatTreeReadOnly<'a> =
    RedBlackTreeReadOnly<'a, RiskProfileDepositorSeat>;

/// `market_orders` tree — keyed by `(market, profile_id)`, payload
/// `RiskProfileOrderRef`.
pub type RiskProfileOrderRefTree<'a> = RedBlackTree<'a, RiskProfileOrderRef>;
pub type RiskProfileOrderRefTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, RiskProfileOrderRef>;

// ─────────────────── Per-tree node helpers ───────────────────

pub fn get_helper_risk_profile(data: &[u8], index: DataIndex) -> &RBNode<RiskProfile> {
    get_helper::<RBNode<RiskProfile>>(data, index)
}

pub fn get_mut_helper_risk_profile(data: &mut [u8], index: DataIndex) -> &mut RBNode<RiskProfile> {
    get_mut_helper::<RBNode<RiskProfile>>(data, index)
}

pub fn get_helper_risk_profile_depositor_seat(
    data: &[u8],
    index: DataIndex,
) -> &RBNode<RiskProfileDepositorSeat> {
    get_helper::<RBNode<RiskProfileDepositorSeat>>(data, index)
}

pub fn get_mut_helper_risk_profile_depositor_seat(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<RiskProfileDepositorSeat> {
    get_mut_helper::<RBNode<RiskProfileDepositorSeat>>(data, index)
}

pub fn get_helper_risk_profile_order_ref(
    data: &[u8],
    index: DataIndex,
) -> &RBNode<RiskProfileOrderRef> {
    get_helper::<RBNode<RiskProfileOrderRef>>(data, index)
}

pub fn get_mut_helper_risk_profile_order_ref(
    data: &mut [u8],
    index: DataIndex,
) -> &mut RBNode<RiskProfileOrderRef> {
    get_mut_helper::<RBNode<RiskProfileOrderRef>>(data, index)
}

// ─────────────────── Accrual ───────────────────

/// fp48 SCALE used for the cumulative yield indices (Aave-style fixed-point).
const ACCRUE_INDEX_SCALE: u128 = 1u128 << 48;

/// Read the marginfi bank's `asset_share_value` as a fp48 u128 for
/// `accrue_risk_profile`'s supply-yield delta computation. Returns 0
/// on bank-data parse failure (caller treats as "skip supply yield"
/// — non-fatal; `accrue_risk_profile` no-ops the supply stream when
/// 0 is passed).
pub fn read_bank_asset_share_value_fp48(
    bank_ai: &solana_program::account_info::AccountInfo,
) -> u128 {
    let data = match bank_ai.try_borrow_data() {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let bank = match marginfi_mocks::state::Bank::try_from_account_data(&data) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    crate::protocol::marginfi::wrapped_i80f48_to_u128(bank.asset_share_value)
}

/// O(1) per-profile accrual. Updates both yield indices and
/// `total_assets_atoms` from `last_accrue_unix` to `now`.
///
/// `current_share_value_fp48` is marginfi's `Bank.asset_share_value`
/// read at the call site. Supply yield is derived from the
/// share-value ratio delta (`current / last_snapshot - 1`) applied to
/// the profile's idle atoms. Pass `0` to skip supply yield (e.g. when
/// the bank read isn't available).
///
/// Math:
/// ```
/// idle = total_principal - deployed - encumbered_in_orders
///
/// // Supply yield (only if snapshot exists; first call seeds it).
/// growth_fp48 = (current_share_value_fp48 << 48) / snapshot - SCALE
/// idle_yield = idle × growth_fp48 / SCALE
///
/// // Lender-rate yield: O(1) via running aggregate.
/// loan_yield = total_weighted_rate_bps × elapsed / SECONDS_PER_YEAR / 10_000
///
/// total_assets += idle_yield + loan_yield
/// supply_index += (idle_yield × SCALE) / total_shares
/// delta_index  += (loan_yield × SCALE) / total_shares
/// ```
///
/// **Loan-iteration-free.** `total_weighted_rate_bps` already captures
/// `Σ(P_i × R_i)` across all open loans (updated at match / close
/// events), so a single multiply yields the entire book's per-second
/// lender-rate atoms.
///
/// No-op when `now <= profile.last_accrue_unix` or
/// `total_principal_atoms == 0`.
pub fn accrue_risk_profile(
    profile: &mut RiskProfile,
    now: i64,
    current_share_value_fp48: u128,
) -> ProgramResult {
    if now <= profile.last_accrue_unix {
        return Ok(());
    }
    if profile.total_principal_atoms == 0 {
        // First-touch: stamp baselines without computing yield.
        // Refresh `last_supply_share_value_fp48` unconditionally on
        // every zero-principal touch (not just when the snapshot is
        // zero). Otherwise a profile that fully drains and later
        // re-deposits would attribute the entire interim share-value
        // drift to the new depositors as supply yield on the next
        // non-zero accrue.
        profile.last_accrue_unix = now;
        if current_share_value_fp48 > 0 {
            profile.last_supply_share_value_fp48 = current_share_value_fp48;
        }
        return Ok(());
    }

    let elapsed: u128 = (now - profile.last_accrue_unix) as u128;
    let denom: u128 = (super::loan::BPS_PER_UNIT as u128)
        .checked_mul(super::loan::SECONDS_PER_YEAR as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // ─── Stream 1: supply yield from marginfi share-value delta ───
    let idle: u64 = profile
        .total_principal_atoms
        .saturating_sub(profile.deployed_principal_atoms)
        .saturating_sub(profile.encumbered_in_orders_atoms);
    let idle_yield_atoms: u128 = if idle > 0 && current_share_value_fp48 > 0 {
        let snapshot = profile.last_supply_share_value_fp48;
        if snapshot == 0 {
            // First call with a real share value — seed the snapshot,
            // skip yield (no baseline to diff against).
            0
        } else if current_share_value_fp48 <= snapshot {
            // Share value didn't grow (or briefly retraced via marginfi
            // bank state). Don't credit negative yield.
            0
        } else {
            // growth_fp48 = (current * SCALE) / snapshot - SCALE.
            let scale: u128 = ACCRUE_INDEX_SCALE;
            let ratio_fp48 = current_share_value_fp48
                .checked_mul(scale)
                .and_then(|x| x.checked_div(snapshot))
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let growth_fp48 = ratio_fp48.saturating_sub(scale);
            (idle as u128)
                .checked_mul(growth_fp48)
                .and_then(|x| x.checked_div(scale))
                .ok_or(ProgramError::ArithmeticOverflow)?
        }
    } else {
        0
    };

    // ─── Stream 2: lender-rate yield on the entire deployed book ───
    // `total_weighted_rate_bps` = Σ(loan_principal × loan_rate_bps).
    // Period yield = total_weighted_rate × elapsed / (BPS × YEAR).
    let loan_yield_atoms: u128 = profile
        .total_weighted_rate_bps
        .checked_mul(elapsed)
        .and_then(|x| x.checked_div(denom))
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let total_yield_atoms = idle_yield_atoms
        .checked_add(loan_yield_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if total_yield_atoms > u64::MAX as u128 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    profile.total_assets_atoms = profile
        .total_assets_atoms
        .checked_add(total_yield_atoms as u64)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // ─── Update cumulative indices for per-depositor crystallisation ───
    // Surface overflow as a hard error rather than silently dropping
    // index growth — under-crediting depositors is a load-bearing
    // accounting failure.
    if profile.total_shares > 0 {
        let supply_growth = idle_yield_atoms
            .checked_mul(ACCRUE_INDEX_SCALE)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_div(profile.total_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.cumulative_supply_yield_index_scaled = profile
            .cumulative_supply_yield_index_scaled
            .saturating_add(supply_growth);

        let delta_growth = loan_yield_atoms
            .checked_mul(ACCRUE_INDEX_SCALE)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_div(profile.total_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.cumulative_delta_yield_index_scaled = profile
            .cumulative_delta_yield_index_scaled
            .saturating_add(delta_growth);
    }

    profile.last_accrue_unix = now;
    if current_share_value_fp48 > 0 {
        profile.last_supply_share_value_fp48 = current_share_value_fp48;
    }
    Ok(())
}

// ─────────────────── Free-list management ───────────────────

use hypertree::FreeList;

use super::constants::{RISK_PROFILE_BLOCK_SIZE, VAULT_NODE_BLOCK_SIZE};

// Compile-time invariants — fail fast if anyone touches the constants
// without re-checking the layout below.
const _: () = {
    assert!(VAULT_CLAIMED_SEAT_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);
    assert!(VAULT_ORDER_REF_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);
    // RiskProfile uses its own (larger) block. The other two share
    // a 128-byte block free list.
    assert!(RISK_PROFILE_BLOCK_PAYLOAD_SIZE != VAULT_NODE_BLOCK_PAYLOAD_SIZE);
};

/// Pop a free 512-byte block off the vault's `profile_free_list_head_index`
/// list. Used to allocate `RiskProfile` nodes.
pub fn get_free_profile_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
) -> DataIndex {
    let mut free_list: FreeList<ProfileUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.profile_free_list_head_index);
    let free_address: DataIndex = free_list.remove();
    fixed.profile_free_list_head_index = free_list.get_head();
    free_address
}

/// Return a freed 512-byte profile block to the profile free list.
pub fn release_profile_address_on_vault_fixed(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    index: DataIndex,
) {
    let mut free_list: FreeList<ProfileUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.profile_free_list_head_index);
    free_list.add(index);
    fixed.profile_free_list_head_index = free_list.get_head();
}

/// Pop a free 128-byte block off the vault's
/// `node_free_list_head_index` list. Used to allocate
/// `RiskProfileDepositorSeat` and `RiskProfileOrderRef` nodes (they
/// share the smaller block size).
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

/// Return a freed 128-byte node block to the node free list.
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

/// Grow the vault's dynamic region by one 512-byte profile block and
/// link it onto the profile free list. Caller is responsible for
/// calling `realloc(num_bytes_allocated)` on the account before this;
/// this function only updates header bookkeeping.
pub fn vault_expand_profile_block(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
) -> ProgramResult {
    let mut free_list: FreeList<ProfileUnusedFreeListPadding> =
        FreeList::new(dynamic, fixed.profile_free_list_head_index);
    free_list.add(fixed.num_bytes_allocated);
    fixed.num_bytes_allocated = fixed
        .num_bytes_allocated
        .checked_add(RISK_PROFILE_BLOCK_SIZE as u32)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    fixed.profile_free_list_head_index = free_list.get_head();
    Ok(())
}

/// Grow the vault's dynamic region by one 128-byte node block and
/// link it onto the node free list. Same pattern as
/// `vault_expand_profile_block`.
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

// ─────────────────── Tree mutation helpers ───────────────────

/// Upsert a `RiskProfileDepositorSeat` for `(owner, profile_id)`. If a node
/// already exists, returns its index (caller mutates `shares` /
/// `snapshot_*` directly). Otherwise allocates a free vault-node
/// block and inserts a fresh zero-share entry.
pub fn upsert_risk_profile_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
    use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations};
    let probe = RiskProfileDepositorSeat::probe(owner, profile_id);
    let existing_idx = {
        let tree =
            RiskProfileDepositorSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
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
    let mut tree = RiskProfileDepositorSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
    tree.insert(order_index, probe);
    fixed.claimed_seats_root_index = tree.get_root_index();
    drop(tree);
    fixed.claimed_seat_count = fixed.claimed_seat_count.saturating_add(1);
    Ok(order_index)
}

/// Remove a `RiskProfileDepositorSeat` for `(owner, profile_id)`. Returns
/// the freed data-index, or `NIL` if none existed. Releases the block
/// back to the node free list (symmetric with `remove_risk_profile_order_ref`).
pub fn remove_risk_profile_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    profile_id: u8,
) -> DataIndex {
    use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations};
    let probe = RiskProfileDepositorSeat::probe(owner, profile_id);
    let existing_idx = {
        let tree =
            RiskProfileDepositorSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx == NIL {
        return NIL;
    }
    let mut tree = RiskProfileDepositorSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
    tree.remove_by_index(existing_idx);
    fixed.claimed_seats_root_index = tree.get_root_index();
    drop(tree);
    fixed.claimed_seat_count = fixed.claimed_seat_count.saturating_sub(1);
    release_node_address_on_vault_fixed(fixed, dynamic, existing_idx);
    existing_idx
}

/// Insert a fresh `RiskProfileOrderRef` for `(market, profile_id)`.
/// Errors with `VaultProfileOrderExists` if one already exists.
#[allow(clippy::too_many_arguments)]
pub fn insert_risk_profile_order_ref(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    profile_id: u8,
    side: u8,
    rate_bps: u16,
    term_seconds: u32,
    order_sequence_in_market: u64,
    placed_at_unix: i64,
) -> Result<DataIndex, ProgramError> {
    use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations};

    let probe = RiskProfileOrderRef {
        market,
        profile_id,
        ..Default::default()
    };
    let existing_idx = {
        let tree =
            RiskProfileOrderRefTreeReadOnly::new(dynamic, fixed.market_orders_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        existing_idx == NIL,
        crate::program::YdeltaError::VaultProfileOrderExists,
        "RiskProfileOrderRef already exists for (market={}, profile_id={})",
        market,
        profile_id
    )?;

    let order_index = get_free_node_address_on_vault_fixed(fixed, dynamic);
    require!(
        order_index != NIL,
        ProgramError::AccountDataTooSmall,
        "no free vault-node block"
    )?;

    let order = RiskProfileOrderRef {
        market,
        profile_id,
        side,
        _pad0: [0; 2],
        rate_bps,
        _pad1: [0; 2],
        term_seconds,
        _pad2: [0; 4],
        order_sequence_in_market,
        placed_at_unix,
        _reserved: [0; 10],
    };
    let mut tree = RiskProfileOrderRefTree::new(dynamic, fixed.market_orders_root_index, NIL);
    tree.insert(order_index, order);
    fixed.market_orders_root_index = tree.get_root_index();
    drop(tree);
    fixed.open_order_count = fixed.open_order_count.saturating_add(1);
    Ok(order_index)
}

/// Remove the `RiskProfileOrderRef` for `(market, profile_id)`.
/// Returns the freed data-index, or NIL if not present.
pub fn remove_risk_profile_order_ref(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    profile_id: u8,
) -> DataIndex {
    use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations};

    let probe = RiskProfileOrderRef {
        market,
        profile_id,
        ..Default::default()
    };
    let idx = {
        let tree =
            RiskProfileOrderRefTreeReadOnly::new(dynamic, fixed.market_orders_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return NIL;
    }
    let mut tree = RiskProfileOrderRefTree::new(dynamic, fixed.market_orders_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.market_orders_root_index = tree.get_root_index();
    drop(tree);
    fixed.open_order_count = fixed.open_order_count.saturating_sub(1);
    release_node_address_on_vault_fixed(fixed, dynamic, idx);
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_fixed_size_locked() {
        assert_eq!(size_of::<GlobalVaultFixed>(), GLOBAL_VAULT_FIXED_SIZE);
    }

    #[test]
    fn risk_profile_size_locked() {
        assert_eq!(size_of::<RiskProfile>(), RISK_PROFILE_BLOCK_PAYLOAD_SIZE);
    }

    #[test]
    fn risk_profile_order_ref_size_locked() {
        assert_eq!(size_of::<RiskProfileOrderRef>(), VAULT_ORDER_REF_SIZE);
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

    fn fresh_profile() -> RiskProfile {
        RiskProfile::new_empty(7, Pubkey::default(), 5_000, 30 * 86_400, 8)
    }

    /// fp48 representation of 1.0 — marginfi's `asset_share_value` at
    /// genesis (no yield accrued).
    const SHARE_VALUE_ONE: u128 = 1u128 << 48;

    #[test]
    fn accrue_zero_elapsed_is_noop() {
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 1_000;
        accrue_risk_profile(&mut p, 1_000, SHARE_VALUE_ONE).unwrap();
        assert_eq!(p.total_assets_atoms, 1_000_000);
        assert_eq!(p.last_accrue_unix, 1_000);
    }

    #[test]
    fn accrue_supply_yield_from_share_value_delta() {
        // 1M idle, share value grows 1% over the period → 10k atoms yield.
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        // current = 1.01 in fp48
        let current = SHARE_VALUE_ONE + (SHARE_VALUE_ONE / 100);
        accrue_risk_profile(&mut p, 86_400, current).unwrap();
        // 1% growth on 1M idle = ~10_000 atoms.
        let yield_atoms = p.total_assets_atoms - 1_000_000;
        assert!(yield_atoms >= 9_990 && yield_atoms <= 10_010);
    }

    #[test]
    fn accrue_loan_yield_uses_total_weighted_rate_aggregate() {
        // 1M total, 100k deployed at 8%. Pass 0 share value to skip
        // supply stream — only test the lender-rate aggregate.
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        accrue_risk_profile(&mut p, 86_400, 0).unwrap();
        let expected = (100_000u128 * 800 * 86_400) / (10_000 * 31_536_000);
        assert_eq!(p.total_assets_atoms - 1_000_000, expected as u64);
    }

    #[test]
    fn accrue_idempotent_when_run_again_at_same_now() {
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        accrue_risk_profile(&mut p, 86_400, SHARE_VALUE_ONE).unwrap();
        let assets_after_first = p.total_assets_atoms;
        // Second call at same `now` is a no-op.
        accrue_risk_profile(&mut p, 86_400, SHARE_VALUE_ONE).unwrap();
        assert_eq!(p.total_assets_atoms, assets_after_first);
    }

    #[test]
    fn accrue_two_calls_match_one_call_within_tolerance() {
        // accrue 30 days in one shot vs 15 + 15 — supply-side share
        // value passed unchanged, lender-rate aggregate is linear in
        // elapsed. Should agree modulo truncation.
        let mut a = fresh_profile();
        a.total_shares = 1_000_000;
        a.total_principal_atoms = 1_000_000;
        a.deployed_principal_atoms = 100_000;
        a.total_weighted_rate_bps = 100_000u128 * 800;
        a.total_assets_atoms = 1_000_000;
        a.last_accrue_unix = 0;
        a.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        let mut b = a;
        accrue_risk_profile(&mut a, 30 * 86_400, SHARE_VALUE_ONE).unwrap();
        accrue_risk_profile(&mut b, 15 * 86_400, SHARE_VALUE_ONE).unwrap();
        accrue_risk_profile(&mut b, 30 * 86_400, SHARE_VALUE_ONE).unwrap();
        let diff = (a.total_assets_atoms as i64 - b.total_assets_atoms as i64).abs();
        assert!(
            diff <= 1,
            "single-call vs two-call diverge by {} atoms",
            diff
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
        assert_eq!(vault.risk_profiles_root_index, NIL);
        assert_eq!(vault.claimed_seats_root_index, NIL);
        assert_eq!(vault.market_orders_root_index, NIL);
        assert_eq!(vault.profile_free_list_head_index, NIL);
        assert_eq!(vault.node_free_list_head_index, NIL);
        assert_eq!(vault.risk_profile_count, 0);
        assert_eq!(vault.claimed_seat_count, 0);
        assert_eq!(vault.open_order_count, 0);
        assert_eq!(vault.global_vault_signer_bump, 255);
        assert!(!vault.has_free_profile_block());
        assert!(!vault.has_free_node_block());
    }
}
