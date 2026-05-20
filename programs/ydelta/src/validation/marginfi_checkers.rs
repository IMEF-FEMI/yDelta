//! Typed wrappers for marginfi accounts at the loader boundary.

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

/// `marginfi.MarginfiGroup` account validated for ownership + discriminator.
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

/// `marginfi.Bank` account validated for ownership + discriminator. The
/// processor can read fields via `marginfi_mocks::state::Bank::try_from_account_data`.
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

    /// Like `new`, plus binds the bank's on-disk `group` field to a
    /// caller-supplied expected marginfi-group pubkey. Without this a
    /// caller could substitute any well-formed marginfi group account,
    /// since the loader otherwise only owner/discriminator-checks the
    /// group. The expected group is the one pinned on
    /// `MarketFixed.marginfi_group` / `GlobalVaultFixed.integration_pool`.
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

/// `marginfi.MarginfiAccount` account validated for ownership + discriminator.
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

    /// Like `new`, plus binds the on-disk `authority` field to a
    /// caller-expected signer (typically `market_signer` or
    /// `global_vault_signer`). Defense-in-depth: PDA-key checks on the
    /// account address are the primary line of defense, but if
    /// marginfi's PDA derivation ever drifts (or a future loader
    /// refactor drops the address check) we still reject silently
    /// hijacked marginfi accounts. Read-cheap (no CPI).
    pub fn new_with_expected_authority(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_authority: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let validated = Self::new(info, marginfi_program_id)?;
        Self::assert_fields(info, Some(expected_authority), None)?;
        Ok(validated)
    }

    /// Like `new`, plus binds the on-disk `group` field to a
    /// caller-supplied expected marginfi-group pubkey, the same binding
    /// `MarginfiBankInfo::new_with_expected_group` performs for banks.
    pub fn new_with_expected_group(
        info: &'a AccountInfo<'info>,
        marginfi_program_id: &Pubkey,
        expected_group: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let validated = Self::new(info, marginfi_program_id)?;
        Self::assert_fields(info, None, Some(expected_group))?;
        Ok(validated)
    }

    /// Like `new_with_expected_authority`, plus binds `group`. Used by
    /// loaders that have both a vault/market signer and a pinned group
    /// in scope. Decodes the account body once for both checks.
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

    /// Single-borrow check of the marginfi account's `authority` and/or
    /// `group` fields against caller expectations. Decodes the body
    /// once so callers that need both checks don't pay two borrows
    /// (compute matters on the hot CPI paths).
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

/// Variadic oracle account slice for one marginfi bank. Marginfi v0.1.8
/// supports several `OracleSetup` variants whose oracle account counts
/// differ — Pyth-push / Switchboard-pull need 1, `StakedWithPythPush`
/// needs 3, and the post-v0.1.8 wrappers (Kamino/Drift/Solend/Juplend)
/// need 2. Loaders read `BankConfigView::expected_oracle_account_count()`
/// and pull that many `AccountInfo`s from the ix account list, validating
/// each against the bank's `oracle_keys[i]`. Processors hand the slice
/// to `MarginfiV18Adapter::oracle_price` and to the marginfi CPI's
/// `remaining_accounts` (one `(bank, …oracles)` tuple per active balance).
///
/// Capped at `MAX_ORACLE_KEYS = 5` (matches marginfi-v2's reservation in
/// `BankConfig.oracle_keys: [Pubkey; 5]`). The `count` is the on-disk
/// expectation; `ais.len() == count` always.
#[derive(Clone)]
pub struct MarginfiOracleAis<'a, 'info> {
    /// Oracle accounts in the order marginfi expects them, validated
    /// against `bank.config.oracle_keys[0..count]`.
    pub ais: Vec<&'a AccountInfo<'info>>,
    /// Mirrors `BankConfigView::expected_oracle_account_count()` for
    /// the bank these oracles belong to. Stored explicitly so callers
    /// don't have to re-borrow the bank to know the slice length.
    pub count: u8,
}

impl<'a, 'info> MarginfiOracleAis<'a, 'info> {
    /// Pull `count` oracle accounts from `account_iter`, validating each
    /// against the bank's `oracle_keys[i]`. `bank` must already be
    /// loaded — the helper borrows its data to read `BankConfigView`.
    ///
    /// Errors with `IncorrectAccount` if (a) the bank's `OracleSetup` is
    /// `None` (count = 0), (b) any oracle key mismatches, or (c) the iter
    /// is exhausted before `count` accounts have been read.
    ///
    /// The decoder side (`protocol/oracles.rs::read_oracle_price`) is
    /// where setups without offset-based decoders (Kamino/Drift/Solend
    /// wrappers, etc.) get rejected with `OracleSetupUnsupported`. The
    /// loader stays setup-agnostic — it only knows the count and the
    /// pubkey-pinning rules.
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

    /// Borrow the oracle slice as `&[&AccountInfo]`. Used by callers
    /// building the `[bank, …oracles]` argument to
    /// `MarginfiV18Adapter::oracle_price`.
    pub fn as_slice(&self) -> &[&'a AccountInfo<'info>] {
        &self.ais
    }
}

/// Marginfi v2 reserves 5 slots in `BankConfig.oracle_keys`. yDelta
/// matches the cap so our wrapper can hold any forward-compatible
/// setup without an unsigned-saturate edge.
pub const MAX_ORACLE_KEYS: usize = 5;

/// Convenience: clone every `AccountInfo` in `[bank, …oracles]` into a
/// fresh `Vec<AccountInfo>` suitable for passing to
/// `MarginfiV18Adapter::oracle_price`. Two callers per ix (debt + collateral),
/// 1-3 entries each — cheap.
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

/// The marginfi program account itself. Validates the pubkey matches the
/// vendored `marginfi_mocks::ID` so we can't be tricked into CPIs against a
/// rogue program.
///
/// TODO(marginfi-v2): pre-mainnet, swap `marginfi_mocks::ID` for the
/// real marginfi v2 program id and verify ABI compatibility of
/// `Bank` / `MarginfiAccount` decoders + `borrow_ix` / `deposit_ix` /
/// `repay_ix` / `withdraw_ix` CPI builders. yDelta deploys ONE program
/// to mainnet; the mocks crate is test-only.
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
