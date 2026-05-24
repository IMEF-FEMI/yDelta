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

#[repr(C)]
#[derive(Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct FeeConfig {
    pub protocol_fee_bps_floor: u16,
    pub origination_bps: u16,
    pub curator_split_bps: u16,
    pub curator_fee_bps: u16,
    pub liquidation_keeper_bps: u16,
    pub liquidation_protocol_bps: u16,

    pub ltv_buffer_bps: u16,
    _padding_for_u32: [u8; 2],

    pub grace_period_seconds: u32,
    _padding_tail: [u8; 4],
}
const_assert_eq!(size_of::<FeeConfig>(), 24);
const_assert_eq!(size_of::<FeeConfig>() % 8, 0);

pub const DEFAULT_GRACE_PERIOD_SECONDS: u32 = 86_400;

pub const DEFAULT_LTV_BUFFER_BPS: u16 = 200;

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            protocol_fee_bps_floor: 0,
            origination_bps: 0,
            curator_split_bps: 0,
            curator_fee_bps: 0,
            liquidation_keeper_bps: 0,
            liquidation_protocol_bps: 0,
            ltv_buffer_bps: DEFAULT_LTV_BUFFER_BPS,
            _padding_for_u32: [0; 2],
            grace_period_seconds: DEFAULT_GRACE_PERIOD_SECONDS,
            _padding_tail: [0; 4],
        }
    }
}

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

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MatchedLoan {
    pub sequence: u64,
    _padding0: [u8; 8],
    pub borrower_marginfi_borrow_shares: u128,
    pub principal_atoms: u64,
    pub origination_atoms: u64,
    pub collateral_atoms: u64,
    pub matched_at_unix: i64,
    pub lender_seat_index: hypertree::DataIndex,
    pub borrower_seat_index: hypertree::DataIndex,
    pub term_seconds: u32,
    pub borrower_rate_bps: u16,
    pub lender_rate_bps: u16,
    pub loan_type: u8,
    pub flags: u8,

    _pad_before_p5: [u8; 6],

    pub curator_fee_bps_snapshot: u16,
    _reserved: [u8; 22],

    pub lender_debt_share_price_snapshot_fp48: u128,

    pub borrower_collateral_share_price_snapshot_fp48: u128,
}

const_assert_eq!(
    size_of::<MatchedLoan>(),
    super::constants::MATCHED_LOAN_SIZE
);
const_assert_eq!(size_of::<MatchedLoan>() % 16, 0);

pub const MATCHED_LOAN_FLAG_VAULT_LENDER: u8 = 0b0000_0001;

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

#[repr(C)]
#[derive(Default, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MarketFixed {
    pub discriminator: u64,

    pub version: u8,
    pub debt_mint_decimals: u8,
    pub collateral_mint_decimals: u8,
    pub debt_vault_bump: u8,
    pub collateral_vault_bump: u8,
    _padding1: [u8; 3],

    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
    pub debt_vault: Pubkey,
    pub collateral_vault: Pubkey,

    pub accumulated_protocol_fee_shares: u128,

    pub order_sequence_number: u64,

    pub matched_loan_sequence: u64,

    pub num_bytes_allocated: u32,

    pub _reserved_bids_root: DataIndex,

    pub _reserved_bids_best: DataIndex,
    pub asks_root_index: DataIndex,
    pub asks_best_index: DataIndex,
    pub claimed_seats_root_index: DataIndex,

    pub matched_loans_root_index: DataIndex,
    pub free_list_head_index: DataIndex,

    pub position_count: u32,

    pub fee_config: FeeConfig,

    _padding_after_fee: [u8; 4],

    pub lender_integration_account: Pubkey,

    pub borrower_integration_account: Pubkey,

    pub debt_lending_pool: Pubkey,

    pub collateral_lending_pool: Pubkey,

    pub marginfi_group: Pubkey,

    pub market_signer: Pubkey,
    pub market_signer_bump: u8,

    pub lender_integration_account_bump: u8,

    pub borrower_integration_account_bump: u8,
    _padding_bumps: [u8; 5],

    pub admin: Pubkey,

    pub pending_admin: Pubkey,

    pub is_paused: u8,
    _padding_pause: [u8; 7],
}

const_assert_eq!(size_of::<MarketFixed>(), MARKET_FIXED_SIZE);
const_assert_eq!(size_of::<MarketFixed>() % 8, 0);

impl Get for MarketFixed {}

impl MarketFixed {
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
            _reserved_bids_root: NIL,
            _reserved_bids_best: NIL,
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

pub type MarketValue = DynamicAccount<MarketFixed, Vec<u8>>;

pub type MarketRef<'a> = DynamicAccount<&'a MarketFixed, &'a [u8]>;

pub type MarketRefMut<'a> = DynamicAccount<&'a mut MarketFixed, &'a mut [u8]>;

pub type ClaimedSeatTree<'a> = RedBlackTree<'a, ClaimedSeat>;
pub type ClaimedSeatTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, ClaimedSeat>;
pub type MatchedLoanTree<'a> = RedBlackTree<'a, MatchedLoan>;
pub type MatchedLoanTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, MatchedLoan>;
pub type Bookside<'a> = RedBlackTree<'a, RestingOrder>;
pub type BooksideReadOnly<'a> = RedBlackTreeReadOnly<'a, RestingOrder>;

pub fn get_helper_seat(data: &[u8], index: DataIndex) -> &RBNode<ClaimedSeat> {
    get_helper::<RBNode<ClaimedSeat>>(data, index)
}
pub fn get_mut_helper_seat(data: &mut [u8], index: DataIndex) -> &mut RBNode<ClaimedSeat> {
    get_mut_helper::<RBNode<ClaimedSeat>>(data, index)
}
pub fn get_helper_order(data: &[u8], index: DataIndex) -> &RBNode<RestingOrder> {
    get_helper::<RBNode<RestingOrder>>(data, index)
}
pub fn get_mut_helper_order(data: &mut [u8], index: DataIndex) -> &mut RBNode<RestingOrder> {
    get_mut_helper::<RBNode<RestingOrder>>(data, index)
}
pub fn get_helper_matched_loan(data: &[u8], index: DataIndex) -> &RBNode<MatchedLoan> {
    get_helper::<RBNode<MatchedLoan>>(data, index)
}

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
        seat.risk_profile_id,
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

    pub fn get_debt_mint(&self) -> &Pubkey {
        &self.borrow_market().fixed.debt_mint
    }
    pub fn get_collateral_mint(&self) -> &Pubkey {
        &self.borrow_market().fixed.collateral_mint
    }
    pub fn get_debt_vault(&self) -> &Pubkey {
        &self.borrow_market().fixed.debt_vault
    }
    pub fn get_collateral_vault(&self) -> &Pubkey {
        &self.borrow_market().fixed.collateral_vault
    }

    pub fn has_free_block(&self) -> bool {
        self.borrow_market().fixed.free_list_head_index != NIL
    }

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

    pub fn lookup_risk_profile_seat_index(
        &self,
        global_vault: &Pubkey,
        risk_profile_id: u8,
    ) -> DataIndex {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        let tree: ClaimedSeatTreeReadOnly =
            ClaimedSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&ClaimedSeat::new_empty(
            *global_vault,
            crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE,
            risk_profile_id,
        ))
    }

    pub fn get_asks(&self) -> BooksideReadOnly {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        BooksideReadOnly::new(dynamic, fixed.asks_root_index, fixed.asks_best_index)
    }
}
