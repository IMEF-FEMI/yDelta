//! # Arithmetic for yDelta
//!
//! ## Rule
//!
//! Any 48-bit-fixed-point value — share values (asv/lsv), oracle prices,
//! share-price snapshots, anything whose field name ends in `_fp48` — MUST
//! be operated on via [`Fp48`] and its inherent methods (`checked_mul`,
//! `checked_div`, `checked_add`, `checked_sub`, `checked_mul_u64`,
//! `checked_div_u64`). Those methods route through `ruint::U256`
//! internally, so the intermediate product cannot overflow `u128`.
//!
//! ## Why — the C-1 audit finding (~$4,300 USDC overflow)
//!
//! The original `liability_shares_to_atoms_ceil_at_lsv` computed
//! `share_fp48.checked_mul(lsv_fp48)` directly in `u128`. A share quantity
//! is roughly `atoms × 2^48`; multiplying that by an lsv ≈ 2^48 gives
//! `atoms × 2^96`. `u128::MAX ≈ 3.4×10^38`, so the product overflowed at
//! `atoms ≈ 2^32 ≈ 4.3×10^9` — i.e. **~$4,300 USDC** borrowed
//! (6-decimal USDC). Below that, math worked; at or above, it returned
//! `ArithmeticOverflow` and broke every borrower over that threshold.
//!
//! The fix routes the multiply through [`Fp48::checked_mul`]
//! (`mul_scale` internally, which widens to `U256`). Regression tests
//! pin the safe range:
//!
//! - `protocol::marginfi::tests::liability_shares_to_atoms_ceil_at_lsv_survives_hundred_k_usdc`
//!   — 100,000 USDC × 1.5 lsv.
//! - `protocol::marginfi::tests::liability_shares_to_atoms_ceil_at_lsv_survives_hundred_million_usdc`
//!   — $100M USDC at 1.0 lsv.
//!
//! A related repeat of the same class of bug landed in
//! `atoms_to_shares_at_snapshot` (May-2026 encumber-scaling fix,
//! commit `03247b5`) — same widen-via-U256 remedy.
//!
//! ## NEVER write
//!
//! ```ignore
//! // raw shift then divide:
//! let shares = ((atoms as u128) << 48) / snapshot_fp48;
//!
//! // u128 multiply of two fp48 quantities:
//! let atoms_fp48 = share_fp48.checked_mul(lsv_fp48)?;
//!
//! // chained checked_mul / checked_div on fp48 values:
//! let out = atoms_u128
//!     .checked_mul(share_value_fp48)
//!     .and_then(|x| x.checked_div(snapshot))?;
//! ```
//!
//! These overflow `u128` at low USDC scales.
//!
//! ## Use instead
//!
//! ```ignore
//! use crate::math::Fp48;
//!
//! // atoms ÷ share_price = shares (in fp48):
//! let shares: Fp48 = Fp48::from_atoms(atoms).checked_div(snapshot)?;
//!
//! // shares × lsv = atoms_fp48, then narrow to u64 atoms (ceil):
//! let atoms_owed: u64 = shares.checked_mul(lsv)?.to_atoms_ceil()?;
//!
//! // bps math on an fp48 value (rate × duration / (BPS × YEAR)):
//! let yield_fp48: Fp48 = principal_fp48
//!     .checked_mul_u64(rate_bps)?
//!     .checked_div_u64(BPS_PER_UNIT * SECONDS_PER_YEAR)?;
//! ```
//!
//! ## Free-form integer ratios
//!
//! For exact non-fp48 ratios `a × b / c`, prefer [`mul_div`] / [`mul_div_u64`]
//! — these also widen through `U256` and are safe at any scale that fits
//! the output type. Plain `checked_mul` on two values that could each be
//! near `u128::MAX` is forbidden.
//!
//! ## Migration
//!
//! The legacy helpers (`mul_scale`, `div_scale`, `to_scaled`,
//! `from_scaled_floor`, `from_scaled_ceil`) remain public during the
//! in-progress port of consumer files. They will be removed once all
//! `_fp48` fields are typed as [`Fp48`]. New code MUST use [`Fp48`].

