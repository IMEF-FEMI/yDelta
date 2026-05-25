//! Generic Solana account-shape wrappers. Typed proofs that an
//! `AccountInfo` is a known program, a writable / readable signer, a
//! token program, or an uninitialized system-owned account.

use crate::require;
use solana_program::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, system_program,
};
use std::ops::Deref;

/// Typed proof that `info.key` equals a specific program id.
#[derive(Clone)]
pub struct Program<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> Program<'a, 'info> {
    /// Construct from `info` after asserting `info.key == expected_program_id`.
    pub fn new(
        info: &'a AccountInfo<'info>,
        expected_program_id: &Pubkey,
    ) -> Result<Program<'a, 'info>, ProgramError> {
        require!(
            info.key == expected_program_id,
            ProgramError::IncorrectProgramId,
            "Incorrect program id expected {:?} actual {:?}",
            expected_program_id,
            info.key
        )?;
        Ok(Self { info })
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for Program<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}

/// Typed proof that `info.key` is either SPL Token or SPL Token-2022.
#[derive(Clone)]
pub struct TokenProgram<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> TokenProgram<'a, 'info> {
    /// Construct from `info` after asserting `info.key` matches one of
    /// `spl_token::id()` or `spl_token_2022::id()`.
    pub fn new(info: &'a AccountInfo<'info>) -> Result<TokenProgram<'a, 'info>, ProgramError> {
        require!(
            *info.key == spl_token::id() || *info.key == spl_token_2022::id(),
            ProgramError::IncorrectProgramId,
            "Incorrect token program id: {:?}",
            info.key
        )?;
        Ok(Self { info })
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for TokenProgram<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}

impl<'a, 'info> Deref for TokenProgram<'a, 'info> {
    type Target = AccountInfo<'info>;

    fn deref(&self) -> &Self::Target {
        self.info
    }
}

/// Typed proof that `info.is_signer == true`.
#[derive(Clone)]
pub struct Signer<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> Signer<'a, 'info> {
    /// Construct from `info` after asserting it signed the transaction.
    pub fn new(info: &'a AccountInfo<'info>) -> Result<Signer<'a, 'info>, ProgramError> {
        require!(
            info.is_signer,
            ProgramError::MissingRequiredSignature,
            "Missing required signature for {:?}",
            info.key
        )?;
        Ok(Self { info })
    }

    /// Stricter [`Signer::new`] that additionally requires
    /// `info.is_writable == true` — for fee-paying / rent-paying signers.
    pub fn new_payer(info: &'a AccountInfo<'info>) -> Result<Signer<'a, 'info>, ProgramError> {
        require!(
            info.is_writable,
            ProgramError::InvalidInstructionData,
            "Payer is not writable. Key {:?}",
            info.key
        )?;
        require!(
            info.is_signer,
            ProgramError::MissingRequiredSignature,
            "Missing required signature for payer {:?}",
            info.key
        )?;
        Ok(Self { info })
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for Signer<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}

impl<'a, 'info> Deref for Signer<'a, 'info> {
    type Target = AccountInfo<'info>;

    fn deref(&self) -> &Self::Target {
        self.info
    }
}

/// Typed proof that `info` is an uninitialized system-owned account
/// (zero data length, owned by the system program). Used as the
/// pre-allocation handle for PDAs the program is about to create.
#[derive(Clone)]
pub struct EmptyAccount<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> EmptyAccount<'a, 'info> {
    /// Construct after asserting `info` has zero data and is owned by
    /// the system program.
    pub fn new(info: &'a AccountInfo<'info>) -> Result<EmptyAccount<'a, 'info>, ProgramError> {
        require!(
            info.data_is_empty(),
            ProgramError::InvalidAccountData,
            "Account must be uninitialized {:?}",
            info.key
        )?;
        require!(
            info.owner == &system_program::id(),
            ProgramError::IllegalOwner,
            "Empty accounts must be owned by the system program {:?}",
            info.key
        )?;
        Ok(Self { info })
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for EmptyAccount<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}
