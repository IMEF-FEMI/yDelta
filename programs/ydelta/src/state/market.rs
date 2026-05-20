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

/// Per-market fee configuration. `protocol_fee_bps_floor` is the
/// spread-floor gate applied at match time. `ltv_buffer_bps`
/// is the LTV-at-match buffer in basis points (`200` = 2%).
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct FeeConfig {
    pub protocol_fee_bps_floor: u16,
    pub origination_bps: u16,
    pub curator_split_bps: u16,
    pub curator_fee_bps: u16,
    pub liquidation_keeper_bps: u16,
    pub liquidation_protocol_bps: u16,
    /// Extra collateral required above the oracle-implied minimum on
    /// every match, in basis points. Defaults to 0 on fresh markets
    /// (set by admin ix). At 0 the LTV check enforces only the bare
    /// oracle math; tests typically flip this to 200 (2%) to mirror
    /// mainnet conventions.
    pub ltv_buffer_bps: u16,
    _padding_for_u32: [u8; 2],
    /// Grace window after `loan.matures_at_unix` before the loan
    /// becomes eligible for the `settle_matured_loan` keeper.
    /// Borrowers have this window to repay before keepers can clear
    /// them at a small bonus. Default 86_400 (24h).
    pub grace_period_seconds: u32,
    _padding_tail: [u8; 4],
}
const_assert_eq!(size_of::<FeeConfig>(), 24);
const_assert_eq!(size_of::<FeeConfig>() % 8, 0);

/// Default grace window between `matures_at_unix` and the loan being
/// eligible for the `settle_matured_loan` keeper. 24 hours.
pub const DEFAULT_GRACE_PERIOD_SECONDS: u32 = 86_400;

/// Free-list block payload. Sized to fill a `MARKET_BLOCK_SIZE` block
/// minus the 4-byte free-list-node header. Mirrors manifest's pattern.
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

/// `MatchedLoan` — transient tree-node payload inserted into the
/// market's `matched_loans` RB-tree by the matching engine on every
/// fill. The cranker's `process_matched_loan` ix promotes each node
/// into a `LoanFixed` PDA and frees the slot back to the market's
/// shared free list.
///
/// Sized to `MATCHED_LOAN_SIZE = MARKET_BLOCK_PAYLOAD_SIZE = 144` so it
/// shares the free list with `RestingOrder` and `ClaimedSeat`. u128 field
/// dictates 16-byte alignment; `borrower_marginfi_borrow_shares` placed at
/// offset 16 so it lands aligned without implicit padding. The two
/// share-price snapshots (offsets 112 and 128) carry the side-relevant
/// bank's `asset_share_value` from match-time to promote-time so the
/// cranker can stamp them onto `LoanFixed` for byte-symmetric encumber/
/// release.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankType)]
pub struct MatchedLoan {
    pub sequence: u64,                             // 0..8 — tree key
    _padding0: [u8; 8],                            // 8..16 — align u128
    pub borrower_marginfi_borrow_shares: u128,     // 16..32 — set when loan_type == P2Pool
    pub principal_atoms: u64,                      // 32..40 — gross matched principal
    pub origination_atoms: u64,                    // 40..48 — fee deducted from borrower credit
    pub collateral_atoms: u64,                     // 48..56
    pub matched_at_unix: i64,                      // 56..64
    pub lender_seat_index: hypertree::DataIndex,   // 64..68
    pub borrower_seat_index: hypertree::DataIndex, // 68..72
    pub term_seconds: u32,                         // 72..76
    pub borrower_rate_bps: u16,                    // 76..78
    pub lender_rate_bps: u16,                      // 78..80
    pub loan_type: u8,                             // 80 — 0 = Fixed, 1 = P2Pool
    pub flags: u8,                                 // 81 — bit 0: VAULT_LENDER
    //       bit 3: VAULT_PRESETTLED
    _pad_before_p5: [u8; 6], // 82..88 — align next u128 to 16

    /// Reserved padding.
    _reserved: [u8; 24], // 88..112

    /// Snapshot of debt_bank.asset_share_value (fp48) at the
    /// lender-side place-order time. Stamped onto LoanFixed at
    /// promote-time as `lender_debt_share_price_snapshot_fp48`. Zero
    /// for P2Pool (no human lender).
    pub lender_debt_share_price_snapshot_fp48: u128, // 112..128
    /// Snapshot of collateral_bank.asset_share_value (fp48) at the
    /// borrower-side place-order time. Stamped onto LoanFixed at
    /// promote-time as `borrower_collateral_share_price_snapshot_fp48`.
    pub borrower_collateral_share_price_snapshot_fp48: u128, // 128..144
}
// 8 + 8 + 16 + 8 + 8 + 8 + 8 + 4 + 4 + 4 + 2 + 2 + 1 + 1 + 6 + 24 + 16 + 16 = 144
const_assert_eq!(
    size_of::<MatchedLoan>(),
    super::constants::MATCHED_LOAN_SIZE
);
const_assert_eq!(size_of::<MatchedLoan>() % 16, 0);

