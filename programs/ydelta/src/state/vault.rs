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
    GLOBAL_VAULT_FIXED_DISCRIMINANT, GLOBAL_VAULT_FIXED_SIZE, RISK_PROFILE_BLOCK_PAYLOAD_SIZE,
    RISK_PROFILE_BLOCK_SIZE, VAULT_CLAIMED_SEAT_SIZE, VAULT_NODE_BLOCK_PAYLOAD_SIZE,
    VAULT_NODE_BLOCK_SIZE, VAULT_ORDER_REF_SIZE,
};

pub const VAULT_SEED: &[u8] = b"vault";

pub const GLOBAL_VAULT_SIGNER_SEED: &[u8] = b"global_vault_signer";

pub const VAULT_INTEGRATION_SEED: &[u8] = b"vault_integration";

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

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct GlobalVaultFixed {
    pub discriminator: u64,
    pub mint: Pubkey,
    pub global_vault_admin: Pubkey,
    pub integration_pool: Pubkey,
    pub integration_account: Pubkey,
    pub global_vault_signer: Pubkey,

    pub lending_pool: Pubkey,

    pub risk_profiles_root_index: DataIndex,
    pub claimed_seats_root_index: DataIndex,
    pub market_orders_root_index: DataIndex,

    pub profile_free_list_head_index: DataIndex,
    pub node_free_list_head_index: DataIndex,

    pub num_bytes_allocated: u32,

    pub risk_profile_count: u8,
    pub global_vault_signer_bump: u8,

    pub version: u8,
    _pad0: [u8; 1],
    pub claimed_seat_count: u32,
    pub open_order_count: u32,
    _pad1: [u8; 4],

    pub pending_global_vault_admin: Pubkey,

    _reserved_aggregates: [u64; 4],

    pub is_paused: u8,

    pub next_profile_id: u8,
    _pad2: [u8; 6],

    _reserved: [u64; 1],
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
            version: crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            _pad0: [0; 1],
            claimed_seat_count: 0,
            open_order_count: 0,
            _pad1: [0; 4],
            pending_global_vault_admin: Pubkey::default(),
            _reserved_aggregates: [0; 4],
            is_paused: 0,
            next_profile_id: 0,
            _pad2: [0; 6],
            _reserved: [0; 1],
        }
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused != 0
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

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfile {
    pub profile_id: u8,
    _pad0: [u8; 7],

    pub curator: Pubkey,

    pub max_ltv_bps: u16,
    _pad1: [u8; 2],
    pub max_term_seconds: u32,

    _pad2: [u8; 16],

    pub total_shares: u128,
    pub total_assets_atoms: u64,
    pub total_principal_atoms: u64,
    pub deployed_principal_atoms: u64,
    pub encumbered_in_orders_atoms: u64,

    pub total_weighted_rate_bps: u128,

    pub accumulated_curator_fee_atoms: u64,
    pub last_accrue_unix: i64,

    pub cumulative_supply_yield_index_scaled: u128,

    pub cumulative_delta_yield_index_scaled: u128,

    pub last_supply_share_value_fp48: u128,

    pub pending_curator: Pubkey,

    pub total_weighted_net_rate_bps: u128,

    /// Atoms that have been retired by repay/liquidation/settle-matured but
    /// not yet swept by the curator's `claim_repayment_for_risk_profile`.
    /// They physically live in the per-market `lender_marginfi_account`,
    /// not in this vault's `global_vault_integration_account`, so they must
    /// be EXCLUDED from the `accrue_risk_profile` idle MTM calc — otherwise
    /// they'd appear to earn the integration-account's share-value drift
    /// while sitting in a different account.
    ///
    /// Lifecycle: `repay` (full-repay close-out) bumps this by the closed
    /// loan's `principal_debt_atoms`. `claim_repayment_for_risk_profile`
    /// decrements it by the atoms it physically moves from the per-market
    /// `lender_marginfi_account` into this vault's integration account.
    pub pending_claim_atoms: u64,

    _reserved_a: [u64; 29],
    _reserved_b: [u64; 2],
}
const_assert_eq!(size_of::<RiskProfile>(), RISK_PROFILE_BLOCK_PAYLOAD_SIZE);
const_assert_eq!(size_of::<RiskProfile>() % 16, 0);