use ruint::aliases::U256;
use solana_program::program_error::ProgramError;

use crate::program::YdeltaError;

/// Number of fractional bits in the fp48 representation. `value × 2^48`
/// is the on-disk encoding shared with marginfi's `I80F48`.
pub const SCALE_BITS: u32 = 48;

/// `2^48` — the fp48 unit. `Fp48::ONE.raw() == SCALE`. Used as both
/// shift constant and divisor by the legacy `mul_scale` / `div_scale`
/// helpers below.
pub const SCALE: u128 = 1u128 << SCALE_BITS;

// ────────────────────────────────────────────────────────────────────────
// Fp48 — type-enforced fp48 arithmetic
// ────────────────────────────────────────────────────────────────────────

/// A 48-bit fixed-point value: the on-disk `u128` stores `real_value × 2^48`.
///
/// `#[repr(transparent)]` over `u128` so the byte layout (and therefore the
/// bytemuck Pod / borsh / on-disk serialization) is bit-identical to the
/// plain `u128` it replaces. Field migrations are type-only — no data
/// migration is needed.
///
/// All multiplicative ops route through `U256` internally and return
/// `Result` so the C-1 class of overflow is impossible to write
/// accidentally. See the module-level docs for the rule and rationale.
#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Fp48(u128);

// SAFETY: `Fp48` is `#[repr(transparent)]` over `u128`, which is itself
// `Pod`/`Zeroable`. Identical layout, same alignment, no padding, no
// invalid bit patterns.
unsafe impl bytemuck::Pod for Fp48 {}
unsafe impl bytemuck::Zeroable for Fp48 {}

impl Fp48 {
    /// Fp48(0) — the additive identity. Also the explicit "no snapshot
    /// yet" sentinel used by some vault paths; callers that special-case
    /// it should check `is_zero()`.
    pub const ZERO: Self = Fp48(0);

    /// Fp48(1.0) — `1u128 << 48`. The multiplicative identity in fp48.
    pub const ONE: Self = Fp48(SCALE);

    /// Wrap a raw `u128` that is *already* fp48-scaled (e.g. the value
    /// read from a marginfi bank's `asset_share_value` field).
    ///
    /// **This is an escape hatch.** Prefer [`Fp48::from_atoms`] when the
    /// source is plain atoms, and prefer composing existing [`Fp48`] values
    /// over re-wrapping raw `u128`s. Audit any new call site.
    pub const fn from_raw(raw: u128) -> Self {
        Fp48(raw)
    }

    /// Unwrap to the raw `u128` payload (still fp48-scaled).
    ///
    /// **Escape hatch.** Use only for: (a) writing to a `_fp48` struct
    /// field that has not yet been migrated to [`Fp48`], (b) passing to a
    /// legacy helper that still takes `u128`, (c) log formatting. Do NOT
    /// use `.raw()` to perform arithmetic — that re-introduces the
    /// overflow class this type exists to prevent.
    pub const fn raw(self) -> u128 {
        self.0
    }

    /// `true` iff this is `Fp48::ZERO`.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Convert plain atoms to fp48 (`atoms × 2^48`). Cannot overflow:
    /// `u64 << 48 ≤ 2^112 < u128::MAX`.
    pub const fn from_atoms(atoms: u64) -> Self {
        Fp48((atoms as u128) << SCALE_BITS)
    }

    /// Truncate this fp48 value to a `u64` atom count, rounding DOWN
    /// (toward zero). Errors if the truncated value does not fit in `u64`.
    pub fn to_atoms_floor(self) -> Result<u64, ProgramError> {
        u64::try_from(from_scaled_floor(self.0))
            .map_err(|_| YdeltaError::MathOverflow.into())
    }

    /// Convert this fp48 value to a `u64` atom count, rounding UP (any
    /// non-zero fractional bits bump the integer part by 1). Errors if the
    /// rounded value does not fit in `u64`.
    pub fn to_atoms_ceil(self) -> Result<u64, ProgramError> {
        u64::try_from(from_scaled_ceil(self.0)?)
            .map_err(|_| YdeltaError::MathOverflow.into())
    }