// Bit masks for `MatchedLoan.flags`.
/// Set at match time on every orderbook-funded Fixed loan whose lender
/// is a vault risk profile. The cranker (`process_matched_loan`) trusts
/// this match-time record to route wallet-vs-vault settlement, rather
/// than a live re-read of the lender seat's `owner_kind`.
pub const MATCHED_LOAN_FLAG_VAULT_LENDER: u8 = 0b0000_0001;
/// Set on the Fixed `MatchedLoan` nodes emitted by
/// `convert_p2pool_to_fixed`. The convert processor performs the vault
/// principal migration (`global_vault.integration → market_debt_vault`)
/// and uses the atoms to retire the borrower's P2Pool marginfi
/// liability inline — so the vault profile's `encumbered_in_orders →
/// deployed` bookkeeping is already done by the time the cranker runs.
/// `process_matched_loan` checks this bit and SKIPS `do_vault_settle`
/// (and its vault-settle account requirement) for these nodes: the
/// atoms are not flowing into `market.lender_integration_account` at
/// crank time — they already left the vault to repay the borrower.
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

/// `MarketFixed` — the 512-byte header at the start of every market account.
///
/// Layout note: u128 fields require 16-byte alignment, which dictates field
/// ordering. `accumulated_protocol_fee_shares` is placed right after the four
/// 32-byte Pubkeys so it lands at offset 144 (16-aligned) without implicit
/// padding. Smaller integers and the `FeeConfig` block come last.
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

    /// Fees accrued in adapter-share units.
    pub accumulated_protocol_fee_shares: u128,

    pub order_sequence_number: u64,
    /// Bumped per match when MatchedLoan nodes land.
    pub matched_loan_sequence: u64,

    pub num_bytes_allocated: u32,

    /// Reserved. Unused index slot, always set to `NIL`.
    pub _reserved_bids_root: DataIndex,
    /// Reserved. Unused index slot, always set to `NIL`.
    pub _reserved_bids_best: DataIndex,
    pub asks_root_index: DataIndex,
    pub asks_best_index: DataIndex,
    pub claimed_seats_root_index: DataIndex,
    /// Root of the matched-loan RB-tree.
    pub matched_loans_root_index: DataIndex,
    pub free_list_head_index: DataIndex,

    pub position_count: u32,

    pub fee_config: FeeConfig,

    /// 4 bytes of explicit padding so the following Pubkey-prefixed
    /// integration region lands at an 8-byte-aligned offset.
    _padding_after_fee: [u8; 4],

    // ─── Split integration accounts ───
    //
    // Two marginfi-accounts per market — lender-side holds USDC asset
    // (debt-mint deposits), borrower-side holds collateral asset +
    // P2Pool debt liability. The split sidesteps marginfi v0.1.8's
    // per-`(account, bank)` asset/liability mutual-exclusion: by
    // construction, a single account never holds both an asset and a
    // liability on the same bank.
    //
    /// Lender-side marginfi-account. Holds lender USDC asset only.
    /// PDA at `[b"marginfi_account", market]`.
    pub lender_integration_account: Pubkey,
    /// Borrower-side marginfi-account. Holds borrower collateral
    /// asset on the collateral bank, and the P2Pool debt-side
    /// liability. PDA at `[b"borrower_marginfi_account", market]`.
    pub borrower_integration_account: Pubkey,
    /// Marginfi `Bank` for the debt mint.
    pub debt_lending_pool: Pubkey,
    /// Marginfi `Bank` for the collateral mint.
    pub collateral_lending_pool: Pubkey,
    /// Marginfi group both banks belong to. Stored redundantly to save a
    /// per-CPI account-data read.
    pub marginfi_group: Pubkey,
    /// PDA that signs marginfi CPIs on behalf of the market. Seeds:
    /// `[b"market_signer", market.key]`.
    pub market_signer: Pubkey,
    pub market_signer_bump: u8,
    /// Bump for `lender_integration_account` PDA.
    pub lender_integration_account_bump: u8,
    /// Bump for `borrower_integration_account` PDA.
    pub borrower_integration_account_bump: u8,
    _padding_bumps: [u8; 5],

    /// Market admin. Set to the `create_market` payer at genesis;
    /// gates `set_fee_config`, `protocol_fee_claim`, and
    /// `set_market_pause`. Transferable via the two-step
    /// `transfer_market_admin` (initiate) → `accept_market_admin`
    /// (finalize) flow.
    pub admin: Pubkey,

    /// Staged successor admin set by `transfer_market_admin`.
    /// `accept_market_admin` (signer = pending_admin) finalizes the
    /// transfer by promoting this into `admin` and zeroing the slot.
    /// `Pubkey::default()` means "no pending transfer".
    pub pending_admin: Pubkey,

    /// When `1`, every state-mutating market ix rejects with
    /// `MarketPaused`. Set/cleared by `set_market_pause`
    /// (admin-gated). Read-only ixs (`SyncMarketPosition`) stay live.
    pub is_paused: u8,
    /// `1` once the admin has explicitly called `set_fee_config`
    /// at least once. A fresh `FeeConfig` defaults every bps field
    /// (incl. `ltv_buffer_bps`) to 0, so a market unpaused before fee
    /// config was set would run zero-margin LTV checks.
    /// `set_market_pause(0)` (unpause) is gated on this flag — the admin
    /// MUST consciously run `set_fee_config` (setting `ltv_buffer_bps`
    /// to the desired safety margin) before the market can go live.
    pub fee_config_set: u8,
    _padding_pause: [u8; 6],
}
// Total size accounting (= 512 = MARKET_FIXED_SIZE):
//   8   discriminant
//   8   version + 4×u8 + 3×_padding1
//   128 4 × Pubkey (debt_mint, collateral_mint, debt_vault, collateral_vault)
//   16  u128 (accumulated_protocol_fee_shares)
//   16  2 × u64 (order_sequence_number, matched_loan_sequence)
//   36  9 × u32 (num_bytes_allocated, 7 × DataIndex incl. 2 reserved
//        index slots, position_count)
//   24  FeeConfig
//   4   _padding_after_fee
//   192 6 × Pubkey (integration accounts + signer + group)
//   8   3 × u8 bumps + 5 _padding_bumps
//   32  admin Pubkey
//   32  pending_admin Pubkey
//   8   is_paused u8 + fee_config_set u8 + _padding_pause [u8;6]
//   ──
//   512
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
            fee_config: FeeConfig {
                // Default 24-hour grace window. Other bps fields stay
                // at their `Default::default()` zero values until the
                // admin tunes them.
                grace_period_seconds: DEFAULT_GRACE_PERIOD_SECONDS,
                ..FeeConfig::default()
            },
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
            // New markets start PAUSED. Admin must explicitly send
            // `set_market_pause(false)` once setup (fee_config, marginfi
            // wiring, oracle plumbing) is verified. Defense-in-depth
            // against the "fresh keypair every run" duplicate-market
            // hazard in setup scripts.
            is_paused: 1,
            // Fee config not yet set — `set_market_pause(0)` refuses to
            // unpause until `set_fee_config` flips this.
            fee_config_set: 0,
            _padding_pause: [0; 6],
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

