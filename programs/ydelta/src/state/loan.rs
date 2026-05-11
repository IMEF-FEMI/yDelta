//! Loan state for promoted fixed loans and P2Pool fallback borrows.

use std::cmp::Ordering;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use hypertree::DataIndex;
use shank::ShankAccount;
use solana_program::{entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey};
use static_assertions::const_assert_eq;

use crate::program::YdeltaError;
use crate::require;
use crate::validation::YdeltaAccount;

// ─────────────────── Constants ───────────────────

/// Discriminator stamped at offset 0 of every `LoanFixed` PDA. Random
/// non-zero u64 (ASCII "ydeltaLN") chosen to be unambiguous in account
/// dumps.
pub const LOAN_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_4C_4E;

/// Total size of a `LoanFixed` account body, including discriminator.
/// 288 bytes leaves headroom for future fields without an on-disk
/// migration. Verified by `const_assert_eq!` below.
pub const LOAN_FIXED_SIZE: usize = 288;

/// Basis-points denominator. `rate_bps = 600` means 6%.
pub const BPS_PER_UNIT: u32 = 10_000;

/// Seconds in a calendar year. Matches marginfi's convention; 365.25-day
/// Julian variants would drift accrual against the underlying protocol.
pub const SECONDS_PER_YEAR: i64 = 365 * 24 * 60 * 60;

/// PDA seed prefix for `LoanFixed` accounts.
pub const LOAN_SEED: &[u8] = b"loan";

// ─────────────────── Enums ───────────────────

/// Loan lifecycle state. Stored as a `u8` field on `LoanFixed`.
///
/// Past-maturity loans stay `Active` from the on-chain state's POV —
/// `settle_matured_loan` is the post-maturity mechanism. Variants 1
/// and 2 are reserved/unused for stable ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanState {
    Active = 0,
    Repaid = 3,
}

