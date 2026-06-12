//! `LoanFixed` is the per-loan account that opens when a `MatchedLoan` is
//! settled by `process_matched_loan`. Holds principal, accrued interest,
//! collateral, the lender/borrower identities, and the snapshots needed
//! to close out cleanly through repay, settlement, or liquidation.
//! Fixed-term loans accrue here in-place; P2Pool loans hold a stake in
//! marginfi and the on-chain ledger only tracks the protocol's slice.

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

/// Eight-byte tag at the head of every loan account.
pub const LOAN_FIXED_DISCRIMINANT: u64 = 0x79_64_65_6C_74_61_4C_4E;

/// Byte size of [`LoanFixed`].
pub const LOAN_FIXED_SIZE: usize = 288;

/// Bps denominator (10_000 = 100%) used throughout rate math.
pub const BPS_PER_UNIT: u32 = 10_000;

/// Seconds per Julian-365 year, used as the simple-interest denominator.
pub const SECONDS_PER_YEAR: i64 = 365 * 24 * 60 * 60;

/// PDA seed prefix for loan accounts. Full seeds:
/// `[LOAN_SEED, market, sequence_le]`.
pub const LOAN_SEED: &[u8] = b"loan";

/// Lifecycle state stored in [`LoanFixed::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanState {
    /// Open loan; accrues interest and is reachable by repay /
    /// settle / liquidate.
    Active = 0,
    /// Fully resolved; outstanding debt is 0 and the account is awaiting
    /// rent reclaim.
    Repaid = 3,
}