    /// `self × other`, both fp48; result is fp48. Internally widens to
    /// `U256` so two near-`u128::MAX` operands cannot overflow the
    /// intermediate (this is the C-1 fix path).
    pub fn checked_mul(self, other: Fp48) -> Result<Fp48, ProgramError> {
        mul_scale(self.0, other.0).map(Fp48)
    }

    /// `self ÷ other`, both fp48; result is fp48. Internally widens to
    /// `U256`. Returns `MathDivisionByZero` when `other.is_zero()`.
    pub fn checked_div(self, other: Fp48) -> Result<Fp48, ProgramError> {
        div_scale(self.0, other.0).map(Fp48)
    }

    /// `self + other` — both already fp48 at the same scale, so plain
    /// `checked_add` is correct (no widening needed; sum can't exceed
    /// `u128::MAX` for any real share-value sum, but we check anyway).
    pub fn checked_add(self, other: Fp48) -> Result<Fp48, ProgramError> {
        self.0
            .checked_add(other.0)
            .map(Fp48)
            .ok_or_else(|| YdeltaError::MathOverflow.into())
    }

    /// `self - other`; both already fp48 at the same scale.
    pub fn checked_sub(self, other: Fp48) -> Result<Fp48, ProgramError> {
        self.0
            .checked_sub(other.0)
            .map(Fp48)
            .ok_or_else(|| YdeltaError::MathOverflow.into())
    }

    /// `self × k` where `k` is a plain integer scalar (e.g. rate in bps,
    /// seconds elapsed, atom count). Result is fp48. Overflow is the
    /// `u128`-fp48 × `u128`-scalar product — checked.
    pub fn checked_mul_u64(self, k: u64) -> Result<Fp48, ProgramError> {
        self.0
            .checked_mul(k as u128)
            .map(Fp48)
            .ok_or_else(|| YdeltaError::MathOverflow.into())
    }

    /// `self ÷ k` where `k` is a plain integer scalar. Result is fp48.
    /// Floors toward zero (standard `u128` div). Returns
    /// `MathDivisionByZero` on `k == 0`.
    pub fn checked_div_u64(self, k: u64) -> Result<Fp48, ProgramError> {
        if k == 0 {
            return Err(YdeltaError::MathDivisionByZero.into());
        }
        Ok(Fp48(self.0 / k as u128))
    }
}

// ────────────────────────────────────────────────────────────────────────
// Legacy raw-u128 helpers
// ────────────────────────────────────────────────────────────────────────
//
// Kept public during the in-progress port from `u128` `_fp48` fields to
// [`Fp48`]. New code MUST go through [`Fp48`]; see module docs. These will
// be removed once consumers are ported.
//
// The implementations here are also reused by [`Fp48`]'s inherent methods.

/// `a × b / 2^48` — fp48 multiply. Widens to `U256` so the intermediate
/// product cannot overflow `u128`. Returns `MathOverflow` if the
/// shifted result exceeds `u128`.
pub fn mul_scale(a: u128, b: u128) -> Result<u128, ProgramError> {
    let prod: U256 = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(YdeltaError::MathOverflow)?;
    let shifted: U256 = prod >> SCALE_BITS;
    u256_to_u128(shifted)
}

/// `a × 2^48 / b` — fp48 divide. Widens to `U256` to keep the
/// `a << 48` numerator safe for `a` up to ~`2^208`. Returns
/// `MathDivisionByZero` when `b == 0`.
pub fn div_scale(a: u128, b: u128) -> Result<u128, ProgramError> {
    if b == 0 {
        return Err(YdeltaError::MathDivisionByZero.into());
    }
    let num: U256 = U256::from(a) << SCALE_BITS;
    let q: U256 = num
        .checked_div(U256::from(b))
        .ok_or(YdeltaError::MathOverflow)?;
    u256_to_u128(q)
}

