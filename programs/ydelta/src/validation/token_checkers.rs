use crate::require;
use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use spl_token_2022::{
    check_spl_token_program_account,
    extension::StateWithExtensions,
    state::{Account as TokenAccount, AccountState, Mint},
};
use std::ops::Deref;

#[derive(Clone)]
pub struct MintAccountInfo<'a, 'info> {
    pub mint: Mint,
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MintAccountInfo<'a, 'info> {
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

#[derive(Clone)]
pub struct TokenAccountInfo<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> TokenAccountInfo<'a, 'info> {
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

pub fn get_vault_address(market: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(market_vault_seeds!(market, mint), &crate::ID)
}