/// Owned `Market` value. Used by clients that copy the entire account.
pub type MarketValue = DynamicAccount<MarketFixed, Vec<u8>>;
/// Read-only view over an in-place market account.
pub type MarketRef<'a> = DynamicAccount<&'a MarketFixed, &'a [u8]>;
/// Mutable view over an in-place market account.
pub type MarketRefMut<'a> = DynamicAccount<&'a mut MarketFixed, &'a mut [u8]>;

pub type ClaimedSeatTree<'a> = RedBlackTree<'a, ClaimedSeat>;
pub type ClaimedSeatTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, ClaimedSeat>;
pub type MatchedLoanTree<'a> = RedBlackTree<'a, MatchedLoan>;
pub type MatchedLoanTreeReadOnly<'a> = RedBlackTreeReadOnly<'a, MatchedLoan>;
pub type Bookside<'a> = RedBlackTree<'a, RestingOrder>;
pub type BooksideReadOnly<'a> = RedBlackTreeReadOnly<'a, RestingOrder>;

/// Read an `RBNode<ClaimedSeat>` payload at `index` in the market's dynamic
/// region.
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

/// Resolve `seat_index` to a *live* `ClaimedSeat` and assert it
/// carries `expected_owner_kind`.
///
/// A `MatchedLoan` queue node stores raw seat `DataIndex`es captured at
/// match time. The primary-promotion cranker stamps a fresh `LoanFixed`
/// from those indices much later; without this check it would trust a
/// node pointing at a freed slot or a seat of the wrong owner kind.
///
/// Liveness is proven by looking the seat up in the claimed-seat tree
/// by its own `(owner, risk_profile_id)` key and confirming the tree
/// returns the *same* index — a node that has been removed/freed (or an
/// out-of-range index reading garbage) cannot round-trip. Seats are
/// never removed from the tree today, but this is the structural
/// guarantee the promotion path should not silently depend on.
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

    /// Look up a seat by `(owner, risk_profile_id)`. Returns `NIL` if absent.
    pub fn lookup_seat_index(&self, owner: &Pubkey, risk_profile_id: u8) -> DataIndex {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        let tree: ClaimedSeatTreeReadOnly =
            ClaimedSeatTreeReadOnly::new(dynamic, fixed.claimed_seats_root_index, NIL);
        tree.lookup_index(&ClaimedSeat::new_empty(*owner, 0, risk_profile_id))
    }

    pub fn get_asks(&self) -> BooksideReadOnly {
        let MarketRef { fixed, dynamic } = self.borrow_market();
        BooksideReadOnly::new(dynamic, fixed.asks_root_index, fixed.asks_best_index)
    }
}
