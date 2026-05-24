use solana_program::{
    account_info::{next_account_info, AccountInfo},
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::ops::Deref;
use std::slice::Iter;

use marginfi_mocks::state::{
    assert_is_marginfi_account, assert_is_marginfi_group, Bank, BankConfigView,
};

use crate::program::YdeltaError;
use crate::require;

#[derive(Clone)]
pub struct MarginfiGroupInfo<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MarginfiGroupInfo<'a, 'info> {
    pub fn new(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
    ) -> Result<Self, ProgramError> {
        require!(
            info.owner == marginfi_program_id,
            YdeltaError::IncorrectAccount,
            "MarginfiGroup not owned by marginfi program (owner={:?})",
            info.owner
        )?;
        let data = info.try_borrow_data()?;
        assert_is_marginfi_group(&data).map_err(|_| YdeltaError::IncorrectAccount)?;
        drop(data);
        Ok(Self { info })
    }
}

impl<'a, 'info> Deref for MarginfiGroupInfo<'a, 'info> {
    type Target = AccountInfo<'info>;
    fn deref(&self) -> &Self::Target {
        self.info
    }
}

#[derive(Clone)]
pub struct MarginfiBankInfo<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MarginfiBankInfo<'a, 'info> {
    pub fn new(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
    ) -> Result<Self, ProgramError> {
        require!(
            info.owner == marginfi_program_id,
            YdeltaError::IncorrectAccount,
            "Bank not owned by marginfi program (owner={:?})",
            info.owner
        )?;
        let data = info.try_borrow_data()?;
        Bank::try_from_account_data(&data).map_err(|_| YdeltaError::IncorrectAccount)?;
        drop(data);
        Ok(Self { info })
    }

    pub fn new_with_expected_group(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_group: &Pubkey,
    ) -> Result<Self, ProgramError> {
        require!(
            info.owner == marginfi_program_id,
            YdeltaError::IncorrectAccount,
            "Bank not owned by marginfi program (owner={:?})",
            info.owner
        )?;
        let data = info.try_borrow_data()?;
        let bank = Bank::try_from_account_data(&data).map_err(|_| YdeltaError::IncorrectAccount)?;
        require!(
            bank.group == *expected_group,
            YdeltaError::IncorrectAccount,
            "bank.group {} does not match expected marginfi_group {}",
            bank.group,
            expected_group
        )?;
        drop(data);
        Ok(Self { info })
    }
}

impl<'a, 'info> Deref for MarginfiBankInfo<'a, 'info> {
    type Target = AccountInfo<'info>;
    fn deref(&self) -> &Self::Target {
        self.info
    }
}

#[derive(Clone)]
pub struct MarginfiAccountInfo<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MarginfiAccountInfo<'a, 'info> {
    pub fn new(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
    ) -> Result<Self, ProgramError> {
        require!(
            info.owner == marginfi_program_id,
            YdeltaError::IncorrectAccount,
            "MarginfiAccount not owned by marginfi program (owner={:?})",
            info.owner
        )?;
        let data = info.try_borrow_data()?;
        assert_is_marginfi_account(&data).map_err(|_| YdeltaError::IncorrectAccount)?;
        drop(data);
        Ok(Self { info })
    }

    pub fn new_with_expected_authority(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_authority: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let validated = Self::new(info, marginfi_program_id)?;
        Self::assert_fields(info, Some(expected_authority), None)?;
        Ok(validated)
    }

    pub fn new_with_expected_group(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_group: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let validated = Self::new(info, marginfi_program_id)?;
        Self::assert_fields(info, None, Some(expected_group))?;
        Ok(validated)
    }

    pub fn new_with_expected_authority_and_group(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_authority: &Pubkey,
        expected_group: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let validated = Self::new(info, marginfi_program_id)?;
        Self::assert_fields(info, Some(expected_authority), Some(expected_group))?;
        Ok(validated)
    }

    fn assert_fields(
        info: &'a AccountInfo<'info>,
        expected_authority: Option<&Pubkey>,
        expected_group: Option<&Pubkey>,
    ) -> Result<(), ProgramError> {
        let data = info.try_borrow_data()?;
        let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&data)
            .map_err(|_| YdeltaError::IncorrectAccount)?;
        if let Some(authority) = expected_authority {
            require!(
                mfi.authority == *authority,
                YdeltaError::IncorrectAccount,
                "marginfi_account.authority {} does not match expected signer {}",
                mfi.authority,
                authority
            )?;
        }
        if let Some(group) = expected_group {
            require!(
                mfi.group == *group,
                YdeltaError::IncorrectAccount,
                "marginfi_account.group {} does not match expected marginfi_group {}",
                mfi.group,
                group
            )?;
        }
        drop(data);
        Ok(())
    }
}