/// Promote a plain integer to fp48 (`amount << 48`). Errors when the
/// top 48 bits of `amount` are non-zero (would shift out and silently
/// drop magnitude).
pub fn to_scaled(amount: u128) -> Result<u128, ProgramError> {
    if amount >> (128 - SCALE_BITS) != 0 {
        return Err(YdeltaError::MathOverflow.into());
    }
    Ok(amount << SCALE_BITS)
}

/// Truncate an fp48 value to its integer part (right shift by 48).
/// Fractional bits are dropped toward zero; never errors.
pub fn from_scaled_floor(scaled: u128) -> u128 {
    scaled >> SCALE_BITS
}

/// Round an fp48 value up to the next integer when any fractional bit
/// is set, otherwise truncate. Returns `MathOverflow` if the +1 wraps.
pub fn from_scaled_ceil(scaled: u128) -> Result<u128, ProgramError> {
    let mask: u128 = (1u128 << SCALE_BITS) - 1;
    let truncated = scaled >> SCALE_BITS;
    if (scaled & mask) != 0 {
        truncated
            .checked_add(1)
            .ok_or(YdeltaError::MathOverflow.into())
    } else {
        Ok(truncated)
    }
}

/// `a × b / c` for plain integers, computed through `U256` so the
/// intermediate product cannot overflow `u128`. `ceil = true` rounds
/// any non-zero remainder up. Errors on `c == 0` or when the quotient
/// exceeds `u128`.
pub fn mul_div(a: u128, b: u128, c: u128, ceil: bool) -> Result<u128, ProgramError> {
    if c == 0 {
        return Err(YdeltaError::MathDivisionByZero.into());
    }
    let prod: U256 = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(YdeltaError::MathOverflow)?;
    let denom: U256 = U256::from(c);
    let q: U256 = prod / denom;
    let r: U256 = prod % denom;
    let result: U256 = if ceil && !r.is_zero() {
        q.checked_add(U256::from(1u8))
            .ok_or(YdeltaError::MathOverflow)?
    } else {
        q
    };
    u256_to_u128(result)
}

/// `u64` convenience wrapper around [`mul_div`]: widens the operands,
/// dispatches through `U256`, then narrows the result. Errors when the
/// quotient does not fit in `u64`.
pub fn mul_div_u64(a: u64, b: u64, c: u64, ceil: bool) -> Result<u64, ProgramError> {
    let result = mul_div(a as u128, b as u128, c as u128, ceil)?;
    u64::try_from(result).map_err(|_| YdeltaError::MathOverflow.into())
}