impl RiskProfile {
    pub fn key(&self) -> u8 {
        self.profile_id
    }

    pub fn is_empty(&self) -> bool {
        self.total_shares == 0
            && self.total_assets_atoms == 0
            && self.total_principal_atoms == 0
            && self.deployed_principal_atoms == 0
            && self.encumbered_in_orders_atoms == 0
            && self.accumulated_curator_fee_atoms == 0
            && self.pending_claim_atoms == 0
    }

    pub fn new_empty(
        profile_id: u8,
        curator: Pubkey,
        max_ltv_bps: u16,
        max_term_seconds: u32,
    ) -> Self {
        Self {
            profile_id,
            _pad0: [0; 7],
            curator,
            max_ltv_bps,
            _pad1: [0; 2],
            max_term_seconds,
            _pad2: [0; 16],
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
            total_weighted_net_rate_bps: 0,
            pending_claim_atoms: 0,
            _reserved_a: [0; 29],
            _reserved_b: [0; 2],
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

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfileDepositorSeat {
    pub owner: Pubkey,
    pub profile_id: u8,
    _pad0: [u8; 15],
    pub shares: u128,
    pub snapshot_supply_yield_index_scaled: u128,
    pub snapshot_delta_yield_index_scaled: u128,
    pub last_updated_unix: i64,
    _padding: [u8; 8],

    _reserved: [u64; 4],
}
const_assert_eq!(
    size_of::<RiskProfileDepositorSeat>(),
    VAULT_CLAIMED_SEAT_SIZE
);
const_assert_eq!(size_of::<RiskProfileDepositorSeat>() % 8, 0);

impl RiskProfileDepositorSeat {
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

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct RiskProfileOrderRef {
    pub market: Pubkey,
    pub profile_id: u8,

    pub side: u8,
    _pad0: [u8; 2],
    pub rate_bps: u16,
    _pad1: [u8; 2],
    pub term_seconds: u32,
    _pad2: [u8; 4],

    pub order_sequence_in_market: u64,
    pub placed_at_unix: i64,

    _reserved: [u64; 10],
}
const_assert_eq!(size_of::<RiskProfileOrderRef>(), VAULT_ORDER_REF_SIZE);
const_assert_eq!(size_of::<RiskProfileOrderRef>() % 8, 0);

impl RiskProfileOrderRef {
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

pub type RiskProfileTree<'a> = RedBlackTree<'a, RiskProfile>;
pub type RiskProfileTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, RiskProfile>;

pub type RiskProfileDepositorSeatTree<'a> = RedBlackTree<'a, RiskProfileDepositorSeat>;
pub type RiskProfileDepositorSeatTreeReadOnly<'a> =
    RedBlackTreeReadOnly<'a, RiskProfileDepositorSeat>;

pub type RiskProfileOrderRefTree<'a> = RedBlackTree<'a, RiskProfileOrderRef>;
pub type RiskProfileOrderRefTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, RiskProfileOrderRef>;

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

const ACCRUE_INDEX_SCALE: u128 = 1u128 << 48;

pub fn read_bank_asset_share_value_fp48(
    bank_ai: &solana_program::account_info::AccountInfo,
) -> Result<u128, ProgramError> {
    let data = bank_ai
        .try_borrow_data()
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
        .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
    crate::protocol::marginfi::wrapped_i80f48_to_u128(bank.asset_share_value)
}

pub fn accrue_risk_profile(
    profile: &mut RiskProfile,
    now: i64,
    current_share_value_fp48: u128,
) -> ProgramResult {
    if now <= profile.last_accrue_unix {
        return Ok(());
    }
    if profile.total_principal_atoms == 0 {
        profile.last_accrue_unix = now;
        if current_share_value_fp48 > 0 {
            profile.last_supply_share_value_fp48 = current_share_value_fp48;
        }
        return Ok(());
    }

    let elapsed: u128 = now
        .checked_sub(profile.last_accrue_unix)
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
    let idle: u64 = profile
        .total_principal_atoms
        .checked_sub(profile.deployed_principal_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?
        .checked_sub(profile.pending_claim_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let idle_delta_atoms: i128 = if idle > 0 && current_share_value_fp48 > 0 {
        let snapshot = profile.last_supply_share_value_fp48;
        if snapshot == 0 {
            0
        } else {
            let current_idle_value = crate::math::mul_div(
                idle as u128,
                current_share_value_fp48,
                snapshot,
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
        profile.total_weighted_net_rate_bps,
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
        profile.total_assets_atoms = profile
            .total_assets_atoms
            .checked_add(idle_gain)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_principal_atoms = profile
            .total_principal_atoms
            .checked_add(idle_gain)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    } else {
        let idle_loss: u64 = idle_delta_atoms
            .checked_neg()
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(crate::program::YdeltaError::MathOverflow)?;
        profile.total_assets_atoms = profile.total_assets_atoms.saturating_sub(idle_loss);
        profile.total_principal_atoms = profile.total_principal_atoms.saturating_sub(idle_loss);
    }
    if loan_yield_atoms > u64::MAX as u128 {
        return Err(crate::program::YdeltaError::MathOverflow.into());
    }
    profile.total_assets_atoms = profile
        .total_assets_atoms
        .checked_add(loan_yield_atoms as u64)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    if profile.total_shares > 0 {
        if idle_delta_atoms > 0 {
            let supply_growth = crate::math::mul_div(
                idle_delta_atoms as u128,
                ACCRUE_INDEX_SCALE,
                profile.total_shares,
                false,
            )?;
            profile.cumulative_supply_yield_index_scaled = profile
                .cumulative_supply_yield_index_scaled
                .checked_add(supply_growth)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        let delta_growth = crate::math::mul_div(
            loan_yield_atoms,
            ACCRUE_INDEX_SCALE,
            profile.total_shares,
            false,
        )?;
        profile.cumulative_delta_yield_index_scaled = profile
            .cumulative_delta_yield_index_scaled
            .checked_add(delta_growth)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    profile.last_accrue_unix = now;
    if current_share_value_fp48 > 0 {
        profile.last_supply_share_value_fp48 = current_share_value_fp48;
    }
    Ok(())
}

const _: () = {
    assert!(VAULT_CLAIMED_SEAT_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);
    assert!(VAULT_ORDER_REF_SIZE == VAULT_NODE_BLOCK_PAYLOAD_SIZE);

    assert!(RISK_PROFILE_BLOCK_PAYLOAD_SIZE != VAULT_NODE_BLOCK_PAYLOAD_SIZE);
};

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

pub fn upsert_risk_profile_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
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
    fixed.claimed_seat_count = fixed
        .claimed_seat_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(order_index)
}

pub fn remove_risk_profile_depositor_seat(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    owner: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = RiskProfileDepositorSeat::probe(owner, profile_id);
    let existing_idx = {
        let tree =
            RiskProfileDepositorSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if existing_idx == NIL {
        return Ok(NIL);
    }
    let mut tree = RiskProfileDepositorSeatTree::new(dynamic, fixed.claimed_seats_root_index, NIL);
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
    fixed.open_order_count = fixed
        .open_order_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(order_index)
}

pub fn remove_risk_profile(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
    let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
    let idx = {
        let tree = RiskProfileTreeReadOnly::new(dynamic, fixed.risk_profiles_root_index, NIL);
        tree.lookup_index(&probe)
    };
    if idx == NIL {
        return Ok(NIL);
    }
    {
        let profile = get_helper_risk_profile(dynamic, idx).get_value();
        require!(
            profile.is_empty(),
            crate::program::YdeltaError::InvalidArgument,
            "remove_risk_profile: profile {} not empty (deployed={}, shares={}, principal={})",
            profile_id,
            profile.deployed_principal_atoms,
            profile.total_shares,
            profile.total_principal_atoms,
        )?;
    }
    let mut tree = RiskProfileTree::new(dynamic, fixed.risk_profiles_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.risk_profiles_root_index = tree.get_root_index();
    drop(tree);
    fixed.risk_profile_count = fixed
        .risk_profile_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    release_profile_address_on_vault_fixed(fixed, dynamic, idx);
    Ok(idx)
}

pub fn remove_risk_profile_order_ref(
    fixed: &mut GlobalVaultFixed,
    dynamic: &mut [u8],
    market: Pubkey,
    profile_id: u8,
) -> Result<DataIndex, ProgramError> {
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
        return Ok(NIL);
    }
    let mut tree = RiskProfileOrderRefTree::new(dynamic, fixed.market_orders_root_index, NIL);
    tree.remove_by_index(idx);
    fixed.market_orders_root_index = tree.get_root_index();
    drop(tree);
    fixed.open_order_count = fixed
        .open_order_count
        .checked_sub(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    release_node_address_on_vault_fixed(fixed, dynamic, idx);
    Ok(idx)
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
        RiskProfile::new_empty(7, Pubkey::default(), 5_000, 30 * 86_400)
    }

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
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;

        let current = SHARE_VALUE_ONE + (SHARE_VALUE_ONE / 100);
        accrue_risk_profile(&mut p, 86_400, current).unwrap();

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
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        let current = SHARE_VALUE_ONE - (SHARE_VALUE_ONE / 100);
        accrue_risk_profile(&mut p, 86_400, current).unwrap();
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
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_weighted_net_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        accrue_risk_profile(&mut p, 86_400, 0).unwrap();
        let expected = (100_000u128 * 800 * 86_400) / (10_000 * 31_536_000);
        assert_eq!(p.total_assets_atoms - 1_000_000, expected as u64);
    }

    #[test]
    fn accrue_loan_yield_is_net_of_curator_fee_not_double_counted() {
        let curator_fee_bps: u128 = 2_000;
        let gross_weighted: u128 = 100_000u128 * 800;
        let net_weighted: u128 = gross_weighted * (10_000 - curator_fee_bps) / 10_000;

        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = gross_weighted;
        p.total_weighted_net_rate_bps = net_weighted;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        accrue_risk_profile(&mut p, 86_400, 0).unwrap();

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
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 100_000;
        p.total_weighted_rate_bps = 100_000u128 * 800;
        p.total_weighted_net_rate_bps = 100_000u128 * 800;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        accrue_risk_profile(&mut p, 86_400, SHARE_VALUE_ONE).unwrap();
        let assets_after_first = p.total_assets_atoms;

        accrue_risk_profile(&mut p, 86_400, SHARE_VALUE_ONE).unwrap();
        assert_eq!(p.total_assets_atoms, assets_after_first);
    }

    #[test]
    fn accrue_two_calls_match_one_call_within_tolerance() {
        let mut a = fresh_profile();
        a.total_shares = 1_000_000;
        a.total_principal_atoms = 1_000_000;
        a.deployed_principal_atoms = 100_000;
        a.total_weighted_rate_bps = 100_000u128 * 800;
        a.total_weighted_net_rate_bps = 100_000u128 * 800;
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
    fn accrue_supply_yield_includes_encumbered_atoms() {
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;
        p.deployed_principal_atoms = 0;
        p.encumbered_in_orders_atoms = 400_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        let current = SHARE_VALUE_ONE + (SHARE_VALUE_ONE / 100);
        accrue_risk_profile(&mut p, 86_400, current).unwrap();
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
            "encumbered idle-side yield must stay withdrawable by the profile"
        );
    }

    #[test]
    fn accrue_hard_fails_when_deployed_exceeds_principal() {
        let mut p = fresh_profile();
        p.total_shares = 1_000_000;
        p.total_principal_atoms = 1_000_000;

        p.deployed_principal_atoms = 1_500_000;
        p.total_assets_atoms = 1_000_000;
        p.last_accrue_unix = 0;
        p.last_supply_share_value_fp48 = SHARE_VALUE_ONE;
        let result = accrue_risk_profile(&mut p, 86_400, SHARE_VALUE_ONE);
        assert!(
            result.is_err(),
            "accrue_risk_profile must hard-fail when deployed > total_principal"
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
