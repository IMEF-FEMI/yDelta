//! Generic wrapper over the protocol's "fixed header + dynamic tail"
//! account shape and the two helper traits (`DerefOrBorrow`,
//! `DerefOrBorrowMut`) that let the same wrapper drive owned, borrowed,
//! and mutably-borrowed views of the same account bytes.

/// Account view that pairs a fixed-size header with its variable-length
/// dynamic tail. Instantiated with owned `Vec<u8>` for tests and with
/// `&[u8]` / `&mut [u8]` against live account data on-chain.
#[derive(Clone)]
pub struct DynamicAccount<Fixed, Dynamic> {
    /// Fixed-size header (e.g. `MarketFixed`, `UserAccountFixed`).
    pub fixed: Fixed,
    /// Variable-length tail holding the hypertree nodes.
    pub dynamic: Dynamic,
}

/// Borrow-uniformly trait used by [`DynamicAccount`] so methods can accept
/// `T`, `&T`, `&mut T`, or `Vec<T>` and resolve to a `&T` view.
pub trait DerefOrBorrow<T: ?Sized> {
    /// Reborrow as a shared reference to `T`.
    fn deref_or_borrow(&self) -> &T;
}

impl<T: ?Sized> DerefOrBorrow<T> for T {
    fn deref_or_borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized> DerefOrBorrow<T> for &T {
    fn deref_or_borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized> DerefOrBorrow<T> for &mut T {
    fn deref_or_borrow(&self) -> &T {
        self
    }
}

impl<T: Sized> DerefOrBorrow<[T]> for Vec<T> {
    fn deref_or_borrow(&self) -> &[T] {
        self
    }
}

/// Mutable counterpart to [`DerefOrBorrow`].
pub trait DerefOrBorrowMut<T: ?Sized> {
    /// Reborrow as a mutable reference to `T`.
    fn deref_or_borrow_mut(&mut self) -> &mut T;
}

impl<T: ?Sized> DerefOrBorrowMut<T> for &mut T {
    fn deref_or_borrow_mut(&mut self) -> &mut T {
        self
    }
}

impl<T: ?Sized> DerefOrBorrowMut<T> for T {
    fn deref_or_borrow_mut(&mut self) -> &mut T {
        self
    }
}

impl<T: Sized> DerefOrBorrowMut<[T]> for Vec<T> {
    fn deref_or_borrow_mut(&mut self) -> &mut [T] {
        self
    }
}
