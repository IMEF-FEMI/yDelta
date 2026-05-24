use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Default, Debug, Copy, Clone, Pod, Zeroable, PartialEq, Eq)]
pub struct WrappedI80F48 {
    pub value: [u8; 16],
}

impl WrappedI80F48 {
    pub const ZERO: Self = Self { value: [0u8; 16] };

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
