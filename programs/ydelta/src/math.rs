//! Fixed-point helpers for yDelta's fp48 `u128` representation.
//! 256-bit intermediates use `ruint::aliases::U256`.

use ruint::aliases::U256;
use solana_program::program_error::ProgramError;

/// Number of fractional bits in yDelta's scaled u128 representation.
pub const SCALE_BITS: u32 = 48;

/// Multiplier: `actual_value × SCALE = scaled_value`.
pub const SCALE: u128 = 1u128 << SCALE_BITS;

/// `(a * b) >> SCALE_BITS` with truncation toward zero. Returns
/// `ArithmeticOverflow` if the result exceeds `u128::MAX`.
pub fn mul_scale(a: u128, b: u128) -> Result<u128, ProgramError> {
    let prod: U256 = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let shifted: U256 = prod >> SCALE_BITS;
    u256_to_u128(shifted)
}

/// `(a << SCALE_BITS) / b` with truncation toward zero. Returns an error on
/// `b == 0` or if the quotient overflows `u128`.
pub fn div_scale(a: u128, b: u128) -> Result<u128, ProgramError> {
    if b == 0 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    let num: U256 = U256::from(a) << SCALE_BITS;
    let q: U256 = num
        .checked_div(U256::from(b))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    u256_to_u128(q)
}

/// `amount << SCALE_BITS`. Returns `ArithmeticOverflow` if any of the top
/// `SCALE_BITS` of `amount` are non-zero.
pub fn to_scaled(amount: u128) -> Result<u128, ProgramError> {
    if amount >> (128 - SCALE_BITS) != 0 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    Ok(amount << SCALE_BITS)
}

/// `scaled >> SCALE_BITS`. Truncation toward zero (i.e. floor for positive
/// values, which is the only domain `u128` represents).
pub fn from_scaled_floor(scaled: u128) -> u128 {
    scaled >> SCALE_BITS
}

/// Ceiling variant: `(scaled + (1 << SCALE_BITS - 1)) >> SCALE_BITS`.
/// Used on liability conversions where a 1-atom floor underpayment
/// would let the caller settle a debt for less than they owe.
pub fn from_scaled_ceil(scaled: u128) -> u128 {
    let mask: u128 = (1u128 << SCALE_BITS) - 1;
    (scaled >> SCALE_BITS) + if (scaled & mask) != 0 { 1 } else { 0 }
}

/// Narrow a `U256` to `u128`, returning `ArithmeticOverflow` if the high
/// 128 bits are non-zero. ruint stores limbs as `[u64; 4]` little-endian.
fn u256_to_u128(x: U256) -> Result<u128, ProgramError> {
    let limbs = x.as_limbs();
    if limbs[2] != 0 || limbs[3] != 0 {
        return Err(ProgramError::ArithmeticOverflow);
    }
    Ok((limbs[0] as u128) | ((limbs[1] as u128) << 64))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 2^80 - 1 fits exactly in 128 bits after the << 48; 2^80 overflows.
        let max_safe = (1u128 << 80) - 1;
        assert!(to_scaled(max_safe).is_ok());
        assert!(to_scaled(1u128 << 80).is_err());
    }

    #[test]
    fn mul_scale_truncates_toward_zero() {
        // (1.5 * 2.5) at scale: 1.5 = 1.5 * 2^48, 2.5 = 2.5 * 2^48.
        // Product after >> 48 = 3.75 * 2^48, which truncates cleanly because
        // 3.75 has an exact 48-bit fractional representation.
        let a = (3u128 << SCALE_BITS) / 2; // 1.5 scaled
        let b = (5u128 << SCALE_BITS) / 2; // 2.5 scaled
        let r = mul_scale(a, b).unwrap();
        let expected = (15u128 << SCALE_BITS) / 4; // 3.75 scaled
        assert_eq!(r, expected);
    }

    #[test]
    fn mul_scale_unit_is_identity() {
        // mul_scale(x, SCALE) == x for any x that doesn't overflow the
        // intermediate product. SCALE represents 1.0 in scaled space.
        for x in [0u128, 1, SCALE, SCALE * 7, (1u128 << 80) - 1] {
            assert_eq!(mul_scale(x, SCALE).unwrap(), x);
        }
    }

    #[test]
    fn div_scale_inverts_mul_scale_within_truncation() {
        let a = 7u128 << SCALE_BITS; // 7.0 scaled
        let b = SCALE; // 1.0 scaled
        let prod = mul_scale(a, b).unwrap();
        assert_eq!(div_scale(prod, b).unwrap(), a);
    }

    #[test]
    fn div_scale_by_zero_errors() {
        assert!(div_scale(SCALE, 0).is_err());
    }

    #[test]
    fn mul_scale_is_monotonic() {
        let k = (3u128 << SCALE_BITS) / 2; // 1.5 scaled
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
        // u128::MAX × u128::MAX overflows even after >> 48. Confirm the
        // ruint path correctly reports the overflow rather than silently
        // truncating.
        let result = mul_scale(u128::MAX, u128::MAX);
        assert!(result.is_err());
    }

    #[test]
    fn u256_narrow_succeeds_for_low_128_bits() {
        let x = U256::from(u128::MAX);
        assert_eq!(u256_to_u128(x).unwrap(), u128::MAX);
    }

    #[test]
    fn u256_narrow_errors_on_high_bits_set() {
        // 2^128 has the lowest bit of the second 128-bit half set.
        let x = U256::from(1u8) << 128;
        assert!(u256_to_u128(x).is_err());
    }
}
