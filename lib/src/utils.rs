use std::mem::size_of;

pub type DataIndex = u32;

/// Marker trait to emit warnings when using get_helper on the Value type
/// rather than on Node<Value>
pub trait Get: bytemuck::Pod {}

/// Read a struct of type `T` in an array of data at a given index.
///
/// # Trust boundary
///
/// `index` is a *byte offset* into `data`. It is **trusted** to be a
/// valid, block-aligned offset that the program itself produced via the
/// free-list / block allocator — it must never be derived directly from
/// untrusted instruction input. A corrupt `DataIndex` (out of range, or
/// pointing into the middle of another block) is a sign the account is
/// already corrupted; this helper does not — and cannot cheaply —
/// validate that the offset lands on a real node of type `T`.
///
/// To turn the worst failure mode (an out-of-bounds read or a
/// `bytemuck` alignment panic deep in matching) into an explicit,
/// localized failure, `debug_assert`s below check the offset is in
/// range and that the resulting slice is properly aligned for `T`.
/// `get_helper_checked` is a fully-checked variant that returns `None`
/// instead of panicking — prefer it on any path that handles a
/// `DataIndex` that could plausibly be corrupt.
pub fn get_helper<T: Get>(data: &[u8], index: DataIndex) -> &T {
    let index_usize: usize = index as usize;
    debug_assert!(
        index_usize
            .checked_add(size_of::<T>())
            .is_some_and(|end| end <= data.len()),
        "get_helper: DataIndex {index} out of bounds for T of size {} in data of len {}",
        size_of::<T>(),
        data.len(),
    );
    bytemuck::from_bytes(&data[index_usize..index_usize + size_of::<T>()])
}

/// Read a struct of type `T` in an array of data at a given index.
///
/// See [`get_helper`] for the trust-boundary contract — the same applies
/// here.
pub fn get_mut_helper<T: Get>(data: &mut [u8], index: DataIndex) -> &mut T {
    let index_usize: usize = index as usize;
    debug_assert!(
        index_usize
            .checked_add(size_of::<T>())
            .is_some_and(|end| end <= data.len()),
        "get_mut_helper: DataIndex {index} out of bounds for T of size {} in data of len {}",
        size_of::<T>(),
        data.len(),
    );
    bytemuck::from_bytes_mut(&mut data[index_usize..index_usize + size_of::<T>()])
}

/// Fully-checked variant of [`get_helper`]: returns `None` instead of
/// panicking when `index` is out of bounds or the resulting slice is
/// mis-aligned for `T`. Use this on paths that may see a `DataIndex`
/// originating from (or influenced by) untrusted account data, so a
/// corrupt offset becomes a graceful error rather than a transaction
/// abort / DoS.
pub fn get_helper_checked<T: Get>(data: &[u8], index: DataIndex) -> Option<&T> {
    let index_usize: usize = index as usize;
    let end: usize = index_usize.checked_add(size_of::<T>())?;
    if end > data.len() {
        return None;
    }
    bytemuck::try_from_bytes(&data[index_usize..end]).ok()
}

/// The standard `bool` is not a `Pod`, define a replacement that is
/// https://docs.rs/spl-pod/latest/src/spl_pod/primitives.rs.html#13
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(transparent)]
pub struct PodBool(pub u8);
impl PodBool {
    pub const fn from_bool(b: bool) -> Self {
        Self(if b { 1 } else { 0 })
    }
}

impl From<bool> for PodBool {
    fn from(b: bool) -> Self {
        Self::from_bool(b)
    }
}

#[test]
fn test_pod_bool() {
    assert_eq!(PodBool::from_bool(false).0 == 1, false);
    assert_eq!(PodBool::from(false).0 == 1, false);
}

#[macro_export]
#[cfg(not(feature = "certora"))]
macro_rules! trace {
    ($($arg:tt)*) => {
        #[cfg(feature = "trace")]
        {
            #[cfg(target_os = "solana")]
            {
            solana_program::msg!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            }
            #[cfg(not(target_os = "solana"))]
            {
            std::println!("[{}:{}] {}", std::file!(), std::line!(), std::format_args!($($arg)*));
            }
        }
    };
}

#[macro_export]
#[cfg(feature = "certora")]
macro_rules! trace {
    ($($arg:tt)*) => {};
}
