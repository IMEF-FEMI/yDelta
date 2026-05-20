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
    /// `0 = User wallet, 1 = GlobalVault`. Every lender is a vault risk
    /// profile, so this is always `1 = GlobalVault` in practice — the
    /// `0` discriminant is retained only for layout/decoder stability.
    /// Stamped at `process_matched_loan` time from
    /// `ClaimedSeat.owner_kind`.
    pub lender_kind: u8, // 201..202
    /// The profile_id within the vault that funded this loan.
    pub lender_profile_id: u8, // 202..203
    /// Reserved.
    _reserved_byte: u8, // 203..204
    /// Snapshot of `market.fee_config.curator_fee_bps` taken at
    /// promotion time (process_matched_loan). Locks the curator's
    /// management-fee rate at loan inception so subsequent admin
    /// changes to the live config don't retroactively alter
    /// already-funded loans. Zero for loans promoted while the
    /// market's curator_fee_bps was 0.
    pub curator_fee_bps_snapshot: u16, // 204..206
    _padding1: [u8; 2],         // 206..208 align to 8 for u64 below

    /// The `GlobalVault` PDA that holds this loan as
    /// `deployed_principal_atoms`. `process_repay` reads this field to
    /// load the trailing vault accounts and route repay atoms directly
    /// to `vault.integration`.
    pub lender_global_vault: Pubkey, // 208..240

    /// Cumulative principal retired across settlement events on this
    /// loan, tracked by `principal_portion_of_settlement` so a sum of
    /// per-event principal portions equals `principal_debt_atoms` at
    /// full close (the final settlement sweeps the exact remainder,
    /// absorbing per-partial floor residues).
    ///
    /// The live close path (`claim_repayment_for_risk_profile`)
    /// decrements `RiskProfile.deployed_principal_atoms` by the full
    /// `principal_debt_atoms` in a single close event, so this field
    /// and `principal_portion_of_settlement` are currently exercised
    /// only by unit tests — retained for partial-settlement bookkeeping
    /// and decoder/layout stability.
    pub principal_retired_atoms: u64, // 240..248

    /// Cumulative, un-floored lender *gross* interest basis.
    ///
    /// `accrue_loan` adds each segment's `lender_interest_gross`
    /// (`borrower_interest − spread_interest`, both already integer
    /// atoms) to this running total. The curator fee is then floored
    /// ONCE against the cumulative basis —
    /// `floor(cumulative × curator_fee_bps / 10_000)` — and the
    /// per-call curator cut is the delta versus
    /// `accumulated_curator_fee_atoms`. This makes the total curator
    /// fee path-independent: it depends only on the cumulative basis
    /// (hence total elapsed time), not on how many small segments the
    /// loan was accrued in. Flooring per-segment instead would let an
    /// actor fragment accrual (many tiny repays) to round the curator
    /// fee toward zero, with the residue silently landing in
    /// `lender_net`.
    ///
    /// Occupies 8 bytes of alignment padding before the trailing u128
    /// block, so the struct size and the next u128's 16-aligned offset
    /// are unaffected.
    pub cumulative_lender_gross_interest_atoms: u64, // 248..256

    /// Snapshot of debt_bank.asset_share_value (fp48) at lender's
    /// place-order time. Stamped by `process_matched_loan` from the
    /// `MatchedLoan` queue node. Drives the byte-symmetric decrement
    /// of the lender's `debt_encumbered_shares` at
    /// `claim_repayment*` time. Zero for P2Pool loans (no human
    /// lender).
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

    fn verify_version(&self) -> ProgramResult {
        require!(
            self.version == crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            ProgramError::InvalidAccountData,
            "Stale LoanFixed layout: version {} (expected {})",
            self.version,
            crate::state::constants::ACCOUNT_LAYOUT_VERSION
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
    // `Ord` must be consistent with `PartialEq`. `PartialEq`
    // compares `(matched_loan_sequence, market)`, so `Ord` tie-breaks on
    // `market` too — otherwise two loans with the same sequence on
    // different markets compare `Ordering::Equal` while `eq` reports
    // `false`, violating the `Ord`/`Eq` contract (`a.cmp(b) == Equal`
    // iff `a == b`).
    fn cmp(&self, other: &Self) -> Ordering {
        self.matched_loan_sequence
            .cmp(&other.matched_loan_sequence)
            .then_with(|| self.market.to_bytes().cmp(&other.market.to_bytes()))
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
            version: crate::state::constants::ACCOUNT_LAYOUT_VERSION,
            bump,
            lender_kind,
            lender_profile_id,
            _reserved_byte: 0,
            curator_fee_bps_snapshot,
            _padding1: [0; 2],
            lender_global_vault,
            principal_retired_atoms: 0,
            cumulative_lender_gross_interest_atoms: 0,
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

    // ─── Path-independent accrual: cumulative simple interest ───
    //
    // Every accrued quantity (borrower interest, protocol spread,
    // curator fee, lender net) is derived as the delta of a CUMULATIVE
    // total measured from `started_at_unix` to `now`. The cumulative
    // totals are floored ONCE against total elapsed time, so they
    // depend ONLY on `now` — not on how many small segments the loan
    // was accrued in. The per-call deltas telescope, so accruing a
    // loan in many tiny segments yields the same totals as accruing it
    // in one shot.
    //
    // The OLD behaviour floored each tiny segment independently and
    // re-floored the curator cut per call: an actor could fragment
    // accrual (many tiny repays) so each segment's interest floored to
    // 0, rounding the curator fee toward zero with the residue
    // silently landing in `lender_net`. Deriving every quantity from a
    // once-floored cumulative basis closes that.
    //
    // The conservation identity `borrower_interest = lender_net +
    // curator + spread` holds exactly on every call — each side is a
    // delta of the same cumulative functions (see the basis block
    // below for why every delta is also `>= 0`).
    //
    // Total elapsed is computed with `checked_sub`, not an
    // unchecked `(now - started_at) as u128` cast. The cast must not
    // depend on the `now <= last_accrued` guard above: a future
    // weakening of that guard would otherwise wrap a negative delta
    // into a huge u128 → unbounded interest. A `None`/non-positive
    // result is treated as a no-op accrual.
    let total_elapsed: u128 = match now.checked_sub(loan.started_at_unix) {
        Some(d) if d > 0 => d as u128,
        _ => {
            loan.last_accrued_unix = now;
            return Ok(());
        }
    };
    let prior_elapsed: u128 = loan
        .last_accrued_unix
        .checked_sub(loan.started_at_unix)
        .filter(|d| *d >= 0)
        .map(|d| d as u128)
        .unwrap_or(0);
    let principal = loan.principal_debt_atoms as u128;
    // `floor(principal × rate_bps × elapsed / (10_000 × SECONDS_PER_YEAR))`
    // — cumulative simple interest from `started_at` over `elapsed`.
    let cumulative_at = |rate_bps: u16, elapsed: u128| -> Result<u128, ProgramError> {
        principal
            .checked_mul(rate_bps as u128)
            .and_then(|x| x.checked_mul(elapsed))
            .and_then(|x| x.checked_div(denom))
            .ok_or(ProgramError::ArithmeticOverflow)
    };

    // `borrower_rate >= lender_rate` is structurally guaranteed at the
    // cross — the hard-fail form documents that a violation is a bug.
    let spread_bps = loan
        .borrower_rate_bps
        .checked_sub(loan.lender_rate_bps)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // ─── Monotonic cumulative basis ───
    //
    // The lender-gross and protocol-spread legs are each floored ONCE
    // as independent cumulative numerators; the borrower's interest is
    // their SUM (not an independent `floor(borrower_rate)` numerator).
    // This is the key to a per-call-underflow-free derivation:
    //
    //   * `cum_lender_gross`, `cum_spread` — each a single floor of a
    //     monotonically growing numerator → each monotonic in elapsed.
    //   * `cum_borrower = cum_lender_gross + cum_spread` — a sum of two
    //     monotonics, hence monotonic, and `>= cum_lender_gross`.
    //
    // Deriving `cum_borrower` as a *third* independent floor instead
    // would make `cum_borrower − cum_lender_gross` non-monotonic (the
    // two floors tick at different instants), so a segment's protocol
    // spread could compute negative. The sum form avoids that: every
    // per-call increment below is a delta of a monotonic function,
    // hence `>= 0`. The borrower interest differs from a single
    // `floor(borrower_rate)` numerator by at most one atom — the same
    // sub-atom truncation tolerance the protocol accepts everywhere.
    let cum_lender_gross_now = cumulative_at(loan.lender_rate_bps, total_elapsed)?;
    let cum_lender_gross_prior = cumulative_at(loan.lender_rate_bps, prior_elapsed)?;
    let cum_spread_now = cumulative_at(spread_bps, total_elapsed)?;
    let cum_spread_prior = cumulative_at(spread_bps, prior_elapsed)?;
    let cum_borrower_now = cum_lender_gross_now
        .checked_add(cum_spread_now)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let cum_borrower_prior = cum_lender_gross_prior
        .checked_add(cum_spread_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Per-call increments — deltas of monotonic cumulative functions,
    // so they telescope across any segmentation and are each `>= 0`.
    let borrower_interest = cum_borrower_now
        .checked_sub(cum_borrower_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let spread_interest = cum_spread_now
        .checked_sub(cum_spread_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Manager-fee model: the curator takes
    // `curator_fee_bps_snapshot` of the lender's GROSS interest. The
    // cumulative curator fee is floored ONCE against the cumulative
    // lender-gross basis; the per-call cut is the delta versus what was
    // already booked. Because the basis is floored once against total
    // elapsed time, the curator total depends ONLY on `now` — an actor
    // can no longer fragment accrual to round the fee toward zero.
    let curator_active = loan.lender_kind == crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE
        && loan.curator_fee_bps_snapshot > 0;
    let cum_curator_fee = |cum_lender_gross: u128| -> Result<u128, ProgramError> {
        if !curator_active {
            return Ok(0);
        }
        cum_lender_gross
            .checked_mul(loan.curator_fee_bps_snapshot as u128)
            .and_then(|x| x.checked_div(BPS_PER_UNIT as u128))
            .ok_or(ProgramError::ArithmeticOverflow)
    };
    let cum_curator_now = cum_curator_fee(cum_lender_gross_now)?;
    let cum_curator_prior = cum_curator_fee(cum_lender_gross_prior)?;
    // Both the curator cut and the lender net are taken as deltas of
    // their CUMULATIVE forms — `cum_curator(g) = floor(g × f / 10_000)`
    // and `cum_lender_net(g) = g − cum_curator(g)`. Each is monotonic
    // non-decreasing in `g` (the fee fraction is `<= 1`, so a unit
    // increase in `g` raises the floored curator fee by 0 or 1), so
    // both deltas are `>= 0` — no underflow on any segmentation. The
    // two deltas still sum to the lender-gross increment
    // (`cum_lender_net + cum_curator == cum_lender_gross`), so the
    // conservation identity `borrower_interest == lender_net + curator
    // + spread` stays exact.
    let curator_take = cum_curator_now
        .checked_sub(cum_curator_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let cum_lender_net_now = cum_lender_gross_now
        .checked_sub(cum_curator_now)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let cum_lender_net_prior = cum_lender_gross_prior
        .checked_sub(cum_curator_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let lender_net = cum_lender_net_now
        .checked_sub(cum_lender_net_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    loan.outstanding_debt_atoms = checked_add_u64(loan.outstanding_debt_atoms, borrower_interest)?;
    loan.lender_claimable_atoms = checked_add_u64(loan.lender_claimable_atoms, lender_net)?;
    loan.accumulated_curator_fee_atoms =
        checked_add_u64(loan.accumulated_curator_fee_atoms, curator_take)?;
    loan.accumulated_protocol_fee_atoms =
        checked_add_u64(loan.accumulated_protocol_fee_atoms, spread_interest)?;
    // Persist the cumulative lender-gross basis (path-independent
    // total since `started_at`).
    loan.cumulative_lender_gross_interest_atoms = cum_lender_gross_now
        .try_into()
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    loan.last_accrued_unix = now;

    Ok(())
}

/// Split a settlement event into its principal portion, pro-rata
/// against current outstanding debt.
///
/// Each *partial* slice is floored, so a naive sum of slices
/// under-retires principal vs the documented "Σ principal_portion ==
/// principal_debt_atoms at full close" invariant — leaking
/// `deployed_principal` in the vault. The fix: on the FINAL settlement
/// (the slice that drives `outstanding` to exactly 0, i.e.
/// `settlement_atoms >= outstanding`) sweep the EXACT remaining
/// principal (`principal_debt_atoms − principal_retired_atoms`) rather
/// than the floored pro-rata share. Any sub-atom truncation residue
/// accumulated by earlier partials is absorbed by this final sweep, so
/// the slices sum to `principal_debt_atoms` exactly.
pub fn principal_portion_of_settlement(loan: &LoanFixed, settlement_atoms: u64) -> u64 {
    let outstanding = loan.outstanding_debt_atoms as u128;
    if outstanding == 0 {
        return 0;
    }
    let principal_remaining =
        (loan.principal_debt_atoms as u128).saturating_sub(loan.principal_retired_atoms as u128);
    // Final settlement: this slice closes the loan (outstanding → 0).
    // Sweep the exact principal remainder so the per-slice floor
    // residues don't leave principal un-retired.
    if (settlement_atoms as u128) >= outstanding {
        return principal_remaining.min(u64::MAX as u128) as u64;
    }
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

/// The loan-body conservation identity.
///
/// `accrue_loan` grows all four fields in lock-step (`borrower_interest
/// = lender_net + curator_take + spread`); every loan is stamped with
/// `outstanding == lender_claimable == net_principal` and the two fee
/// accumulators at 0. So at all times the **accrual identity** holds:
///
/// ```text
/// outstanding_debt_atoms + Σ(atoms already retired)
///   == lender_claimable_atoms
///    + accumulated_protocol_fee_atoms
///    + accumulated_curator_fee_atoms
/// ```
///
/// where `Σ(atoms already retired)` is the cumulative atoms physically
/// deposited into `lender_marginfi_account` by prior partial-resolution
/// events, tracked on `principal_retired_atoms`. The three
/// lender-side accumulators are **cumulative claimable totals** (the
/// payout the lender / protocol / curator are owed on the whole loan,
/// deposited incrementally as the borrower repays). At full close
/// (`outstanding == 0`) they equal exactly the atoms physically
/// deposited, so `claim_repayment_for_risk_profile` /
/// `protocol_fee_claim` draw a fully-backed amount.
///
/// This function checks that identity. `apply_partial_resolution`
/// preserves it across every partial resolution event.
pub fn assert_loan_conservation(loan: &LoanFixed) -> ProgramResult {
    let claim_side = loan
        .lender_claimable_atoms
        .checked_add(loan.accumulated_protocol_fee_atoms)
        .and_then(|x| x.checked_add(loan.accumulated_curator_fee_atoms))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let owed_side = loan
        .outstanding_debt_atoms
        .checked_add(loan.principal_retired_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    require!(
        claim_side == owed_side,
        YdeltaError::InvalidArgument,
        "loan conservation violated: outstanding({}) + retired({}) = {} != \
         lender_claimable({}) + protocol_fee({}) + curator_fee({}) = {}",
        loan.outstanding_debt_atoms,
        loan.principal_retired_atoms,
        owed_side,
        loan.lender_claimable_atoms,
        loan.accumulated_protocol_fee_atoms,
        loan.accumulated_curator_fee_atoms,
        claim_side
    )?;
    Ok(())
}

/// Retire a `repaid`-atom slice of `outstanding_debt_atoms` on a partial
/// repay / liquidate / settle event.
///
/// `repaid` must be `> 0` and `<= outstanding_debt_atoms` (the caller's
/// `actual_repay_atoms`, already clamped to live outstanding).
///
/// The three lender-side accumulators (`lender_claimable_atoms`,
/// `accumulated_protocol_fee_atoms`, `accumulated_curator_fee_atoms`)
/// are **cumulative claimable totals** — they are not decremented here.
/// What this function maintains is the still-owed side of the
/// conservation identity: it shrinks `outstanding_debt_atoms` by
/// `repaid` and grows the cumulative `principal_retired_atoms` tracker
/// by the same amount, so `outstanding + retired` stays constant and
/// equal to the claim side. The body therefore always reports the true
/// still-owed amount and the claim/protocol-fee withdrawals stay backed
/// (`apply_partial_resolution` keeps `Σ retired == Σ deposited`).
///
/// Returns the `(lender_portion, protocol_portion, curator_portion)` of
/// the slice — its proportional split across the three claimable
/// buckets — so callers (`liquidate_loan` / `settle_matured_loan`) can
/// reason about how the slice maps onto the protocol-fee haircut.
pub fn apply_partial_resolution(
    loan: &mut LoanFixed,
    repaid: u64,
) -> Result<(u64, u64, u64), ProgramError> {
    let outstanding_before = loan.outstanding_debt_atoms;
    require!(
        repaid > 0,
        YdeltaError::InvalidArgument,
        "apply_partial_resolution: repaid must be > 0"
    )?;
    require!(
        repaid <= outstanding_before,
        YdeltaError::InvalidArgument,
        "apply_partial_resolution: repaid {} exceeds outstanding {}",
        repaid,
        outstanding_before
    )?;
    // Conservation identity must hold going in.
    assert_loan_conservation(loan)?;

    // Proportional split of the slice across the three claimable
    // buckets — pure information for the caller; the cumulative
    // accumulators themselves are not mutated.
    let (lender_portion, protocol_portion, curator_portion) = if repaid >= outstanding_before {
        // Final slice — the still-owed remainder of every bucket.
        let protocol_portion = loan
            .accumulated_protocol_fee_atoms
            .saturating_sub(protocol_already_retired(loan));
        let curator_portion = loan
            .accumulated_curator_fee_atoms
            .saturating_sub(curator_already_retired(loan));
        let lender_portion = repaid
            .checked_sub(protocol_portion)
            .and_then(|x| x.checked_sub(curator_portion))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        (lender_portion, protocol_portion, curator_portion)
    } else {
        let outstanding_u128 = outstanding_before as u128;
        let slice_portion_of = |field: u64| -> u64 {
            ((field as u128)
                .checked_mul(repaid as u128)
                .expect("field * repaid fits u128")
                / outstanding_u128) as u64
        };
        let protocol_portion = slice_portion_of(loan.accumulated_protocol_fee_atoms);
        let curator_portion = slice_portion_of(loan.accumulated_curator_fee_atoms);
        let lender_portion = repaid
            .checked_sub(protocol_portion)
            .and_then(|x| x.checked_sub(curator_portion))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        (lender_portion, protocol_portion, curator_portion)
    };

    loan.outstanding_debt_atoms = loan
        .outstanding_debt_atoms
        .checked_sub(repaid)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    loan.principal_retired_atoms = loan
        .principal_retired_atoms
        .checked_add(repaid)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Conservation identity must still hold coming out.
    assert_loan_conservation(loan)?;
    Ok((lender_portion, protocol_portion, curator_portion))
}

/// Atoms of `accumulated_protocol_fee_atoms` already retired by prior
/// slices — derived from the loan's accrual ratios. Used only by the
/// final-slice remainder math in `apply_partial_resolution`.
fn protocol_already_retired(loan: &LoanFixed) -> u64 {
    let total_claim = (loan.lender_claimable_atoms as u128)
        + (loan.accumulated_protocol_fee_atoms as u128)
        + (loan.accumulated_curator_fee_atoms as u128);
    if total_claim == 0 {
        return 0;
    }
    ((loan.accumulated_protocol_fee_atoms as u128) * (loan.principal_retired_atoms as u128)
        / total_claim) as u64
}

/// Curator-fee counterpart of `protocol_already_retired`.
fn curator_already_retired(loan: &LoanFixed) -> u64 {
    let total_claim = (loan.lender_claimable_atoms as u128)
        + (loan.accumulated_protocol_fee_atoms as u128)
        + (loan.accumulated_curator_fee_atoms as u128);
    if total_claim == 0 {
        return 0;
    }
    ((loan.accumulated_curator_fee_atoms as u128) * (loan.principal_retired_atoms as u128)
        / total_claim) as u64
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
        // The lender-gross and protocol-spread legs are each
        // floored ONCE as independent cumulative numerators; the
        // borrower's interest is their SUM. So:
        //   spread   = floor(p × 400  × e / denom)   (400 bps spread)
        //   lender   = floor(p × 600  × e / denom)   (600 bps lender)
        //   borrower = lender + spread               (= floor(l)+floor(s))
        // The borrower interest therefore differs from a single
        // `floor(p × 1000 × e / denom)` numerator by at most one atom
        // — the truncation residue of the two independent floors.
        let denom = 10_000u128 * 31_536_000u128;
        let expected_spread = 1_000_000u128 * 400 * 2_592_000 / denom;
        let expected_lender = 1_000_000u128 * 600 * 2_592_000 / denom;
        let expected_borrower = expected_lender + expected_spread;
        assert_eq!(
            loan.outstanding_debt_atoms as u128 - 1_000_000,
            expected_borrower
        );
        assert_eq!(
            loan.lender_claimable_atoms as u128 - 1_000_000,
            expected_lender
        );
        assert_eq!(loan.accumulated_protocol_fee_atoms as u128, expected_spread);
        // The summed-floor borrower interest differs from a single
        // combined-numerator floor by at most one atom.
        let combined_floor = 1_000_000u128 * 1000 * 2_592_000 / denom;
        assert!(
            (expected_borrower as i128 - combined_floor as i128).abs() <= 1,
            "summed-floor vs combined-floor borrower interest differ only by the truncation residue"
        );
        // Conservation: borrower interest = lender gross + spread, exact.
        assert_eq!(expected_borrower, expected_lender + expected_spread);
        // Boundary: state is still Active at exactly maturity (now > m
        // is false when now == m).
        assert_eq!(loan.state, LoanState::Active as u8);
    }

    #[test]
    fn accrue_post_maturity_uses_same_rate() {
        // Past maturity, accrual continues at the ORIGINAL rate.
        let mut loan = fresh_loan();
        let m = loan.matures_at_unix;
        // Walk to maturity.
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        let outstanding_at_m = loan.outstanding_debt_atoms;
        // Walk another 30 days post-maturity — expect simple interest
        // on ORIGINAL principal at the nominal borrower_rate (1000bps).
        // Accrual is cumulative-from-`started_at`: the post-
        // maturity delta is `cum_borrower(60d) − cum_borrower(30d)`
        // where `cum_borrower = floor(p×600×e/d) + floor(p×400×e/d)`.
        let post_seconds: i64 = 30 * 86_400;
        accrue_loan(&mut loan, m + post_seconds, TEST_GRACE).unwrap();
        let denom = 10_000u128 * 31_536_000u128;
        let started = loan.started_at_unix;
        let cum_borrower =
            |e: u128| 1_000_000u128 * 600 * e / denom + 1_000_000u128 * 400 * e / denom;
        let e_at_m = (m - started) as u128;
        let e_at_post = (m + post_seconds - started) as u128;
        let expected_outstanding_delta = cum_borrower(e_at_post) - cum_borrower(e_at_m);
        assert_eq!(
            loan.outstanding_debt_atoms as u128 - outstanding_at_m as u128,
            expected_outstanding_delta
        );
        // Within one atom of the nominal `floor(p×1000×post/d)`.
        let nominal = 1_000_000u128 * 1000 * (post_seconds as u128) / denom;
        assert!((expected_outstanding_delta as i128 - nominal as i128).abs() <= 1);
        // State stays Active — liquidation flows handle state
        // transitions, not accrue_loan.
        assert_eq!(loan.state, LoanState::Active as u8);
    }

    /// Accruing a loan in many small segments yields the SAME total
    /// curator fee as accruing it in one shot. If `curator_take` were
    /// floored on every lazy `accrue_loan` call, an actor could
    /// fragment accrual (many tiny repays) to round the curator fee
    /// toward zero, with the residue silently landing in `lender_net`.
    /// Flooring the curator cut ONCE against a cumulative basis makes
    /// the total path-independent.
    #[test]
    fn curator_fee_is_path_independent_across_accrual_segments() {
        let make = || -> LoanFixed {
            let mut l = fresh_loan();
            // A risk-profile lender with a non-trivial curator fee so
            // there is a fee to round. 137 bps is deliberately not a
            // clean divisor — it maximizes per-segment truncation.
            l.lender_kind = crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
            l.curator_fee_bps_snapshot = 137;
            l
        };

        // One-shot: accrue the full term in a single call.
        let mut one_shot = make();
        let m = one_shot.matures_at_unix;
        accrue_loan(&mut one_shot, m, TEST_GRACE).unwrap();

        // Fragmented: accrue the SAME interval in many tiny 1-second
        // segments — the adversarial accrual pattern.
        let mut fragmented = make();
        let start = fragmented.last_accrued_unix;
        let total_seconds = m - start;
        let segment: i64 = 1; // one-second segments
        let mut t = start;
        while t < m {
            t = (t + segment).min(m);
            accrue_loan(&mut fragmented, t, TEST_GRACE).unwrap();
        }
        assert_eq!(fragmented.last_accrued_unix, m);

        // The curator fee is identical regardless of segmentation —
        // EXACTLY equal, not within a tolerance: it is floored once
        // against the cumulative basis.
        assert_eq!(
            one_shot.accumulated_curator_fee_atoms, fragmented.accumulated_curator_fee_atoms,
            "curator fee must be path-independent ({}s total accrued in \
             {}s segments vs one shot)",
            total_seconds, segment,
        );
        assert!(
            one_shot.accumulated_curator_fee_atoms > 0,
            "test must exercise a non-zero curator fee"
        );
        // The cumulative-basis tracker is EXACTLY path-independent: it
        // is a single floor of `principal × lender_rate × total_elapsed`
        // — a function of `now` alone.
        assert_eq!(
            one_shot.cumulative_lender_gross_interest_atoms,
            fragmented.cumulative_lender_gross_interest_atoms,
        );
        // Every accrued field is path-independent, not just the curator
        // fee — outstanding debt, lender claimable, protocol spread.
        assert_eq!(
            one_shot.outstanding_debt_atoms,
            fragmented.outstanding_debt_atoms
        );
        assert_eq!(
            one_shot.lender_claimable_atoms,
            fragmented.lender_claimable_atoms
        );
        assert_eq!(
            one_shot.accumulated_protocol_fee_atoms,
            fragmented.accumulated_protocol_fee_atoms
        );
        // Conservation still holds for both.
        assert_loan_conservation(&one_shot).unwrap();
        assert_loan_conservation(&fragmented).unwrap();
        // Per-segment conservation invariant: borrower interest ==
        // lender_net + curator + spread on the fragmented path too.
        let borrower = fragmented.outstanding_debt_atoms - fragmented.principal_debt_atoms;
        let lender = fragmented.lender_claimable_atoms - fragmented.principal_debt_atoms;
        let curator = fragmented.accumulated_curator_fee_atoms;
        let spread = fragmented.accumulated_protocol_fee_atoms;
        assert_eq!(borrower, lender + curator + spread);
    }

    /// `accrue_loan` computes `elapsed` with
    /// `checked_sub`, so a `now < last_accrued_unix` input is a benign
    /// no-op (timestamp bumped, nothing accrued) rather than an
    /// unchecked cast that would wrap a negative delta into a huge u128
    /// and mint unbounded interest.
    #[test]
    fn accrue_negative_elapsed_is_safe_noop() {
        let mut loan = fresh_loan();
        // Accrue forward 10 days so the body carries real interest.
        let t1 = loan.last_accrued_unix + 10 * 86_400;
        accrue_loan(&mut loan, t1, TEST_GRACE).unwrap();
        let snapshot = loan;
        // Now feed a timestamp in the PAST. The `now <= last_accrued`
        // guard catches this today, but the test pins the behaviour
        // independently: no field grows, no overflow, no panic.
        accrue_loan(&mut loan, t1 - 5 * 86_400, TEST_GRACE).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, snapshot.outstanding_debt_atoms);
        assert_eq!(loan.lender_claimable_atoms, snapshot.lender_claimable_atoms);
        assert_eq!(
            loan.accumulated_protocol_fee_atoms,
            snapshot.accumulated_protocol_fee_atoms
        );
        assert_eq!(
            loan.accumulated_curator_fee_atoms,
            snapshot.accumulated_curator_fee_atoms
        );
        // The guarded path does not advance the timestamp backwards.
        assert_eq!(loan.last_accrued_unix, snapshot.last_accrued_unix);
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
        // accrual happens once or twice. ±1 atom drift is acceptable
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

    /// Summed partial-settlement principal portions must
    /// equal `principal_debt_atoms` at full close. Earlier partials floor
    /// their pro-rata share; the FINAL slice (the one that drives
    /// `outstanding` to 0) sweeps the exact remainder so no principal is
    /// left un-retired.
    #[test]
    fn principal_portions_sum_to_full_principal_at_close() {
        let mut loan = fresh_loan();
        // Outstanding deliberately not a clean multiple of the slice
        // size, so each pro-rata partial slice has a fractional part.
        loan.outstanding_debt_atoms = 1_000_007;
        loan.principal_debt_atoms = 1_000_000;
        loan.principal_retired_atoms = 0;

        // Three partial slices that each leave a positive residual, then
        // a final slice (>= remaining outstanding) that closes the loan.
        let partial_slices = [250_001u64, 250_001, 250_001];
        let mut retired_total: u64 = 0;
        for s in partial_slices {
            let portion = principal_portion_of_settlement(&loan, s);
            retired_total += portion;
            loan.principal_retired_atoms += portion;
            loan.outstanding_debt_atoms -= s;
        }
        // Final settlement: settlement_atoms >= remaining outstanding →
        // sweeps the exact principal remainder.
        let remaining_outstanding = loan.outstanding_debt_atoms;
        assert!(remaining_outstanding > 0, "test setup leaves a final slice");
        let final_portion = principal_portion_of_settlement(&loan, remaining_outstanding);
        retired_total += final_portion;

        // The floored partials alone under-retire; the final sweep
        // closes the gap to exactly principal_debt_atoms.
        assert_eq!(
            retired_total, 1_000_000,
            "summed principal portions must equal principal_debt_atoms at full close"
        );
    }

    /// The conservation identity (`outstanding + retired ==
    /// lender_claimable + protocol_fee + curator_fee`) holds at
    /// inception, after accrual, and after every partial resolution.
    #[test]
    fn conservation_identity_holds_through_accrual_and_partials() {
        let mut loan = fresh_loan();
        // Inception.
        assert_loan_conservation(&loan).unwrap();
        // After accrual.
        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        assert_loan_conservation(&loan).unwrap();

        // Three partial resolutions, each verified, then a final slice
        // that closes the loan.
        let outstanding_total = loan.outstanding_debt_atoms;
        let slice = outstanding_total / 4;
        for _ in 0..3 {
            let (l, p, c) = apply_partial_resolution(&mut loan, slice).unwrap();
            // The slice splits exactly across the three buckets.
            assert_eq!(l + p + c, slice);
            assert_loan_conservation(&loan).unwrap();
        }
        // Final slice closes the loan.
        let remaining = loan.outstanding_debt_atoms;
        assert!(remaining > 0);
        apply_partial_resolution(&mut loan, remaining).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, 0);
        // All retired atoms == the cumulative claimable totals: the
        // claim withdrawal is exactly backed.
        assert_eq!(loan.principal_retired_atoms, outstanding_total);
        assert_eq!(
            loan.lender_claimable_atoms
                + loan.accumulated_protocol_fee_atoms
                + loan.accumulated_curator_fee_atoms,
            outstanding_total,
            "claimable totals at close equal the atoms physically retired"
        );
        assert_loan_conservation(&loan).unwrap();
    }

    /// A partial-then-full liquidation. After the
    /// partial slice and the closing slice, the conservation identity
    /// holds, and the liquidation protocol fee (a haircut applied
    /// strictly to `lender_claimable_atoms`) leaves the protocol-fee
    /// accumulator exactly backed: `lender_claimable + protocol_fee +
    /// curator_fee == principal_retired` at close — i.e. every atom in
    /// the accumulator was physically retired.
    #[test]
    fn partial_then_full_liquidation_conservation_and_backed_protocol_fee() {
        let mut loan = fresh_loan();
        // Accrue so the body carries lender / protocol / curator slices.
        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        assert_loan_conservation(&loan).unwrap();
        let outstanding_total = loan.outstanding_debt_atoms;

        // ── Partial liquidation: retire ~30% of the debt. ──
        let partial = outstanding_total * 30 / 100;
        apply_partial_resolution(&mut loan, partial).unwrap();
        // Liquidation protocol fee on the partial slice — modelled as a
        // haircut on the lender's recovery: moved from
        // `lender_claimable_atoms` onto `accumulated_protocol_fee_atoms`.
        // 500 bps of the slice.
        let partial_fee = (partial * 500 / 10_000).min(loan.lender_claimable_atoms);
        loan.lender_claimable_atoms -= partial_fee;
        loan.accumulated_protocol_fee_atoms += partial_fee;
        // The haircut is a pure transfer between two claim buckets — the
        // conservation identity is preserved.
        assert_loan_conservation(&loan).unwrap();

        // ── Closing liquidation: retire the remaining outstanding. ──
        let remaining = loan.outstanding_debt_atoms;
        assert!(remaining > 0);
        apply_partial_resolution(&mut loan, remaining).unwrap();
        let close_fee = (remaining * 500 / 10_000).min(loan.lender_claimable_atoms);
        loan.lender_claimable_atoms -= close_fee;
        loan.accumulated_protocol_fee_atoms += close_fee;
        assert_loan_conservation(&loan).unwrap();

        // Loan fully closed; every claim atom is backed by a retired atom.
        assert_eq!(loan.outstanding_debt_atoms, 0);
        assert_eq!(loan.principal_retired_atoms, outstanding_total);
        assert_eq!(
            loan.lender_claimable_atoms
                + loan.accumulated_protocol_fee_atoms
                + loan.accumulated_curator_fee_atoms,
            outstanding_total,
            "claim buckets at close are exactly backed by retired atoms"
        );
        // The protocol-fee accumulator holds exactly the two haircuts and
        // nothing unbacked (no atoms drawn from a phantom surplus).
        assert_eq!(
            loan.accumulated_protocol_fee_atoms,
            partial_fee + close_fee + accrued_protocol_fee_at(&loan),
        );
    }

    /// Helper for the test above: the protocol spread accrued by
    /// `accrue_loan` (everything in the accumulator that is NOT a
    /// liquidation haircut). Recomputed from the loan's rates so the
    /// assertion is independent of the haircut bookkeeping.
    fn accrued_protocol_fee_at(loan: &LoanFixed) -> u64 {
        // `partial_then_full_liquidation_*` accrues a single 30-day
        // segment on a `fresh_loan()` — spread rate is 400 bps.
        let principal = loan.principal_debt_atoms as u128;
        let elapsed =
            (30u128 * 24 * 60 * 60).min((loan.matures_at_unix - loan.started_at_unix) as u128);
        (principal * 400 * elapsed / (10_000 * SECONDS_PER_YEAR as u128)) as u64
    }

    /// On a bad-debt close the curator fee is clawed back. The curator
    /// is a fee-taker, not a senior creditor: a loan that closes with a
    /// lender principal loss must not pay the curator a management fee
    /// accrued on interest the lender never received. This mirrors the
    /// `liquidate_loan` / `settle_matured_loan` clawback: on
    /// `bad_debt_gap_atoms > 0` the accumulated curator fee is
    /// reattributed to `lender_claimable_atoms` before close.
    #[test]
    fn bad_debt_close_claws_back_curator_fee() {
        let mut loan = fresh_loan();
        // Give the loan a curator fee so there is something to claw back.
        loan.lender_kind = crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
        loan.curator_fee_bps_snapshot = 1_000; // 10%
        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        let curator_before = loan.accumulated_curator_fee_atoms;
        assert!(
            curator_before > 0,
            "loan must carry a curator fee for the clawback to be meaningful"
        );
        assert_loan_conservation(&loan).unwrap();

        // Close the whole loan (bad-debt liquidation retires the debt).
        let outstanding_total = loan.outstanding_debt_atoms;
        apply_partial_resolution(&mut loan, outstanding_total).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, 0);

        // Clawback: bad_debt_gap_atoms > 0 → zero the curator fee and
        // reattribute it to the lender side (junior-to-LP recovery).
        let lender_before = loan.lender_claimable_atoms;
        let clawback = loan.accumulated_curator_fee_atoms;
        loan.accumulated_curator_fee_atoms = 0;
        loan.lender_claimable_atoms += clawback;

        assert_eq!(
            loan.accumulated_curator_fee_atoms, 0,
            "curator collects no management fee on a defaulted loan"
        );
        assert_eq!(loan.lender_claimable_atoms, lender_before + curator_before);
        // Conservation still holds — the clawback is a transfer between
        // claim buckets, not a mint or burn.
        assert_loan_conservation(&loan).unwrap();
        assert_eq!(loan.principal_retired_atoms, outstanding_total);
        assert_eq!(
            loan.lender_claimable_atoms
                + loan.accumulated_protocol_fee_atoms
                + loan.accumulated_curator_fee_atoms,
            outstanding_total,
        );
    }

    /// `apply_partial_resolution` rejects an over-large or zero slice.
    #[test]
    fn apply_partial_resolution_rejects_bad_slices() {
        let mut loan = fresh_loan();
        assert!(apply_partial_resolution(&mut loan, 0).is_err());
        let too_big = loan.outstanding_debt_atoms + 1;
        assert!(apply_partial_resolution(&mut loan, too_big).is_err());
    }

    /// A corrupted body (claim side != owed side) is caught.
    #[test]
    fn assert_loan_conservation_catches_drift() {
        let mut loan = fresh_loan();
        loan.lender_claimable_atoms += 1; // break the identity
        assert!(assert_loan_conservation(&loan).is_err());
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
