//! Typed wrapper for accounts owned by the ydelta program. Asserts the
//! program owner, validates the on-disk discriminant + version through
//! the [`YdeltaAccount`] trait, and exposes a `get_fixed()` helper that
//! returns a `Ref<T>` view over the header bytes.

use bytemuck::Pod;
use hypertree::{get_helper, Get};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};
use std::{cell::Ref, mem::size_of, ops::Deref};

use crate::require;

/// Typed proof that `info` is owned by the ydelta program and its
/// on-disk header decodes as a `T`. Parametric over a state header
/// type that implements [`YdeltaAccount`].
#[derive(Clone)]
pub struct YdeltaAccountInfo<'a, 'info, T: YdeltaAccount + Pod + Clone> {
    pub info: &'a AccountInfo<'info>,

    phantom: std::marker::PhantomData<T>,
}

impl<'a, 'info, T: YdeltaAccount + Get + Clone> YdeltaAccountInfo<'a, 'info, T> {
    /// Construct from an initialized account: asserts the program owner,
    /// minimum data size for `T`, then runs
    /// `verify_discriminant` + `verify_version`.
    pub fn new(
        info: &'a AccountInfo<'info>,
    ) -> Result<YdeltaAccountInfo<'a, 'info, T>, ProgramError> {
        verify_owned_by_ydelta(info.owner)?;

        let bytes: Ref<&mut [u8]> = info.try_borrow_data()?;

        require!(
            bytes.len() >= size_of::<T>(),
            ProgramError::AccountDataTooSmall,
            "Account data ({} bytes) shorter than header ({} bytes)",
            bytes.len(),
            size_of::<T>()
        )?;
        let (header_bytes, _) = bytes.split_at(size_of::<T>());
        let header: &T = get_helper::<T>(header_bytes, 0_u32);
        header.verify_discriminant()?;

        header.verify_version()?;

        Ok(Self {
            info,
            phantom: std::marker::PhantomData,
        })
    }

    /// Construct from an uninitialized PDA: asserts ydelta ownership,
    /// exact length `size_of::<T>()`, and that the data is all zero —
    /// the bootstrap state for a freshly-allocated header.
    pub fn new_init(
        info: &'a AccountInfo<'info>,
    ) -> Result<YdeltaAccountInfo<'a, 'info, T>, ProgramError> {
        verify_owned_by_ydelta(info.owner)?;
        verify_uninitialized::<T>(info)?;
        Ok(Self {
            info,
            phantom: std::marker::PhantomData,
        })
    }

    /// Borrow the on-disk header bytes as a `Ref<T>`. Returned guard
    /// holds the underlying `AccountInfo` borrow open — drop it before
    /// re-borrowing the account mutably.
    pub fn get_fixed(&self) -> Result<Ref<'_, T>, ProgramError> {
        let data: Ref<&mut [u8]> = self.info.try_borrow_data()?;
        Ok(Ref::map(data, |data| get_helper::<T>(data, 0_u32)))
    }
}

impl<'a, 'info, T: YdeltaAccount + Pod + Clone> Deref for YdeltaAccountInfo<'a, 'info, T> {
    type Target = AccountInfo<'info>;

    fn deref(&self) -> &Self::Target {
        self.info
    }
}

/// Contract every ydelta state header implements: a discriminant
/// check (8-byte tag matches the type's keccak hash) and an optional
/// version check (default no-op).
pub trait YdeltaAccount {
    /// Returns `Err` if the on-disk discriminant does not match the
    /// type's expected tag.
    fn verify_discriminant(&self) -> ProgramResult;

    /// Optional header-version check; default accepts any version.
    fn verify_version(&self) -> ProgramResult {
        Ok(())
    }
}

fn verify_owned_by_ydelta(owner: &Pubkey) -> ProgramResult {
    require!(
        owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Account must be owned by the yDelta program expected:{} actual:{}",
        crate::ID,
        owner
    )?;
    Ok(())
}

fn verify_uninitialized<T: Pod + YdeltaAccount>(info: &AccountInfo) -> ProgramResult {
    let bytes: Ref<&mut [u8]> = info.try_borrow_data()?;
    require!(
        size_of::<T>() == bytes.len(),
        ProgramError::InvalidAccountData,
        "Incorrect length for uninitialized header expected: {} actual: {}",
        size_of::<T>(),
        bytes.len()
    )?;
    require!(
        bytes.iter().all(|&byte| byte == 0),
        ProgramError::InvalidAccountData,
        "Expected zeroed",
    )?;
    Ok(())
}