impl LoanState {
    /// Decode the byte stored on the loan account into the strongly-typed
    /// enum; errors on any value outside the declared discriminants.
    pub fn from_u8(v: u8) -> Result<Self, ProgramError> {
        match v {
            0 => Ok(Self::Active),
            3 => Ok(Self::Repaid),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

/// Two flavours of loan the protocol supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoanType {
    /// Fixed-term, fixed-rate book-matched loan with on-ledger accrual.
    Fixed = 0,
    /// P2Pool residual borrowed from marginfi at the variable pool rate;
    /// ledger fields hold only the static metadata.
    P2Pool = 1,
}

impl LoanType {
    /// Decode the byte stored on the loan account into the strongly-typed
    /// enum; errors on any value outside the declared discriminants.
    pub fn from_u8(v: u8) -> Result<Self, ProgramError> {
        match v {
            0 => Ok(Self::Fixed),
            1 => Ok(Self::P2Pool),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

/// Per-loan PDA written at match-time and updated by accrual, repay,
/// settle, and liquidate. The conservation invariant
/// `outstanding_debt + principal_retired == lender_claimable +
/// protocol_fee + curator_fee` is enforced by [`assert_loan_conservation`]
/// before and after every partial resolution.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Zeroable, Pod, ShankAccount)]
pub struct LoanFixed {
    /// Layout tag; must equal [`LOAN_FIXED_DISCRIMINANT`].
    pub discriminator: u64,
    /// Market this loan belongs to.
    pub market: Pubkey,
    /// Wallet that signed the place_order which created this loan.
    pub created_by: Pubkey,

    /// Per-market loan sequence (matches the `MatchedLoan.sequence` it
    /// was minted from).
    pub matched_loan_sequence: u64,

    /// Borrower's recorded marginfi liability-share count; used for
    /// P2Pool live-outstanding lookups.
    pub borrower_marginfi_borrow_shares: u128,
    /// Reserved snapshot of marginfi's debt index at last accrual.
    pub debt_index_at_last_accrual: u128,

    /// Atoms borrowed at origination (the fixed principal).
    pub principal_debt_atoms: u64,
    /// Atoms still owed by the borrower, inclusive of accrued interest.
    pub outstanding_debt_atoms: u64,
    /// Atoms the lender can claim (principal share + lender-net
    /// interest).
    pub lender_claimable_atoms: u64,
    /// Collateral encumbered by this loan.
    pub collateral_atoms: u64,
    /// Spread-side interest accumulated for the protocol.
    pub accumulated_protocol_fee_atoms: u64,
    /// Curator-side fee accumulated when the lender is a sub-vault.
    pub accumulated_curator_fee_atoms: u64,

    /// Unix-ts when the loan started.
    pub started_at_unix: i64,
    /// Unix-ts when the fixed term expires; after this plus the grace
    /// period the loan becomes settle-eligible.
    pub matures_at_unix: i64,
    /// Unix-ts of the last accrual pass.
    pub last_accrued_unix: i64,

    /// Lender's seat index in the market's seat tree.
    pub lender_seat_index: DataIndex,
    /// Borrower's seat index in the market's seat tree.
    pub borrower_seat_index: DataIndex,

    /// Effective borrower interest rate (max of bid rate and lender rate
    /// + protocol floor).
    pub borrower_rate_bps: u16,
    /// Effective lender interest rate.
    pub lender_rate_bps: u16,
    /// One of the [`LoanState`] discriminants encoded as `u8`.
    pub state: u8,
    /// One of the [`LoanType`] discriminants encoded as `u8`.
    pub loan_type: u8,
    /// Match-time flags carried over from the source `MatchedLoan`.
    pub flags: u8,
    /// Layout version stamped at creation.
    pub version: u8,
    /// PDA bump.
    pub bump: u8,

    /// Lender's `owner_kind` tag (user wallet vs sub-vault).
    pub lender_kind: u8,

    /// Lender sub-vault id when `lender_kind == OWNER_KIND_SUB_VAULT`.
    pub lender_sub_vault_id: u8,

    _reserved_byte: u8,

    /// Curator fee in bps captured at match time; frozen for the life of
    /// the loan even if the market's fee config changes.
    pub curator_fee_bps_snapshot: u16,
    _padding1: [u8; 2],

    /// Lender's global-vault pubkey when the lender is a sub-vault.
    pub lender_global_vault: Pubkey,

    /// Principal portion physically retired by all partial resolutions.
    pub principal_retired_atoms: u64,

    /// Running total of lender gross interest accrued over the loan's
    /// life; used for path-independent curator-fee math.
    pub cumulative_lender_gross_interest_atoms: u64,

    /// Lender-side debt share price at match time. Lets repay convert
    /// retired atoms back to vault shares at the original basis.
    pub lender_debt_share_price_snapshot_fp48: crate::math::Fp48,

    /// Borrower-side collateral share price at match time.
    pub borrower_collateral_share_price_snapshot_fp48: crate::math::Fp48,
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
    /// Errors with `InvalidArgument` if `self.state` does not decode to
    /// `expected`.
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

    /// `true` iff the loan is currently `LoanState::Active`. Errors when
    /// the on-account byte does not decode.
    pub fn is_active(&self) -> Result<bool, ProgramError> {
        Ok(matches!(self.loan_state()?, LoanState::Active))
    }

    /// `true` iff the loan is currently `LoanState::Repaid`. Errors when
    /// the on-account byte does not decode.
    pub fn is_repaid(&self) -> Result<bool, ProgramError> {
        Ok(matches!(self.loan_state()?, LoanState::Repaid))
    }

    /// Decode `self.loan_type` into the strongly-typed enum.
    pub fn loan_type(&self) -> Result<LoanType, ProgramError> {
        LoanType::from_u8(self.loan_type)
    }

    /// Decode `self.state` into the strongly-typed enum.
    pub fn loan_state(&self) -> Result<LoanState, ProgramError> {
        LoanState::from_u8(self.state)
    }
}

impl Ord for LoanFixed {
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

/// Derives the loan PDA for `(market, sequence)` and its bump.
pub fn loan_pda(market: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    let seq_le = sequence.to_le_bytes();
    Pubkey::find_program_address(&[LOAN_SEED, market.as_ref(), &seq_le], &crate::ID)
}

impl LoanFixed {
    /// Thin wrapper that calls [`Self::new_from_matched_loan_with_lender`]
    /// with the lender-vault fields zeroed; used by paths where the
    /// lender is a user wallet.
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
        lender_debt_share_price_snapshot_fp48: crate::math::Fp48,
        borrower_collateral_share_price_snapshot_fp48: crate::math::Fp48,
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
            0,
            0,
            Pubkey::default(),
            0,
            lender_debt_share_price_snapshot_fp48,
            borrower_collateral_share_price_snapshot_fp48,
        )
    }

    /// Build a fresh `LoanFixed` from a settled `MatchedLoan`. Stamps
    /// `state = Active`, `last_accrued_unix = matched_at_unix`, and
    /// derives `matures_at_unix` as `started + term_seconds`.
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
        lender_sub_vault_id: u8,
        lender_global_vault: Pubkey,
        curator_fee_bps_snapshot: u16,
        lender_debt_share_price_snapshot_fp48: crate::math::Fp48,
        borrower_collateral_share_price_snapshot_fp48: crate::math::Fp48,
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
            lender_sub_vault_id,
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

/// Apply simple-interest accrual on a fixed-term loan from
/// `last_accrued_unix` up to `now`. P2Pool loans are a no-op here — their
/// outstanding is read live from marginfi via [`super::ltv::loan_live_outstanding_atoms`].
/// Splits accrued interest into lender-net, curator-fee, and protocol-fee
/// buckets in a path-independent way (cumulative-at-now minus
/// cumulative-at-prior). Hard-errors on time rewinds.
pub fn accrue_loan(loan: &mut LoanFixed, now: i64, _grace_period_seconds: u32) -> ProgramResult {
    if now == loan.last_accrued_unix {
        return Ok(());
    }
    require!(
        now > loan.last_accrued_unix,
        YdeltaError::InvalidArgument,
        "accrue_loan: time rewind detected — now ({}) < last_accrued_unix ({})",
        now,
        loan.last_accrued_unix,
    )?;

    if loan.outstanding_debt_atoms == 0 || loan.state == LoanState::Repaid as u8 {
        loan.last_accrued_unix = now;
        return Ok(());
    }

    if loan.loan_type == LoanType::P2Pool as u8 {
        // Ledger-field accrual is a no-op for P2Pool — see docstring.
        // outstanding_debt_atoms intentionally stays stale; readers must
        // use `loan_live_outstanding_atoms` for the live marginfi-derived
        // value.
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

    let cumulative_at = |rate_bps: u16, elapsed: u128| -> Result<u128, ProgramError> {
        let rate_times_elapsed = (rate_bps as u128)
            .checked_mul(elapsed)
            .ok_or(crate::program::YdeltaError::MathOverflow)?;
        crate::math::mul_div(principal, rate_times_elapsed, denom, false)
    };

    let spread_bps = loan
        .borrower_rate_bps
        .checked_sub(loan.lender_rate_bps)
        .ok_or(ProgramError::ArithmeticOverflow)?;

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

    let borrower_interest = cum_borrower_now
        .checked_sub(cum_borrower_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let spread_interest = cum_spread_now
        .checked_sub(cum_spread_prior)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    let curator_active = loan.lender_kind == crate::state::claimed_seat::OWNER_KIND_SUB_VAULT
        && loan.curator_fee_bps_snapshot > 0;
    let cum_curator_fee = |cum_lender_gross: u128| -> Result<u128, ProgramError> {
        if !curator_active {
            return Ok(0);
        }
        crate::math::mul_div(
            cum_lender_gross,
            loan.curator_fee_bps_snapshot as u128,
            BPS_PER_UNIT as u128,
            false,
        )
    };
    let cum_curator_now = cum_curator_fee(cum_lender_gross_now)?;
    let cum_curator_prior = cum_curator_fee(cum_lender_gross_prior)?;

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

    loan.cumulative_lender_gross_interest_atoms = cum_lender_gross_now
        .try_into()
        .map_err(|_| ProgramError::ArithmeticOverflow)?;
    loan.last_accrued_unix = now;

    Ok(())
}

/// Of `settlement_atoms` paid against this loan, returns how many atoms
/// correspond to principal (vs accrued interest), scaled proportionally
/// against `outstanding_debt_atoms`. Settles full when settlement covers
/// the whole outstanding.
pub fn principal_portion_of_settlement(
    loan: &LoanFixed,
    settlement_atoms: u64,
) -> Result<u64, ProgramError> {
    let outstanding = loan.outstanding_debt_atoms as u128;
    if outstanding == 0 {
        return Ok(0);
    }
    let principal_remaining =
        (loan.principal_debt_atoms as u128).saturating_sub(loan.principal_retired_atoms as u128);

    if (settlement_atoms as u128) >= outstanding {
        return Ok(principal_remaining.min(u64::MAX as u128) as u64);
    }

    let raw = crate::math::mul_div(
        settlement_atoms as u128,
        principal_remaining,
        outstanding,
        false,
    )?;
    Ok(raw.min(principal_remaining).min(u64::MAX as u128) as u64)
}

/// Enforces the loan's bucket-conservation invariant:
/// `lender_claimable + protocol_fee + curator_fee ==
///  outstanding_debt + principal_retired`. Run before and after every
/// resolution to surface accounting drift immediately.
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

/// Apply `repaid` atoms against the loan and return the
/// `(lender, protocol, curator)` portion split. On a full close-out
/// returns the actual remaining claimable in each bucket; on a partial
/// resolution slices proportionally against accumulated fees. Asserts
/// conservation on entry and exit.
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

    assert_loan_conservation(loan)?;

    let (lender_portion, protocol_portion, curator_portion) = if repaid >= outstanding_before {
        let protocol_portion = loan
            .accumulated_protocol_fee_atoms
            .saturating_sub(protocol_already_retired(loan)?);
        let curator_portion = loan
            .accumulated_curator_fee_atoms
            .saturating_sub(curator_already_retired(loan)?);
        let lender_portion = repaid
            .checked_sub(protocol_portion)
            .and_then(|x| x.checked_sub(curator_portion))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        (lender_portion, protocol_portion, curator_portion)
    } else {
        let outstanding_u128 = outstanding_before as u128;
        let slice_portion_of = |field: u64| -> Result<u64, ProgramError> {
            let result =
                crate::math::mul_div(field as u128, repaid as u128, outstanding_u128, false)?;
            Ok(result as u64)
        };
        let protocol_portion = slice_portion_of(loan.accumulated_protocol_fee_atoms)?;
        let curator_portion = slice_portion_of(loan.accumulated_curator_fee_atoms)?;
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

    assert_loan_conservation(loan)?;
    Ok((lender_portion, protocol_portion, curator_portion))
}

fn protocol_already_retired(loan: &LoanFixed) -> Result<u64, ProgramError> {
    let total_claim = (loan.lender_claimable_atoms as u128)
        .checked_add(loan.accumulated_protocol_fee_atoms as u128)
        .and_then(|x| x.checked_add(loan.accumulated_curator_fee_atoms as u128))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if total_claim == 0 {
        return Ok(0);
    }
    let numer = (loan.accumulated_protocol_fee_atoms as u128)
        .checked_mul(loan.principal_retired_atoms as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let result = numer
        .checked_div(total_claim)
        .ok_or(crate::program::YdeltaError::MathDivisionByZero)?;
    u64::try_from(result).map_err(|_| ProgramError::ArithmeticOverflow)
}

fn curator_already_retired(loan: &LoanFixed) -> Result<u64, ProgramError> {
    let total_claim = (loan.lender_claimable_atoms as u128)
        .checked_add(loan.accumulated_protocol_fee_atoms as u128)
        .and_then(|x| x.checked_add(loan.accumulated_curator_fee_atoms as u128))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if total_claim == 0 {
        return Ok(0);
    }
    let numer = (loan.accumulated_curator_fee_atoms as u128)
        .checked_mul(loan.principal_retired_atoms as u128)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let result = numer
        .checked_div(total_claim)
        .ok_or(crate::program::YdeltaError::MathDivisionByZero)?;
    u64::try_from(result).map_err(|_| ProgramError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_loan() -> LoanFixed {
        const SHARE_VALUE_ONE: crate::math::Fp48 = crate::math::Fp48::ONE;
        LoanFixed::new_from_matched_loan(
            Pubkey::default(),
            42,
            255,
            Pubkey::default(),
            0,
            0,
            1_000_000,
            1_000_000,
            500_000,
            1000,
            600,
            30 * 24 * 60 * 60,
            1_000_000_000,
            0,
            LoanType::Fixed,
            0,
            SHARE_VALUE_ONE,
            SHARE_VALUE_ONE,
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

        let maturity = loan.matures_at_unix;
        accrue_loan(&mut loan, maturity, TEST_GRACE).unwrap();

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

        let combined_floor = 1_000_000u128 * 1000 * 2_592_000 / denom;
        assert!(
            (expected_borrower as i128 - combined_floor as i128).abs() <= 1,
            "summed-floor vs combined-floor borrower interest differ only by the truncation residue"
        );

        assert_eq!(expected_borrower, expected_lender + expected_spread);

        assert_eq!(loan.state, LoanState::Active as u8);
    }

    #[test]
    fn accrue_post_maturity_uses_same_rate() {
        let mut loan = fresh_loan();
        let m = loan.matures_at_unix;

        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        let outstanding_at_m = loan.outstanding_debt_atoms;

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

        let nominal = 1_000_000u128 * 1000 * (post_seconds as u128) / denom;
        assert!((expected_outstanding_delta as i128 - nominal as i128).abs() <= 1);

        assert_eq!(loan.state, LoanState::Active as u8);
    }

    #[test]
    fn curator_fee_is_path_independent_across_accrual_segments() {
        let make = || -> LoanFixed {
            let mut l = fresh_loan();

            l.lender_kind = crate::state::claimed_seat::OWNER_KIND_SUB_VAULT;
            l.curator_fee_bps_snapshot = 137;
            l
        };

        let mut one_shot = make();
        let m = one_shot.matures_at_unix;
        accrue_loan(&mut one_shot, m, TEST_GRACE).unwrap();

        let mut fragmented = make();
        let start = fragmented.last_accrued_unix;
        let total_seconds = m - start;
        let segment: i64 = 1;
        let mut t = start;
        while t < m {
            t = (t + segment).min(m);
            accrue_loan(&mut fragmented, t, TEST_GRACE).unwrap();
        }
        assert_eq!(fragmented.last_accrued_unix, m);

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

        assert_eq!(
            one_shot.cumulative_lender_gross_interest_atoms,
            fragmented.cumulative_lender_gross_interest_atoms,
        );

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

        assert_loan_conservation(&one_shot).unwrap();
        assert_loan_conservation(&fragmented).unwrap();

        let borrower = fragmented.outstanding_debt_atoms - fragmented.principal_debt_atoms;
        let lender = fragmented.lender_claimable_atoms - fragmented.principal_debt_atoms;
        let curator = fragmented.accumulated_curator_fee_atoms;
        let spread = fragmented.accumulated_protocol_fee_atoms;
        assert_eq!(borrower, lender + curator + spread);
    }

    #[test]
    fn accrue_time_rewind_hard_errors() {
        let mut loan = fresh_loan();
        let t1 = loan.last_accrued_unix + 10 * 86_400;
        accrue_loan(&mut loan, t1, TEST_GRACE).unwrap();

        // Time rewind: now < last_accrued_unix → InvalidArgument.
        let result = accrue_loan(&mut loan, t1 - 5 * 86_400, TEST_GRACE);
        assert!(
            result.is_err(),
            "accrue_loan must reject time-rewind (M-19); got Ok",
        );

        // Equal-time still no-ops (idempotent re-accrue at same tick).
        let snapshot = loan;
        accrue_loan(&mut loan, t1, TEST_GRACE).unwrap();
        assert_eq!(loan.last_accrued_unix, snapshot.last_accrued_unix);
        assert_eq!(loan.outstanding_debt_atoms, snapshot.outstanding_debt_atoms);
    }

    #[test]
    fn accrue_conservation_holds() {
        let mut loan = fresh_loan();
        let when = loan.last_accrued_unix + 86_400;
        accrue_loan(&mut loan, when, TEST_GRACE).unwrap();

        let borrower = loan.outstanding_debt_atoms - loan.principal_debt_atoms;
        let lender = loan.lender_claimable_atoms - loan.principal_debt_atoms;
        let spread = loan.accumulated_protocol_fee_atoms;
        assert_eq!(borrower, lender + spread);
    }

    #[test]
    fn accrue_twice_compounds_linearly() {
        let mut a = fresh_loan();
        let mut b = fresh_loan();

        let a_target = a.last_accrued_unix + 30 * 86_400;
        accrue_loan(&mut a, a_target, TEST_GRACE).unwrap();

        let b_mid = b.last_accrued_unix + 15 * 86_400;
        accrue_loan(&mut b, b_mid, TEST_GRACE).unwrap();
        let b_target = b.last_accrued_unix + 15 * 86_400;
        accrue_loan(&mut b, b_target, TEST_GRACE).unwrap();

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
    fn principal_portions_sum_to_full_principal_at_close() {
        let mut loan = fresh_loan();

        loan.outstanding_debt_atoms = 1_000_007;
        loan.principal_debt_atoms = 1_000_000;
        loan.principal_retired_atoms = 0;

        let partial_slices = [250_001u64, 250_001, 250_001];
        let mut retired_total: u64 = 0;
        for s in partial_slices {
            let portion = principal_portion_of_settlement(&loan, s).unwrap();
            retired_total += portion;
            loan.principal_retired_atoms += portion;
            loan.outstanding_debt_atoms -= s;
        }

        let remaining_outstanding = loan.outstanding_debt_atoms;
        assert!(remaining_outstanding > 0, "test setup leaves a final slice");
        let final_portion = principal_portion_of_settlement(&loan, remaining_outstanding).unwrap();
        retired_total += final_portion;

        assert_eq!(
            retired_total, 1_000_000,
            "summed principal portions must equal principal_debt_atoms at full close"
        );
    }

    #[test]
    fn conservation_identity_holds_through_accrual_and_partials() {
        let mut loan = fresh_loan();

        assert_loan_conservation(&loan).unwrap();

        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        assert_loan_conservation(&loan).unwrap();

        let outstanding_total = loan.outstanding_debt_atoms;
        let slice = outstanding_total / 4;
        for _ in 0..3 {
            let (l, p, c) = apply_partial_resolution(&mut loan, slice).unwrap();

            assert_eq!(l + p + c, slice);
            assert_loan_conservation(&loan).unwrap();
        }

        let remaining = loan.outstanding_debt_atoms;
        assert!(remaining > 0);
        apply_partial_resolution(&mut loan, remaining).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, 0);

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

    #[test]
    fn partial_then_full_liquidation_conservation_and_backed_protocol_fee() {
        let mut loan = fresh_loan();

        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        assert_loan_conservation(&loan).unwrap();
        let outstanding_total = loan.outstanding_debt_atoms;

        let partial = outstanding_total * 30 / 100;
        apply_partial_resolution(&mut loan, partial).unwrap();

        let partial_fee = (partial * 500 / 10_000).min(loan.lender_claimable_atoms);
        loan.lender_claimable_atoms -= partial_fee;
        loan.accumulated_protocol_fee_atoms += partial_fee;

        assert_loan_conservation(&loan).unwrap();

        let remaining = loan.outstanding_debt_atoms;
        assert!(remaining > 0);
        apply_partial_resolution(&mut loan, remaining).unwrap();
        let close_fee = (remaining * 500 / 10_000).min(loan.lender_claimable_atoms);
        loan.lender_claimable_atoms -= close_fee;
        loan.accumulated_protocol_fee_atoms += close_fee;
        assert_loan_conservation(&loan).unwrap();

        assert_eq!(loan.outstanding_debt_atoms, 0);
        assert_eq!(loan.principal_retired_atoms, outstanding_total);
        assert_eq!(
            loan.lender_claimable_atoms
                + loan.accumulated_protocol_fee_atoms
                + loan.accumulated_curator_fee_atoms,
            outstanding_total,
            "claim buckets at close are exactly backed by retired atoms"
        );

        assert_eq!(
            loan.accumulated_protocol_fee_atoms,
            partial_fee + close_fee + accrued_protocol_fee_at(&loan),
        );
    }

    fn accrued_protocol_fee_at(loan: &LoanFixed) -> u64 {
        let principal = loan.principal_debt_atoms as u128;
        let elapsed =
            (30u128 * 24 * 60 * 60).min((loan.matures_at_unix - loan.started_at_unix) as u128);
        (principal * 400 * elapsed / (10_000 * SECONDS_PER_YEAR as u128)) as u64
    }

    #[test]
    fn bad_debt_close_claws_back_curator_fee() {
        let mut loan = fresh_loan();

        loan.lender_kind = crate::state::claimed_seat::OWNER_KIND_SUB_VAULT;
        loan.curator_fee_bps_snapshot = 1_000;
        let m = loan.matures_at_unix;
        accrue_loan(&mut loan, m, TEST_GRACE).unwrap();
        let curator_before = loan.accumulated_curator_fee_atoms;
        assert!(
            curator_before > 0,
            "loan must carry a curator fee for the clawback to be meaningful"
        );
        assert_loan_conservation(&loan).unwrap();

        let outstanding_total = loan.outstanding_debt_atoms;
        apply_partial_resolution(&mut loan, outstanding_total).unwrap();
        assert_eq!(loan.outstanding_debt_atoms, 0);

        let lender_before = loan.lender_claimable_atoms;
        let clawback = loan.accumulated_curator_fee_atoms;
        loan.accumulated_curator_fee_atoms = 0;
        loan.lender_claimable_atoms += clawback;

        assert_eq!(
            loan.accumulated_curator_fee_atoms, 0,
            "curator collects no management fee on a defaulted loan"
        );
        assert_eq!(loan.lender_claimable_atoms, lender_before + curator_before);

        assert_loan_conservation(&loan).unwrap();
        assert_eq!(loan.principal_retired_atoms, outstanding_total);
        assert_eq!(
            loan.lender_claimable_atoms
                + loan.accumulated_protocol_fee_atoms
                + loan.accumulated_curator_fee_atoms,
            outstanding_total,
        );
    }

    #[test]
    fn apply_partial_resolution_rejects_bad_slices() {
        let mut loan = fresh_loan();
        assert!(apply_partial_resolution(&mut loan, 0).is_err());
        let too_big = loan.outstanding_debt_atoms + 1;
        assert!(apply_partial_resolution(&mut loan, too_big).is_err());
    }

    #[test]
    fn assert_loan_conservation_catches_drift() {
        let mut loan = fresh_loan();
        loan.lender_claimable_atoms += 1;
        assert!(assert_loan_conservation(&loan).is_err());
    }

    #[test]
    fn loan_state_round_trip() {
        for s in [LoanState::Active, LoanState::Repaid] {
            assert_eq!(LoanState::from_u8(s as u8).unwrap(), s);
        }

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

        let (pda3, _) = loan_pda(&market, 8);
        assert_ne!(pda1, pda3);
    }

    #[test]
    fn predicates_error_on_unknown_state_byte() {
        let mut loan = fresh_loan();

        // Known-valid states round-trip through both predicates.
        loan.state = LoanState::Active as u8;
        assert!(loan.is_active().unwrap());
        assert!(!loan.is_repaid().unwrap());

        loan.state = LoanState::Repaid as u8;
        assert!(!loan.is_active().unwrap());
        assert!(loan.is_repaid().unwrap());

        // Every byte that doesn't decode to a LoanState variant must
        // SURFACE an error from both predicates — not silently report
        // false for both. LoanState today has {Active=0, Repaid=3};
        // 1, 2, 4..=255 are all invalid.
        for invalid_byte in [1u8, 2, 4, 5, 42, 99, 200, 255] {
            loan.state = invalid_byte;
            assert!(
                loan.is_active().is_err(),
                "is_active must err on unknown state byte {invalid_byte}",
            );
            assert!(
                loan.is_repaid().is_err(),
                "is_repaid must err on unknown state byte {invalid_byte}",
            );
        }
    }
}
