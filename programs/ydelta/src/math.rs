use ruint::aliases::U256;
use solana_program::program_error::ProgramError;

use crate::program::YdeltaError;

pub const SCALE_BITS: u32 = 48;

pub const SCALE: u128 = 1u128 << SCALE_BITS;

pub fn mul_scale(a: u128, b: u128) -> Result<u128, ProgramError> {
    let prod: U256 = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(YdeltaError::MathOverflow)?;
    let shifted: U256 = prod >> SCALE_BITS;
    u256_to_u128(shifted)
}

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

pub fn to_scaled(amount: u128) -> Result<u128, ProgramError> {
    if amount >> (128 - SCALE_BITS) != 0 {
        return Err(YdeltaError::MathOverflow.into());
    }
    Ok(amount << SCALE_BITS)
}

pub fn from_scaled_floor(scaled: u128) -> u128 {
    scaled >> SCALE_BITS
}

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
}