fn u256_to_u128(x: U256) -> Result<u128, ProgramError> {
    let limbs = x.as_limbs();
    if limbs[2] != 0 || limbs[3] != 0 {
        return Err(YdeltaError::MathOverflow.into());
    }
    Ok((limbs[0] as u128) | ((limbs[1] as u128) << 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_code(e: ProgramError) -> u32 {
        match e {
            ProgramError::Custom(c) => c,
            other => panic!("expected Custom error, got {:?}", other),
        }
    }

    #[test]
    fn scale_constants_are_consistent() {
        assert_eq!(SCALE, 1u128 << 48);
        assert_eq!(SCALE_BITS, 48);
        assert_eq!(SCALE.trailing_zeros(), SCALE_BITS);
    }

    #[test]
    fn to_and_from_scaled_round_trip_for_small_values() {
        for x in [0u128, 1, 2, 100, 1_000_000, 1u128 << 32, (1u128 << 80) - 1] {
            let s = to_scaled(x).unwrap();
            assert_eq!(from_scaled_floor(s), x, "round trip broken at {}", x);
        }
    }

    #[test]
    fn to_scaled_overflows_above_2_to_80() {
        let max_safe = (1u128 << 80) - 1;
        assert!(to_scaled(max_safe).is_ok());
        let err = to_scaled(1u128 << 80).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn mul_scale_truncates_toward_zero() {
        let a = (3u128 << SCALE_BITS) / 2;
        let b = (5u128 << SCALE_BITS) / 2;
        let r = mul_scale(a, b).unwrap();
        let expected = (15u128 << SCALE_BITS) / 4;
        assert_eq!(r, expected);
    }

    #[test]
    fn mul_scale_unit_is_identity() {
        for x in [0u128, 1, SCALE, SCALE * 7, (1u128 << 80) - 1] {
            assert_eq!(mul_scale(x, SCALE).unwrap(), x);
        }
    }

    #[test]
    fn div_scale_inverts_mul_scale_within_truncation() {
        let a = 7u128 << SCALE_BITS;
        let b = SCALE;
        let prod = mul_scale(a, b).unwrap();
        assert_eq!(div_scale(prod, b).unwrap(), a);
    }

    #[test]
    fn div_scale_by_zero_returns_division_by_zero_variant() {
        let err = div_scale(SCALE, 0).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
    }

    #[test]
    fn from_scaled_ceil_rounds_up_on_fractional_bits() {
        assert_eq!(from_scaled_ceil(5u128 << SCALE_BITS).unwrap(), 5);
        assert_eq!(from_scaled_ceil((5u128 << SCALE_BITS) + 1).unwrap(), 6);
        assert_eq!(
            from_scaled_ceil((5u128 << SCALE_BITS) + (SCALE / 2)).unwrap(),
            6
        );
        assert_eq!(from_scaled_ceil(0).unwrap(), 0);
    }

    #[test]
    fn from_scaled_ceil_overflow_faults() {
        let r = from_scaled_ceil(u128::MAX).unwrap();
        assert_eq!(r, (u128::MAX >> SCALE_BITS) + 1);
    }

    #[test]
    fn mul_scale_is_monotonic() {
        let k = (3u128 << SCALE_BITS) / 2;
        for (a, b) in [
            (0u128, 1u128),
            (SCALE, SCALE * 2),
            (SCALE * 1000, SCALE * 1001),
        ] {
            assert!(mul_scale(a, k).unwrap() <= mul_scale(b, k).unwrap());
        }
    }

    #[test]
    fn mul_scale_overflows_at_extreme_values() {
        let err = mul_scale(u128::MAX, u128::MAX).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn u256_narrow_succeeds_for_low_128_bits() {
        let x = U256::from(u128::MAX);
        assert_eq!(u256_to_u128(x).unwrap(), u128::MAX);
    }

    #[test]
    fn u256_narrow_errors_on_high_bits_set() {
        let x = U256::from(1u8) << 128;
        let err = u256_to_u128(x).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn mul_div_floor_unit_cases() {
        assert_eq!(mul_div(0, 100, 7, false).unwrap(), 0);
        assert_eq!(mul_div(100, 0, 7, false).unwrap(), 0);
        assert_eq!(mul_div(10, 3, 4, false).unwrap(), 7);
        assert_eq!(mul_div(u128::MAX, 1, 1, false).unwrap(), u128::MAX);
    }

    #[test]
    fn mul_div_ceil_unit_cases() {
        assert_eq!(mul_div(10, 3, 4, true).unwrap(), 8);
        assert_eq!(mul_div(12, 3, 4, true).unwrap(), 9);
        assert_eq!(mul_div(0, 100, 7, true).unwrap(), 0);
        assert_eq!(mul_div(u128::MAX, 1, 1, true).unwrap(), u128::MAX);
    }

    #[test]
    fn mul_div_division_by_zero_returns_distinct_variant() {
        let err = mul_div(1, 1, 0, false).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
        let err = mul_div(1, 1, 0, true).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
    }

    #[test]
    fn mul_div_survives_u128_overflow_intermediate() {
        let a = u128::MAX;
        let b = u128::MAX;
        let c = u128::MAX;
        assert_eq!(mul_div(a, b, c, false).unwrap(), u128::MAX);
        assert_eq!(mul_div(a, b, c, true).unwrap(), u128::MAX);
    }

    #[test]
    fn mul_div_overflow_when_quotient_too_large() {
        let a = u128::MAX;
        let b = 2u128;
        let c = 1u128;
        let err = mul_div(a, b, c, false).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn mul_div_ceil_overflow_when_quotient_at_max_plus_remainder() {
        let a = u128::MAX;
        let b = 3u128;
        let c = 2u128;
        let err = mul_div(a, b, c, true).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn mul_div_u64_floor_and_ceil() {
        assert_eq!(mul_div_u64(10, 3, 4, false).unwrap(), 7);
        assert_eq!(mul_div_u64(10, 3, 4, true).unwrap(), 8);
        assert_eq!(mul_div_u64(u64::MAX, 1, 1, false).unwrap(), u64::MAX);
    }

    #[test]
    fn mul_div_u64_errors_when_result_exceeds_u64() {
        let err = mul_div_u64(u64::MAX, 2, 1, false).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn mul_div_u64_division_by_zero_returns_distinct_variant() {
        let err = mul_div_u64(1, 1, 0, false).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
    }

    #[test]
    fn mul_div_matches_naive_when_no_overflow() {
        for (a, b, c) in [
            (12u128, 34, 5),
            (1_000, 1_000, 7),
            (u64::MAX as u128, u64::MAX as u128, u64::MAX as u128),
            (SCALE, SCALE, SCALE),
        ] {
            let naive_floor = ((a as u128) * b) / c;
            let r_floor = mul_div(a, b, c, false).unwrap();
            assert_eq!(r_floor, naive_floor, "floor mismatch at ({a},{b},{c})");

            let naive_ceil = if (a * b) % c == 0 {
                naive_floor
            } else {
                naive_floor + 1
            };
            let r_ceil = mul_div(a, b, c, true).unwrap();
            assert_eq!(r_ceil, naive_ceil, "ceil mismatch at ({a},{b},{c})");
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Fp48 tests
    // ──────────────────────────────────────────────────────────────────

    #[test]
    fn fp48_layout_matches_u128() {
        assert_eq!(std::mem::size_of::<Fp48>(), std::mem::size_of::<u128>());
        assert_eq!(std::mem::align_of::<Fp48>(), std::mem::align_of::<u128>());
    }

    #[test]
    fn fp48_constants() {
        assert_eq!(Fp48::ZERO.raw(), 0);
        assert_eq!(Fp48::ONE.raw(), SCALE);
        assert!(Fp48::ZERO.is_zero());
        assert!(!Fp48::ONE.is_zero());
    }

    #[test]
    fn fp48_from_atoms_round_trip() {
        for atoms in [0u64, 1, 1_000_000, u64::MAX] {
            let fp = Fp48::from_atoms(atoms);
            assert_eq!(fp.to_atoms_floor().unwrap(), atoms);
            assert_eq!(fp.to_atoms_ceil().unwrap(), atoms);
        }
    }

    #[test]
    fn fp48_to_atoms_ceil_rounds_up_on_dust() {
        // from_raw(SCALE + 1) = 1 + 1 ulp, ceil → 2.
        let one_and_dust = Fp48::from_raw(SCALE + 1);
        assert_eq!(one_and_dust.to_atoms_floor().unwrap(), 1);
        assert_eq!(one_and_dust.to_atoms_ceil().unwrap(), 2);
    }

    #[test]
    fn fp48_to_atoms_floor_errors_when_result_exceeds_u64() {
        // 2^64 atoms in fp48 = 2^112; floor = 2^64 which doesn't fit in u64.
        let too_big = Fp48::from_raw(1u128 << (64 + SCALE_BITS));
        let err = too_big.to_atoms_floor().unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn fp48_checked_mul_unit_is_identity() {
        for x in [Fp48::ZERO, Fp48::ONE, Fp48::from_atoms(1_000_000)] {
            assert_eq!(x.checked_mul(Fp48::ONE).unwrap(), x);
            assert_eq!(Fp48::ONE.checked_mul(x).unwrap(), x);
        }
    }

    #[test]
    fn fp48_checked_div_unit_is_identity() {
        for x in [Fp48::from_atoms(1), Fp48::from_atoms(1_000_000), Fp48::ONE] {
            assert_eq!(x.checked_div(Fp48::ONE).unwrap(), x);
        }
    }

    #[test]
    fn fp48_checked_div_by_zero_errors() {
        let err = Fp48::ONE.checked_div(Fp48::ZERO).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
    }

    #[test]
    fn fp48_checked_div_u64_by_zero_errors() {
        let err = Fp48::ONE.checked_div_u64(0).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathDivisionByZero as u32);
    }

    #[test]
    fn fp48_add_sub_round_trip() {
        let a = Fp48::from_atoms(7);
        let b = Fp48::from_atoms(5);
        let sum = a.checked_add(b).unwrap();
        assert_eq!(sum.to_atoms_floor().unwrap(), 12);
        assert_eq!(sum.checked_sub(b).unwrap(), a);
    }

    #[test]
    fn fp48_checked_sub_underflow_errors() {
        let a = Fp48::from_atoms(3);
        let b = Fp48::from_atoms(5);
        let err = a.checked_sub(b).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    /// The C-1 regression in Fp48 form: an fp48 share count times an
    /// fp48 lsv at 100k-USDC scale must NOT overflow.
    ///
    /// Pre-fix `share.checked_mul(lsv)` in plain u128 overflowed at
    /// ~$4,300 USDC (atoms ≈ 2^32). `Fp48::checked_mul` widens through
    /// U256, so this passes at 100k USDC (atoms = 1e11 ≈ 2^36) and well
    /// beyond.
    #[test]
    fn fp48_checked_mul_survives_c1_regression() {
        let atoms: u64 = 100_000 * 1_000_000;
        let shares = Fp48::from_atoms(atoms);
        let lsv = Fp48::from_raw(SCALE + (SCALE / 2)); // 1.5
        let atoms_fp48 = shares.checked_mul(lsv).expect("C-1 regression");
        let out = atoms_fp48.to_atoms_ceil().unwrap();
        assert_eq!(out as u128, atoms as u128 + (atoms as u128) / 2);
    }

    /// Same regression at $100M USDC scale.
    #[test]
    fn fp48_checked_mul_survives_hundred_million_usdc() {
        let atoms: u64 = 100_000_000 * 1_000_000;
        let shares = Fp48::from_atoms(atoms);
        let atoms_fp48 = shares.checked_mul(Fp48::ONE).unwrap();
        assert_eq!(atoms_fp48.to_atoms_floor().unwrap(), atoms);
    }

    /// `Fp48::checked_mul` at the U256-overflow extreme still hard-faults
    /// (same as the underlying `mul_scale`).
    #[test]
    fn fp48_checked_mul_overflow_at_extreme_values() {
        let a = Fp48::from_raw(u128::MAX);
        let err = a.checked_mul(a).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn fp48_checked_mul_u64_scales_correctly() {
        // 5 atoms × 3 = 15 atoms (in fp48).
        let a = Fp48::from_atoms(5);
        assert_eq!(a.checked_mul_u64(3).unwrap().to_atoms_floor().unwrap(), 15);
    }

    #[test]
    fn fp48_checked_mul_u64_overflows_at_extreme() {
        let a = Fp48::from_raw(u128::MAX);
        let err = a.checked_mul_u64(2).unwrap_err();
        assert_eq!(err_code(err), YdeltaError::MathOverflow as u32);
    }

    #[test]
    fn fp48_checked_div_u64_scales_correctly() {
        let a = Fp48::from_atoms(15);
        assert_eq!(a.checked_div_u64(3).unwrap().to_atoms_floor().unwrap(), 5);
    }

    /// Bytemuck Pod round-trip: cast Fp48 → bytes → Fp48 preserves the
    /// payload. Confirms `repr(transparent)` + `unsafe impl Pod` is
    /// layout-compatible with the original `u128` on-disk encoding.
    #[test]
    fn fp48_pod_round_trip_matches_u128_bytes() {
        let orig = Fp48::from_atoms(123_456_789);
        let bytes = bytemuck::bytes_of(&orig).to_vec();
        let u128_bytes = bytemuck::bytes_of(&orig.raw()).to_vec();
        assert_eq!(bytes, u128_bytes, "Fp48 layout must equal u128");
        let back: Fp48 = *bytemuck::from_bytes(&bytes);
        assert_eq!(back, orig);
    }
}