impl LoanState {
    pub fn from_u8(v: u8) -> Result<Self, ProgramError> {
        match v {
            0 => Ok(Self::Active),
            3 => Ok(Self::Repaid),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

/// Source of a loan's principal. `Fixed` is an orderbook match;
/// `P2Pool` is the marginfi-borrow fallback for unfilled bid residuals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanType {
    Fixed = 0,
    P2Pool = 1,
}

impl LoanType {
    pub fn from_u8(v: u8) -> Result<Self, ProgramError> {
        match v {
            0 => Ok(Self::Fixed),
            1 => Ok(Self::P2Pool),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

// ─────────────────── LoanFixed ───────────────────

/// `LoanFixed` — a fully-promoted loan, owned by the yDelta program. PDA
/// seeds `[b"loan", market.key, sequence.to_le_bytes()]`; deterministic so
/// any cranker can pre-derive.
///
/// Layout note: u128 fields require 16-byte alignment. `borrower_marginfi_borrow_shares`
/// and `debt_index_at_last_accrual` are u128; placed after the Pubkey block
/// (offset 80 — 16-aligned) so no implicit padding is inserted.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct LoanFixed {
    pub discriminator: u64, // 0..8
    pub market: Pubkey,     // 8..40
    pub created_by: Pubkey, // 40..72   cranker; receives lamports on close

    pub matched_loan_sequence: u64, // 72..80   back-ref into market's MatchedLoan series

    pub borrower_marginfi_borrow_shares: u128, // 80..96   set when loan_type == P2Pool
    pub debt_index_at_last_accrual: u128,      // 96..112  reserved for post-maturity compounding

    pub principal_debt_atoms: u64, // 112..120 immutable (post-origination net)
    pub outstanding_debt_atoms: u64, // 120..128 accrues at borrower_rate_bps
    pub lender_claimable_atoms: u64, // 128..136 accrues at lender_rate_bps
    pub collateral_atoms: u64,     // 136..144 immutable; returned on full repay
    pub accumulated_protocol_fee_atoms: u64, // 144..152 spread captured per accrue_loan
    pub accumulated_curator_fee_atoms: u64, // 152..160 curator fee accrual

    pub started_at_unix: i64,   // 160..168
    pub matures_at_unix: i64,   // 168..176
    pub last_accrued_unix: i64, // 176..184

    pub lender_seat_index: DataIndex,   // 184..188
    pub borrower_seat_index: DataIndex, // 188..192

    pub borrower_rate_bps: u16, // 192..194
    pub lender_rate_bps: u16,   // 194..196 invariant: borrower_rate >= lender_rate
    pub state: u8,              // 196..197 LoanState
    pub loan_type: u8,          // 197..198 LoanType
    pub flags: u8,              // 198..199
    pub version: u8,            // 199..200
    pub bump: u8,               // 200..201 PDA bump
    /// `0 = User wallet, 1 = GlobalVault`. Wallet-funded loans carry
    /// `lender_kind = 0` and `lender_global_vault = Pubkey::default()`.
    /// Vault-funded loans stamp this at `process_matched_loan` time
    /// from `ClaimedSeat.owner_kind`.
    pub lender_kind: u8, // 201..202
    /// When `lender_kind = Vault`, the profile_id within the vault
    /// that funded this loan. Zero for wallet-funded.
    pub lender_profile_id: u8, // 202..203
    /// `1` iff a `SecondaryLoanSale` bid for this loan is currently
    /// resting in the market's bids tree. Set by
    /// `place_secondary_bid`, cleared by the cranker on full
    /// finalize, the borrower's full-repay sweep, the staleness
    /// cranker, and any `cancel_order` that targets a secondary bid
    /// for this loan. O(1) lookup replacing a bids-tree walk.
    pub has_resting_secondary_bid: u8, // 203..204
    /// Snapshot of `market.fee_config.curator_fee_bps` taken at
    /// promotion time (process_matched_loan). Locks the curator's
    /// management-fee rate at loan inception so subsequent admin
    /// changes to the live config don't retroactively alter
    /// already-funded vault loans. Zero for wallet-funded loans
    /// (`lender_kind != OWNER_KIND_RISK_PROFILE`) and for vault loans
    /// promoted while the market's curator_fee_bps was 0.
    pub curator_fee_bps_snapshot: u16, // 204..206
    _padding1: [u8; 2],         // 206..208 align to 8 for u64 below

    /// When `lender_kind = Vault`, the `GlobalVault` PDA that holds
    /// this loan as `deployed_principal_atoms`. The vault branch of
    /// `process_repay` reads this field to load the trailing vault
    /// accounts and route repay atoms directly to
    /// `vault.integration`. `Pubkey::default()` for wallet-funded
    /// loans.
    pub lender_global_vault: Pubkey, // 208..240

    /// Cumulative principal repaid across all settlement events on this
    /// loan. Used by vault settlement bookkeeping to split each event's
    /// cash flow into principal vs interest portions, so
    /// `RiskProfile.deployed_principal_atoms` decrements only by the
    /// principal share. Sum of `principal_portion` across all events
    /// equals `principal_debt_atoms` at full close.
    pub principal_retired_atoms: u64, // 240..248
    _padding_align_u128: [u64; 1], // 248..256 — push next u128 to 16-aligned offset

    /// Snapshot of debt_bank.asset_share_value (fp48) at lender's
    /// place-order time. Stamped by `process_matched_loan` from the
    /// `MatchedLoan` queue node. Drives the byte-symmetric decrement
    /// of the lender's `debt_encumbered_shares` at
    /// `claim_repayment*` time. Zero for P2Pool loans (no human
    /// lender) and SECONDARY-promoted bodies.
    pub lender_debt_share_price_snapshot_fp48: u128, // 256..272
    /// Snapshot of collateral_bank.asset_share_value (fp48) at the
    /// borrower's place-order time. Stamped by
    /// `process_matched_loan`. Drives the byte-symmetric decrement
    /// of the borrower's `collateral_encumbered_shares` at
    /// `settle_matured_loan` / `liquidate_loan` / `repay` (full
    /// repay) time.
    pub borrower_collateral_share_price_snapshot_fp48: u128, // 272..288
}

const_assert_eq!(size_of::<LoanFixed>(), LOAN_FIXED_SIZE);
const_assert_eq!(size_of::<LoanFixed>() % 16, 0);

impl YdeltaAccount for LoanFixed {
    fn verify_discriminant(&self) -> ProgramResult {
        require!(
            self.discriminator == LOAN_FIXED_DISCRIMINANT,
            ProgramError::InvalidAccountData,
            "Invalid loan discriminant: {} (expected {})",
            self.discriminator,
            LOAN_FIXED_DISCRIMINANT
        )?;
        Ok(())
    }
}

impl hypertree::Get for LoanFixed {}

impl LoanFixed {
    pub fn assert_state(&self, expected: LoanState) -> ProgramResult {
        let actual = LoanState::from_u8(self.state)?;
        require!(
            actual == expected,
            YdeltaError::InvalidArgument,
            "Loan in state {:?}, expected {:?}",
            actual,
            expected
        )?;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.state == LoanState::Active as u8
    }

    pub fn is_repaid(&self) -> bool {
        self.state == LoanState::Repaid as u8
    }

    pub fn loan_type(&self) -> Result<LoanType, ProgramError> {
        LoanType::from_u8(self.loan_type)
    }

    pub fn loan_state(&self) -> Result<LoanState, ProgramError> {
        LoanState::from_u8(self.state)
    }
}

// ─────────────────── Tree-key Ord (for the matched_loans tree) ───────────────────
//
// Used by hypertree to order MatchedLoan nodes; LoanFixed itself is a PDA,
// not a tree node. Ord on LoanFixed is provided for completeness (compare
// by sequence) but isn't exercised by the runtime.

impl Ord for LoanFixed {
    fn cmp(&self, other: &Self) -> Ordering {
        self.matched_loan_sequence.cmp(&other.matched_loan_sequence)
    }
}

impl PartialOrd for LoanFixed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for LoanFixed {
    fn eq(&self, other: &Self) -> bool {
        self.matched_loan_sequence == other.matched_loan_sequence && self.market == other.market
    }
}

impl Eq for LoanFixed {}

// ─────────────────── PDA helpers ───────────────────

/// Derive the loan PDA address for `(market, sequence)`. Returns address +
/// canonical bump; cranker stores the bump on the `LoanFixed` for
/// `invoke_signed` use during `process_repay` / `process_claim_repayment`.
pub fn loan_pda(market: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    let seq_le = sequence.to_le_bytes();
    Pubkey::find_program_address(&[LOAN_SEED, market.as_ref(), &seq_le], &crate::ID)
}

/// PDA seed prefix for split sub-loans created by the cranker on a
/// SECONDARY_SPLIT match. Distinct from `LOAN_SEED` so the seed
/// space can't collide with primary loans.
pub const SPLIT_LOAN_SEED: &[u8] = b"split_loan";

/// Derive the PDA for a split sub-loan from the matching event's
/// stable queue-node sequence (NOT the market's live
/// `matched_loan_sequence` counter, which can bump between cranker
/// derivation and tx execution under contention). The queue node's
/// `sequence` is assigned at match time and stays stable across the
/// node's lifetime.
pub fn split_loan_pda(market: &Pubkey, queue_node_sequence: u64) -> (Pubkey, u8) {
    let seq_le = queue_node_sequence.to_le_bytes();
    Pubkey::find_program_address(&[SPLIT_LOAN_SEED, market.as_ref(), &seq_le], &crate::ID)
}

// ─────────────────── Construction ───────────────────

impl LoanFixed {
    /// Stamp a `LoanFixed` from a `MatchedLoan` node + the market it
    /// belongs to + the cranker who paid the rent. `net_principal` is the
    /// borrower's credited principal (gross matched principal minus the
    /// origination fee deducted at promotion time).
    #[allow(clippy::too_many_arguments)]
    pub fn new_from_matched_loan(
        market: Pubkey,
        sequence: u64,
        bump: u8,
        created_by: Pubkey,
        lender_seat_index: DataIndex,
        borrower_seat_index: DataIndex,
        principal_atoms: u64,
        net_principal: u64,
        collateral_atoms: u64,
        borrower_rate_bps: u16,
        lender_rate_bps: u16,
        term_seconds: u32,
        matched_at_unix: i64,
        flags: u8,
        loan_type: LoanType,
        borrower_marginfi_borrow_shares: u128,
        lender_debt_share_price_snapshot_fp48: u128,
        borrower_collateral_share_price_snapshot_fp48: u128,
    ) -> Self {
        Self::new_from_matched_loan_with_lender(
            market,
            sequence,
            bump,
            created_by,
            lender_seat_index,
            borrower_seat_index,
            principal_atoms,
            net_principal,
            collateral_atoms,
            borrower_rate_bps,
            lender_rate_bps,
            term_seconds,
            matched_at_unix,
            flags,
            loan_type,
            borrower_marginfi_borrow_shares,
            /*lender_kind=*/ 0, // wallet
            /*lender_profile_id=*/ 0,
            /*lender_global_vault=*/ Pubkey::default(),
            /*curator_fee_bps_snapshot=*/ 0, // no curator on wallet loans
            lender_debt_share_price_snapshot_fp48,
            borrower_collateral_share_price_snapshot_fp48,
        )
    }

    /// Variant that stamps the `lender_kind`, `lender_profile_id`,
    /// `lender_global_vault`, and `curator_fee_bps_snapshot` fields.
    /// Used by `process_matched_loan` after reading the lender's
    /// `ClaimedSeat`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_from_matched_loan_with_lender(
        market: Pubkey,
        sequence: u64,
        bump: u8,
        created_by: Pubkey,
        lender_seat_index: DataIndex,
        borrower_seat_index: DataIndex,
        principal_atoms: u64,
        net_principal: u64,
        collateral_atoms: u64,
        borrower_rate_bps: u16,
        lender_rate_bps: u16,
        term_seconds: u32,
        matched_at_unix: i64,
        flags: u8,
        loan_type: LoanType,
        borrower_marginfi_borrow_shares: u128,
        lender_kind: u8,
        lender_profile_id: u8,
        lender_global_vault: Pubkey,
        curator_fee_bps_snapshot: u16,
        lender_debt_share_price_snapshot_fp48: u128,
        borrower_collateral_share_price_snapshot_fp48: u128,
    ) -> Self {
        let started_at_unix = matched_at_unix;
        let matures_at_unix = started_at_unix.saturating_add(term_seconds as i64);
        Self {
            discriminator: LOAN_FIXED_DISCRIMINANT,
            market,
            created_by,
            matched_loan_sequence: sequence,
            borrower_marginfi_borrow_shares,
            debt_index_at_last_accrual: 0,
            principal_debt_atoms: principal_atoms,
            outstanding_debt_atoms: net_principal,
            lender_claimable_atoms: net_principal,
            collateral_atoms,
            accumulated_protocol_fee_atoms: 0,
            accumulated_curator_fee_atoms: 0,
            started_at_unix,
            matures_at_unix,
            last_accrued_unix: started_at_unix,
            lender_seat_index,
            borrower_seat_index,
            borrower_rate_bps,
            lender_rate_bps,
            state: LoanState::Active as u8,
            loan_type: loan_type as u8,
            flags,
            version: 0,
            bump,
            lender_kind,
            lender_profile_id,
            has_resting_secondary_bid: 0,
            curator_fee_bps_snapshot,
            _padding1: [0; 2],
            lender_global_vault,
            principal_retired_atoms: 0,
            _padding_align_u128: [0; 1],
            lender_debt_share_price_snapshot_fp48,
            borrower_collateral_share_price_snapshot_fp48,
        }
    }
}

// ─────────────────── Accrue ───────────────────

/// Accrue simple interest on the original principal from
/// `last_accrued_unix` up to `now`. No-op when `now <= last_accrued_unix`.
pub fn accrue_loan(loan: &mut LoanFixed, now: i64, _grace_period_seconds: u32) -> ProgramResult {
    if now <= loan.last_accrued_unix {
        return Ok(());
    }
    // Already-settled loans don't accrue further. Without this gate,
    // a subsequent accrue_loan call (e.g. from claim_repayment after
    // a clock-advance to maturity) would regenerate
    // outstanding_debt_atoms from 0 → interest, breaking the
    // outstanding == 0 invariant the closed-loan checks rely on.
    if loan.outstanding_debt_atoms == 0 || loan.state == LoanState::Repaid as u8 {
        loan.last_accrued_unix = now;
        return Ok(());
    }
    // P2Pool loans don't accrue dual-rate interest — outstanding tracks
    // marginfi share inflation directly via the borrow CPI's bookkeeping.
    // Just bump the timestamp.
    if loan.loan_type == LoanType::P2Pool as u8 {
        loan.last_accrued_unix = now;
        return Ok(());
    }

    let denom = (BPS_PER_UNIT as u128)
        .checked_mul(SECONDS_PER_YEAR as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let checked_add_u64 = |a: u64, b: u128| -> Result<u64, ProgramError> {
        let sum = (a as u128)
            .checked_add(b)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        if sum > u64::MAX as u128 {
            return Err(ProgramError::ArithmeticOverflow);
        }
        Ok(sum as u64)
    };

    // ─── Single-segment: simple interest on original principal ───
    let elapsed = (now - loan.last_accrued_unix) as u128;
    let principal = loan.principal_debt_atoms as u128;
    let mul_rate = |rate_bps: u16| -> Result<u128, ProgramError> {
        principal
            .checked_mul(rate_bps as u128)
            .and_then(|x| x.checked_mul(elapsed))
            .and_then(|x| x.checked_div(denom))
            .ok_or(ProgramError::ArithmeticOverflow)
    };
    let borrower_interest = mul_rate(loan.borrower_rate_bps)?;
    let lender_interest_gross = mul_rate(loan.lender_rate_bps)?;
    let spread_interest = borrower_interest
        .checked_sub(lender_interest_gross)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Manager-fee model: the curator takes
    // `curator_fee_bps_snapshot` of the lender's gross interest. The
    // remaining (lender_net) lands on `lender_claimable_atoms`. Only
    // applies to vault-funded loans; wallet-funded loans always have
    // `curator_fee_bps_snapshot == 0` (stamped at promotion). Floor
    // division → curator's cut never exceeds the gross lender
    // interest, even at high bps.
    let curator_take: u128 = if loan.lender_kind
        == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE
        && loan.curator_fee_bps_snapshot > 0
    {
        lender_interest_gross
            .checked_mul(loan.curator_fee_bps_snapshot as u128)
            .and_then(|x| x.checked_div(BPS_PER_UNIT as u128))
            .ok_or(ProgramError::ArithmeticOverflow)?
    } else {
        0
    };
    let lender_net = lender_interest_gross
        .checked_sub(curator_take)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    loan.outstanding_debt_atoms = checked_add_u64(loan.outstanding_debt_atoms, borrower_interest)?;
    loan.lender_claimable_atoms = checked_add_u64(loan.lender_claimable_atoms, lender_net)?;
    loan.accumulated_curator_fee_atoms =
        checked_add_u64(loan.accumulated_curator_fee_atoms, curator_take)?;
    loan.accumulated_protocol_fee_atoms =
        checked_add_u64(loan.accumulated_protocol_fee_atoms, spread_interest)?;
    loan.last_accrued_unix = now;

    Ok(())
}

/// Split a settlement event into its principal portion, pro-rata
/// against current outstanding debt.
pub fn principal_portion_of_settlement(loan: &LoanFixed, settlement_atoms: u64) -> u64 {
    let outstanding = loan.outstanding_debt_atoms as u128;
    if outstanding == 0 {
        return 0;
    }
    let principal_remaining =
        (loan.principal_debt_atoms as u128).saturating_sub(loan.principal_retired_atoms as u128);
    // `settlement_atoms` and `principal_remaining` are both ≤ u64::MAX,
    // so their product fits in u128 — `checked_mul` cannot fail. The
    // hard-fail form documents the intent (overflow would be a bug,
    // not a silent saturation).
    let raw = (settlement_atoms as u128)
        .checked_mul(principal_remaining)
        .expect("settlement * principal_remaining overflows u128")
        / outstanding;
    raw.min(principal_remaining).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_loan() -> LoanFixed {
        const SHARE_VALUE_ONE: u128 = 1u128 << 48;
        LoanFixed::new_from_matched_loan(
            Pubkey::default(),
            42,
            255,
            Pubkey::default(),
            0,
            0,
            1_000_000,         // principal_atoms (gross)
            1_000_000,         // net_principal (no origination)
            500_000,           // collateral_atoms
            1000,              // borrower_rate_bps = 10%
            600,               // lender_rate_bps = 6%
            30 * 24 * 60 * 60, // 30-day term
            1_000_000_000,     // matched_at_unix
            0,                 // flags
            LoanType::Fixed,   // loan_type
            0,                 // borrower_marginfi_borrow_shares
            SHARE_VALUE_ONE,   // lender_debt_share_price_snapshot_fp48
            SHARE_VALUE_ONE,   // borrower_collateral_share_price_snapshot_fp48
        )
    }

    #[test]
    fn loan_size_is_locked() {
        assert_eq!(size_of::<LoanFixed>(), LOAN_FIXED_SIZE);
    }

    #[test]
    fn fresh_loan_has_expected_initial_state() {
        let loan = fresh_loan();
        assert_eq!(loan.discriminator, LOAN_FIXED_DISCRIMINANT);
        assert_eq!(loan.principal_debt_atoms, 1_000_000);
        assert_eq!(loan.outstanding_debt_atoms, 1_000_000);
        assert_eq!(loan.lender_claimable_atoms, 1_000_000);
        assert_eq!(loan.collateral_atoms, 500_000);
        assert_eq!(loan.state, LoanState::Active as u8);
        assert_eq!(loan.loan_type, LoanType::Fixed as u8);
        assert_eq!(loan.last_accrued_unix, 1_000_000_000);
        assert_eq!(loan.matures_at_unix, 1_000_000_000 + 30 * 24 * 60 * 60);
    }

    /// `grace_period_seconds` is unused inside `accrue_loan` but kept
    /// on the function signature for callsite stability
    /// (`liquidate_loan` reads it directly from the market). Tests
    /// pass 0.
    const TEST_GRACE: u32 = 0;

    #[test]
    fn accrue_zero_elapsed_is_noop() {
        let mut loan = fresh_loan();
        let pre = loan;
        let now = loan.last_accrued_unix;
        accrue_loan(&mut loan, now, TEST_GRACE).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, pre.outstanding_debt_atoms);
        assert_eq!(loan.lender_claimable_atoms, pre.lender_claimable_atoms);
        assert_eq!(
            loan.accumulated_protocol_fee_atoms,
            pre.accumulated_protocol_fee_atoms
        );
    }

    #[test]
    fn accrue_pre_maturity_simple_interest() {
        let mut loan = fresh_loan();
        // Walk to exactly maturity: 30 days of simple interest on
        // principal. borrower_rate=1000bps, lender_rate=600bps.
        let maturity = loan.matures_at_unix;
        accrue_loan(&mut loan, maturity, TEST_GRACE).unwrap();
        let expected_borrower = 1_000_000u128 * 1000 * 2_592_000 / (10_000 * 31_536_000);
        let expected_lender = 1_000_000u128 * 600 * 2_592_000 / (10_000 * 31_536_000);
        let expected_spread = expected_borrower - expected_lender;
        assert_eq!(
            loan.outstanding_debt_atoms as u128 - 1_000_000,
            expected_borrower
        );
        assert_eq!(
            loan.lender_claimable_atoms as u128 - 1_000_000,
            expected_lender
        );
        assert_eq!(loan.accumulated_protocol_fee_atoms as u128, expected_spread);
        // Boundary: state is still Active at exactly maturity (now > m
        // is false when now == m).
        assert_eq!(loan.state, LoanState::Active as u8);
    }

    #[test]
    fn accrue_post_maturity_uses_same_rate() {
        // Past maturity, accrual continues at the ORIGINAL rate.
        // Single-segment math.
        let mut loan = fresh_loan();
        let m = loan.matures_at_unix;
        // Walk to maturity.
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        let outstanding_at_m = loan.outstanding_debt_atoms;
        // Walk another 30 days post-maturity — expect simple interest
        // on ORIGINAL principal at the nominal borrower_rate (1000bps).
        let post_seconds: i64 = 30 * 86_400;
        accrue_loan(&mut loan, m + post_seconds, TEST_GRACE).unwrap();
        let elapsed = post_seconds as u128;
        let denom = 10_000u128 * 31_536_000u128;
        let expected_outstanding_delta = 1_000_000u128 * 1000 * elapsed / denom;
        assert_eq!(
            loan.outstanding_debt_atoms as u128 - outstanding_at_m as u128,
            expected_outstanding_delta
        );
        // State stays Active — liquidation flows handle state
        // transitions, not accrue_loan.
        assert_eq!(loan.state, LoanState::Active as u8);
    }

    #[test]
    fn accrue_conservation_holds() {
        let mut loan = fresh_loan();
        let when = loan.last_accrued_unix + 86_400;
        accrue_loan(&mut loan, when, TEST_GRACE).unwrap();
        // borrower_interest = lender_interest + spread_interest
        let borrower = loan.outstanding_debt_atoms - loan.principal_debt_atoms;
        let lender = loan.lender_claimable_atoms - loan.principal_debt_atoms;
        let spread = loan.accumulated_protocol_fee_atoms;
        assert_eq!(borrower, lender + spread);
    }

    #[test]
    fn accrue_twice_compounds_linearly() {
        let mut a = fresh_loan();
        let mut b = fresh_loan();
        // a: accrue 30 days in one shot
        let a_target = a.last_accrued_unix + 30 * 86_400;
        accrue_loan(&mut a, a_target, TEST_GRACE).unwrap();
        // b: accrue 15 days, then another 15 days
        let b_mid = b.last_accrued_unix + 15 * 86_400;
        accrue_loan(&mut b, b_mid, TEST_GRACE).unwrap();
        let b_target = b.last_accrued_unix + 15 * 86_400;
        accrue_loan(&mut b, b_target, TEST_GRACE).unwrap();
        // Within 1 atom — simple interest is mathematically linear, but
        // the per-call truncation residue differs depending on whether
        // you accrue once or twice. ±1 atom drift is acceptable
        // (matches marginfi's `assert_within_one_token` tolerance).
        let diff = |x: u64, y: u64| ((x as i64) - (y as i64)).abs();
        assert!(diff(a.outstanding_debt_atoms, b.outstanding_debt_atoms) <= 1);
        assert!(diff(a.lender_claimable_atoms, b.lender_claimable_atoms) <= 1);
        assert!(
            diff(
                a.accumulated_protocol_fee_atoms,
                b.accumulated_protocol_fee_atoms
            ) <= 1
        );
    }

    #[test]
    fn loan_state_round_trip() {
        for s in [LoanState::Active, LoanState::Repaid] {
            assert_eq!(LoanState::from_u8(s as u8).unwrap(), s);
        }
        // Variants 1 and 2 are reserved/unused for stable ABI;
        // round-trip rejects them.
        assert!(LoanState::from_u8(1).is_err());
        assert!(LoanState::from_u8(2).is_err());
        assert!(LoanState::from_u8(99).is_err());
    }

    #[test]
    fn loan_pda_is_deterministic() {
        let market = Pubkey::new_unique();
        let (pda1, bump1) = loan_pda(&market, 7);
        let (pda2, bump2) = loan_pda(&market, 7);
        assert_eq!(pda1, pda2);
        assert_eq!(bump1, bump2);
        // Different sequence yields different pda.
        let (pda3, _) = loan_pda(&market, 8);
        assert_ne!(pda1, pda3);
    }
}
