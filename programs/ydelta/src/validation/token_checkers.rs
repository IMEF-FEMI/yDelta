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

        let mint: Mint = StateWithExtensions::<Mint>::unpack(&info.data.borrow())?.base;
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
        // Decode via `StateWithExtensions` rather than slicing raw bytes:
        // this validates the SPL / Token-2022 account layout, so a
        // too-short or non-token account is rejected cleanly instead of
        // panicking on an out-of-bounds `[0..32]` slice.
        let data = info.try_borrow_data()?;
        let token_account = StateWithExtensions::<TokenAccount>::unpack(&data)?.base;
        // Reject uninitialized AND frozen accounts — the program never
        // wants to transfer into or out of either. `delegate` /
        // `close_authority` are intentionally NOT rejected here: a
        // user-supplied source account may legitimately carry a delegate,
        // and the program's own vault accounts are PDA-owned and created
        // fresh, so they cannot carry a delegate or a non-program close
        // authority unless the owning PDA itself signed for it.
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

    pub fn get_owner(&self) -> Pubkey {
        Pubkey::new_from_array(
            self.info.try_borrow_data().unwrap()[32..64]
                .try_into()
                .unwrap(),
        )
    }

    pub fn get_balance_atoms(&self) -> u64 {
        u64::from_le_bytes(
            self.info.try_borrow_data().unwrap()[64..72]
                .try_into()
                .unwrap(),
        )
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
