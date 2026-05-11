//! Wire-format primitives. `WrappedI80F48` is the on-disk encoding marginfi
//! uses for any fixed-point value — a raw `[u8; 16]` whose bit pattern, when
//! reinterpreted as `i128`, IS the underlying `I80F48`.
//!
//! This crate **never** interprets these bytes as I80F48. Conversion to/from
//! yDelta's `u128 scaled` (where `SCALE = 2^48`, mirroring I80F48's 48-bit
//! fractional) lives in `ydelta::protocol::marginfi`. Keeping the wire layer
//! opaque ensures `fixed`/`I80F48` only appears in the adapter file.

use bytemuck::{Pod, Zeroable};

/// Marginfi's serialised fixed-point value. Bit-identical to the lower 16
/// bytes of an `I80F48`.
#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Pod, Zeroable, PartialEq, Eq)]
pub struct WrappedI80F48 {
    pub value: [u8; 16],
}

impl WrappedI80F48 {
    pub const ZERO: Self = Self { value: [0u8; 16] };

    /// Reinterpret the 16 little-endian bytes as a raw `i128`. The caller
    /// (the adapter) maps this i128 to `u128 scaled` via a bit-pattern cast.
    pub fn to_i128_bits(self) -> i128 {
        i128::from_le_bytes(self.value)
    }

    pub fn from_i128_bits(bits: i128) -> Self {
        Self {
            value: bits.to_le_bytes(),
        }
    }
}

impl From<i128> for WrappedI80F48 {
    fn from(bits: i128) -> Self {
        Self::from_i128_bits(bits)
    }
}

impl From<WrappedI80F48> for i128 {
    fn from(w: WrappedI80F48) -> i128 {
        w.to_i128_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_bits() {
        for bits in [
            0i128,
            1,
            -1,
            i128::MAX,
            i128::MIN,
            1i128 << 64,
            -(1i128 << 100),
        ] {
            assert_eq!(WrappedI80F48::from_i128_bits(bits).to_i128_bits(), bits);
        }
    }

    #[test]
    fn zero_is_all_zero_bytes() {
        assert_eq!(WrappedI80F48::ZERO.value, [0u8; 16]);
        assert_eq!(WrappedI80F48::ZERO.to_i128_bits(), 0);
    }

    #[test]
    fn pod_size_is_16() {
        assert_eq!(std::mem::size_of::<WrappedI80F48>(), 16);
    }
}