impl<'a, 'info> Deref for MarginfiAccountInfo<'a, 'info> {
    type Target = AccountInfo<'info>;
    fn deref(&self) -> &Self::Target {
        self.info
    }
}

#[derive(Clone)]
pub struct MarginfiOracleAis<'a, 'info> {
    pub ais: Vec<&'a AccountInfo<'info>>,

    pub count: u8,
}

impl<'a, 'info> MarginfiOracleAis<'a, 'info> {
    pub fn load(
        account_iter: &mut Iter<'a, AccountInfo<'info>>,
        bank: &'a AccountInfo<'info>,
    ) -> Result<Self, ProgramError> {
        let bank_data = bank.try_borrow_data()?;
        let cfg = BankConfigView::try_from_account_data(&bank_data)
            .map_err(|_| YdeltaError::IncorrectAccount)?;
        let count = cfg.expected_oracle_account_count();
        require!(
            count > 0,
            YdeltaError::IncorrectAccount,
            "bank has OracleSetup::None or unsupported setup (count = 0)"
        )?;
        require!(
            count as usize <= MAX_ORACLE_KEYS,
            YdeltaError::IncorrectAccount,
            "bank requires {} oracles; cap is {}",
            count,
            MAX_ORACLE_KEYS
        )?;
        let mut ais: Vec<&'a AccountInfo<'info>> = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let ai = next_account_info(account_iter)?;
            require!(
                *ai.key == cfg.oracle_key(i),
                YdeltaError::IncorrectAccount,
                "oracle account [{}] {} does not match bank.config.oracle_keys[{}] {}",
                i,
                ai.key,
                i,
                cfg.oracle_key(i)
            )?;
            ais.push(ai);
        }
        drop(bank_data);
        Ok(Self { ais, count })
    }

    pub fn as_slice(&self) -> &[&'a AccountInfo<'info>] {
        &self.ais
    }
}

pub const MAX_ORACLE_KEYS: usize = 5;

pub fn oracle_price_args<'info>(
    bank: &AccountInfo<'info>,
    oracle_ais: &MarginfiOracleAis<'_, 'info>,
) -> Vec<AccountInfo<'info>> {
    let mut out = Vec::with_capacity(1 + oracle_ais.count as usize);
    out.push(bank.clone());
    for ai in &oracle_ais.ais {
        out.push((*ai).clone());
    }
    out
}

#[derive(Clone)]
pub struct MarginfiProgram<'a, 'info> {
    pub info: &'a AccountInfo<'info>,
}

impl<'a, 'info> MarginfiProgram<'a, 'info> {
    pub fn new(info: &'a AccountInfo<'info>) -> Result<Self, ProgramError> {
        require!(
            info.key == &marginfi_mocks::ID,
            YdeltaError::IncorrectAccount,
            "marginfi_program account key does not match expected program id"
        )?;
        Ok(Self { info })
    }
}

impl<'a, 'info> Deref for MarginfiProgram<'a, 'info> {
    type Target = AccountInfo<'info>;
    fn deref(&self) -> &Self::Target {
        self.info
    }
}
