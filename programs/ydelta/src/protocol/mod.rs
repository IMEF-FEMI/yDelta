pub mod marginfi;
pub mod marginfi_rate_calc;
pub mod oracles;

use solana_program::{account_info::AccountInfo, program_error::ProgramError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProtocolId {
    Marginfi = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AdapterError {
    UnexpectedAtomDelta = 100,

    InvalidIntegrationAccount = 101,

    OracleStale = 102,

    OracleSetupUnsupported = 103,

    OracleMaxConfidenceExceeded = 104,

    OracleAccountMismatch = 105,

    OracleNonPositive = 106,
}

impl From<AdapterError> for ProgramError {
    fn from(e: AdapterError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

pub trait LendingProtocol {
    fn id(&self) -> ProtocolId;

    fn deposit<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    /// H-14: returns `(actual_atoms_withdrawn, actual_shares_burned)`.
    /// Caller's bookkeeping MUST decrement seat / share counts by
    /// `actual_shares_burned`, NOT by the input `shares` — marginfi
    /// runs its own intra-CPI accrual which bumps `asset_share_value`,
    /// so the actual burn is `ceil(expected_atoms / new_asv)`, strictly
    /// ≤ the input. Using the input would drift the recorded count
    /// above the real marginfi count over time.
    fn withdraw<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<(u64, u128), ProgramError>;

    fn borrow<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    fn repay<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError>;

    fn repay_atoms<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError>;

    fn shares_to_amount<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError>;

    fn amount_to_asset_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    fn amount_to_liability_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError>;

    fn oracle_price<'info>(&self, accounts: &[AccountInfo<'info>]) -> Result<u128, ProgramError>;

    fn init_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;

    fn maint_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError>;
}
