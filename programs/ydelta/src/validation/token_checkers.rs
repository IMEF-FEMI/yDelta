//! SPL Token / Token-2022 account-shape wrappers and the market-vault
//! PDA derivation. Loaders use these to prove a `&AccountInfo` is an
//! initialized mint or token account with the expected mint/owner.

use crate::require;
use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use spl_token_2022::{
    check_spl_token_program_account,
    extension::StateWithExtensions,
    state::{Account as TokenAccount, AccountState, Mint},
};
use std::ops::Deref;

/// Typed proof that `info` is an initialized SPL or Token-2022 mint.
/// Carries a decoded [`Mint`] snapshot taken at construction.
#[derive(Clone)]
pub struct MintAccountInfo<'a, 'info> {
    pub mint: Mint,
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MintAccountInfo<'a, 'info> {
    /// Construct after asserting owner is SPL Token / Token-2022 and
    /// `Mint::is_initialized`. Decodes the mint via Token-2022
    /// `StateWithExtensions` so legacy and extended mints both unpack.
    pub fn new(info: &'a AccountInfo<'info>) -> Result<MintAccountInfo<'a, 'info>, ProgramError> {
        check_spl_token_program_account(info.owner)?;

        let mint: Mint = StateWithExtensions::<Mint>::unpack(&info.try_borrow_data()?)?.base;
        require!(
            mint.is_initialized,
            ProgramError::InvalidAccountData,
            "Mint account is not initialized",
        )?;

        Ok(Self { mint, info })
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for MintAccountInfo<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}

/// Typed proof that `info` is an initialized SPL / Token-2022 token
/// account with a specific mint.
#[derive(Clone)]
pub struct TokenAccountInfo<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> TokenAccountInfo<'a, 'info> {
    /// Construct after asserting the SPL Token program owner,
    /// `AccountState::Initialized`, and `account.mint == mint`.
    pub fn new(
        info: &'a AccountInfo<'info>,
        mint: &Pubkey,
    ) -> Result<TokenAccountInfo<'a, 'info>, ProgramError> {
        require!(
            info.owner == &spl_token::id() || info.owner == &spl_token_2022::id(),
            ProgramError::IllegalOwner,
            "Token account must be owned by the Token Program",
        )?;

        let data = info.try_borrow_data()?;
        let token_account = StateWithExtensions::<TokenAccount>::unpack(&data)?.base;

        require!(
            token_account.state == AccountState::Initialized,
            ProgramError::InvalidAccountData,
            "Token account is uninitialized or frozen",
        )?;
        require!(
            token_account.mint == *mint,
            ProgramError::InvalidAccountData,
            "Token account mint mismatch",
        )?;
        Ok(Self { info })
    }

    /// Decode the `owner` field (bytes 32..64) without re-unpacking
    /// the full token-account layout.
    pub fn get_owner(&self) -> Result<Pubkey, ProgramError> {
        let data = self.info.try_borrow_data()?;
        if data.len() < 64 {
            return Err(ProgramError::InvalidAccountData);
        }
        let arr: [u8; 32] = data[32..64]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        Ok(Pubkey::new_from_array(arr))
    }

    /// Decode the `amount` field (bytes 64..72) without re-unpacking
    /// the full token-account layout.
    pub fn get_balance_atoms(&self) -> Result<u64, ProgramError> {
        let data = self.info.try_borrow_data()?;
        if data.len() < 72 {
            return Err(ProgramError::InvalidAccountData);
        }
        let bytes: [u8; 8] = data[64..72]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Stricter [`Self::new`] that additionally asserts
    /// `account.owner == owner`.
    pub fn new_with_owner(
        info: &'a AccountInfo<'info>,
        mint: &Pubkey,
        owner: &Pubkey,
    ) -> Result<TokenAccountInfo<'a, 'info>, ProgramError> {
        let token_account_info = Self::new(info, mint)?;
        let data = info.try_borrow_data()?;
        let token_account = StateWithExtensions::<TokenAccount>::unpack(&data)?.base;
        require!(
            token_account.owner == *owner,
            ProgramError::IllegalOwner,
            "Token account owner mismatch",
        )?;
        Ok(token_account_info)
    }

    /// Strictest variant — asserts `info.key == key` first (used to
    /// pin a vault PDA), then delegates to [`Self::new_with_owner`].
    pub fn new_with_owner_and_key(
        info: &'a AccountInfo<'info>,
        mint: &Pubkey,
        owner: &Pubkey,
        key: &Pubkey,
    ) -> Result<TokenAccountInfo<'a, 'info>, ProgramError> {
        require!(
            info.key == key,
            ProgramError::InvalidInstructionData,
            "Invalid pubkey for Token Account {:?}",
            info.key
        )?;
        Self::new_with_owner(info, mint, owner)
    }
}

impl<'a, 'info> AsRef<AccountInfo<'info>> for TokenAccountInfo<'a, 'info> {
    fn as_ref(&self) -> &AccountInfo<'info> {
        self.info
    }
}

impl<'a, 'info> Deref for TokenAccountInfo<'a, 'info> {
    type Target = AccountInfo<'info>;

    fn deref(&self) -> &Self::Target {
        self.info
    }
}

#[macro_export]
macro_rules! market_vault_seeds {
    ( $market:expr, $mint:expr ) => {
        &[b"vault", $market.as_ref(), $mint.as_ref()]
    };
}

#[macro_export]
macro_rules! market_vault_seeds_with_bump {
    ( $market:expr, $mint:expr, $bump:expr ) => {
        &[&[b"vault", $market.as_ref(), $mint.as_ref(), &[$bump]]]
    };
}

/// Derive `(vault_ata, bump)` for a market's per-mint vault PDA.
/// Authority is the market signer PDA from [`crate::validation::pdas`].
pub fn get_vault_address(market: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(market_vault_seeds!(market, mint), &crate::ID)
}
