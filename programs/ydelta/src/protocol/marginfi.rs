//! marginfi v0.1.8 [`LendingProtocol`] adapter. Wraps deposit / withdraw
//! / borrow / repay CPIs and the read-only share / weight queries. Each
//! mutating call reads pre-state, invokes marginfi, then re-reads
//! post-state and returns the realized delta — refusing to mask CPI
//! drift behind a predicted value.

use solana_program::{
    account_info::AccountInfo, instruction::AccountMeta, program::invoke_signed,
    program_error::ProgramError,
};

use marginfi_mocks::cpi::{
    borrow_ix, deposit_ix, repay_ix, withdraw_ix, BorrowAccounts, DepositAccounts, RepayAccounts,
    WithdrawAccounts,
};
use marginfi_mocks::state::{Bank, MarginfiAccount};
use marginfi_mocks::wire::WrappedI80F48;

use super::{AdapterError, LendingProtocol, ProtocolId};

const DEPOSIT_EXPLICIT_ACCOUNTS: usize = 7;
const WITHDRAW_EXPLICIT_ACCOUNTS: usize = 8;
const BORROW_EXPLICIT_ACCOUNTS: usize = 8;
const REPAY_EXPLICIT_ACCOUNTS: usize = 7;

/// Zero-sized [`LendingProtocol`] impl for marginfi v0.1.8. Stateless;
/// all per-call context is passed through `accounts` and `signer_seeds`.
#[derive(Default, Clone, Copy)]
pub struct MarginfiV18Adapter;

/// Reinterprets a marginfi `WrappedI80F48` as the program's unsigned
/// fp48 `u128`. Rejects negative bit patterns with
/// `InvalidIntegrationAccount` rather than silently coercing to zero
/// (asset/liability share values are invariants ≥ 0).
pub fn wrapped_i80f48_to_u128(w: WrappedI80F48) -> Result<u128, ProgramError> {
    let bits = w.to_i128_bits();
    if bits < 0 {
        solana_program::msg!(
            "wrapped_i80f48_to_u128: negative I80F48 bit pattern ({}) — refusing silent 0",
            bits,
        );
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(bits as u128)
}

/// Reverse of [`wrapped_i80f48_to_u128`]. Returns the bit-equivalent
/// `WrappedI80F48` for an unsigned fp48 `u128`. Round-trips losslessly.
pub fn u128_to_wrapped_i80f48(scaled: u128) -> WrappedI80F48 {
    WrappedI80F48::from_i128_bits(scaled as i128)
}

fn borrow_bank(bank_data: &[u8]) -> Result<&Bank, ProgramError> {
    Bank::try_from_account_data(bank_data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount.into())
}

fn assert_marginfi_owned(
    bank_info: &solana_program::account_info::AccountInfo,
) -> Result<(), ProgramError> {
    if bank_info.owner != &marginfi_mocks::ID {
        solana_program::msg!(
            "marginfi adapter: bank account {} owned by {} (expected marginfi program {})",
            bank_info.key,
            bank_info.owner,
            marginfi_mocks::ID,
        );
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(())
}

fn borrow_marginfi_account(data: &[u8]) -> Result<&MarginfiAccount, ProgramError> {
    MarginfiAccount::try_from_account_data(data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount.into())
}

/// Reads `marginfi_account.balance_for(bank).asset_shares` as a `u128`
/// (rounded from I80F48; negative bit patterns clamp to 0). Returns 0
/// when the account has no active balance for the bank. Public so the
/// withdraw processor can cap drain-all expected_shares at the bank's
/// actual share count (the seat may over-count from deposit-time floor
/// rounding; uncapped withdraws hit MarginfiError::OperationWithdrawOnly).
pub fn read_asset_shares_u128(
    marginfi_account_info: &AccountInfo,
    bank_pk: &solana_program::pubkey::Pubkey,
) -> Result<u128, ProgramError> {
    let data = marginfi_account_info.try_borrow_data()?;
    let mfi = borrow_marginfi_account(&data)?;
    match mfi.find_balance(bank_pk) {
        Some(b) => wrapped_i80f48_to_u128(b.asset_shares),
        None => Ok(0),
    }
}

fn read_liability_shares_u128(
    marginfi_account_info: &AccountInfo,
    bank_pk: &solana_program::pubkey::Pubkey,
) -> Result<u128, ProgramError> {
    let data = marginfi_account_info.try_borrow_data()?;
    let mfi = borrow_marginfi_account(&data)?;
    match mfi.find_balance(bank_pk) {
        Some(b) => wrapped_i80f48_to_u128(b.liability_shares),
        None => Ok(0),
    }
}

fn account_infos_to_metas(infos: &[AccountInfo]) -> Vec<AccountMeta> {
    infos
        .iter()
        .map(|a| AccountMeta::new_readonly(*a.key, false))
        .collect()
}

impl LendingProtocol for MarginfiV18Adapter {
    fn id(&self) -> ProtocolId {
        ProtocolId::Marginfi
    }

    fn deposit<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError> {
        if accounts.len() < DEPOSIT_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        if accounts[DEPOSIT_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let signer_token_account = &accounts[4];
        let liquidity_vault = &accounts[5];
        let token_program = &accounts[6];

        {
            let data = bank.try_borrow_data()?;
            borrow_bank(&data)?;
        }

        let bank_pk = *bank.key;
        let pre_shares = read_asset_shares_u128(marginfi_account, &bank_pk)?;

        let ix = deposit_ix(
            &DepositAccounts {
                group: *group.key,
                marginfi_account: *marginfi_account.key,
                authority: *authority.key,
                bank: bank_pk,
                signer_token_account: *signer_token_account.key,
                liquidity_vault: *liquidity_vault.key,
                token_program: *token_program.key,
            },
            amount_atoms,
            None,
            &account_infos_to_metas(&accounts[DEPOSIT_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;

        let post_shares = read_asset_shares_u128(marginfi_account, &bank_pk)?;
        Ok(post_shares.saturating_sub(pre_shares))
    }

    fn withdraw<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<(u64, u128), ProgramError> {
        if accounts.len() < WITHDRAW_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        if accounts[WITHDRAW_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let destination_token_account = &accounts[4];
        let bank_liquidity_vault_authority = &accounts[5];
        let liquidity_vault = &accounts[6];
        let token_program = &accounts[7];

        let bank_pk = *bank.key;
        let expected_atoms_u128 = {
            let data = bank.try_borrow_data()?;
            let bank_view = borrow_bank(&data)?;
            let asv_u128 = wrapped_i80f48_to_u128(bank_view.asset_share_value)?;
            let atoms = crate::math::mul_scale(shares, asv_u128)?;
            crate::math::from_scaled_floor(atoms)
        };
        if expected_atoms_u128 > u64::MAX as u128 {
            return Err(ProgramError::ArithmeticOverflow);
        }
        let expected_atoms = expected_atoms_u128 as u64;

        let pre_destination = token_balance_of(destination_token_account)?;
        let pre_asset_shares = read_asset_shares_u128(marginfi_account, &bank_pk)?;

        let ix = withdraw_ix(
            &WithdrawAccounts {
                group: *group.key,
                marginfi_account: *marginfi_account.key,
                authority: *authority.key,
                bank: bank_pk,
                destination_token_account: *destination_token_account.key,
                bank_liquidity_vault_authority: *bank_liquidity_vault_authority.key,
                liquidity_vault: *liquidity_vault.key,
                token_program: *token_program.key,
            },
            expected_atoms,
            None,
            &account_infos_to_metas(&accounts[WITHDRAW_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;

        let post_destination = token_balance_of(destination_token_account)?;
        let actual_atoms = post_destination.saturating_sub(pre_destination);

        let diff = (actual_atoms as i128 - expected_atoms as i128).abs();
        if diff > 1 {
            return Err(AdapterError::UnexpectedAtomDelta.into());
        }

        let post_asset_shares = read_asset_shares_u128(marginfi_account, &bank_pk)?;
        let actual_shares_burned = pre_asset_shares.saturating_sub(post_asset_shares);
        Ok((actual_atoms, actual_shares_burned))
    }

    fn borrow<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError> {
        if accounts.len() < BORROW_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        if accounts[BORROW_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let destination_token_account = &accounts[4];
        let bank_liquidity_vault_authority = &accounts[5];
        let liquidity_vault = &accounts[6];
        let token_program = &accounts[7];

        {
            let data = bank.try_borrow_data()?;
            borrow_bank(&data)?;
        }

        let bank_pk = *bank.key;
        let pre_liability = read_liability_shares_u128(marginfi_account, &bank_pk)?;

        let ix = borrow_ix(
            &BorrowAccounts {
                group: *group.key,
                marginfi_account: *marginfi_account.key,
                authority: *authority.key,
                bank: bank_pk,
                destination_token_account: *destination_token_account.key,
                bank_liquidity_vault_authority: *bank_liquidity_vault_authority.key,
                liquidity_vault: *liquidity_vault.key,
                token_program: *token_program.key,
            },
            amount_atoms,
            &account_infos_to_metas(&accounts[BORROW_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;

        let post_liability = read_liability_shares_u128(marginfi_account, &bank_pk)?;
        Ok(post_liability.saturating_sub(pre_liability))
    }

    fn repay<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError> {
        if accounts.len() < REPAY_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        if accounts[REPAY_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let signer_token_account = &accounts[4];
        let liquidity_vault = &accounts[5];
        let token_program = &accounts[6];

        let bank_pk = *bank.key;
        let expected_atoms_u128 = {
            let data = bank.try_borrow_data()?;
            let bank_view = borrow_bank(&data)?;
            let lsv_u128 = wrapped_i80f48_to_u128(bank_view.liability_share_value)?;
            let atoms = crate::math::mul_scale(shares, lsv_u128)?;
            crate::math::from_scaled_ceil(atoms)?
        };
        if expected_atoms_u128 > u64::MAX as u128 {
            return Err(ProgramError::ArithmeticOverflow);
        }
        let expected_atoms = expected_atoms_u128 as u64;

        let pre_signer = token_balance_of(signer_token_account)?;

        let ix = repay_ix(
            &RepayAccounts {
                group: *group.key,
                marginfi_account: *marginfi_account.key,
                authority: *authority.key,
                bank: bank_pk,
                signer_token_account: *signer_token_account.key,
                liquidity_vault: *liquidity_vault.key,
                token_program: *token_program.key,
            },
            expected_atoms,
            None,
            &account_infos_to_metas(&accounts[REPAY_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;

        let post_signer = token_balance_of(signer_token_account)?;
        let actual_atoms = pre_signer.saturating_sub(post_signer);
        let diff = (actual_atoms as i128 - expected_atoms as i128).abs();
        if diff > 1 {
            return Err(AdapterError::UnexpectedAtomDelta.into());
        }
        Ok(actual_atoms)
    }

    fn repay_atoms<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError> {
        repay_atoms_inner(accounts, amount_atoms, false, signer_seeds)
    }

    fn shares_to_amount<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        assert_marginfi_owned(bank_info)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value)?;
        let atoms = crate::math::mul_scale(shares, asv_u128)?;
        let atoms = crate::math::from_scaled_floor(atoms);
        if atoms > u64::MAX as u128 {
            return Err(ProgramError::ArithmeticOverflow);
        }
        Ok(atoms as u64)
    }

    fn amount_to_asset_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        assert_marginfi_owned(bank_info)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value)?;
        let amount_fp48 = crate::math::to_scaled(amount_atoms as u128)?;
        crate::math::div_scale(amount_fp48, asv_u128)
    }

    fn amount_to_liability_shares<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        assert_marginfi_owned(bank_info)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let lsv_u128 = wrapped_i80f48_to_u128(bank.liability_share_value)?;
        let amount_fp48 = crate::math::to_scaled(amount_atoms as u128)?;
        crate::math::div_scale(amount_fp48, lsv_u128)
    }

    fn oracle_price<'info>(&self, accounts: &[AccountInfo<'info>]) -> Result<u128, ProgramError> {
        super::oracles::read_oracle_price(accounts)
    }

    fn init_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError> {
        let bank_ai = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        assert_marginfi_owned(bank_ai)?;
        let data = bank_ai.try_borrow_data()?;
        let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&data)
            .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
        Ok((
            wrapped_i80f48_to_u128(cfg.asset_weight_init())?,
            wrapped_i80f48_to_u128(cfg.liability_weight_init())?,
        ))
    }

    fn maint_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError> {
        let bank_ai = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        assert_marginfi_owned(bank_ai)?;
        let data = bank_ai.try_borrow_data()?;
        let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&data)
            .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
        Ok((
            wrapped_i80f48_to_u128(cfg.asset_weight_maint())?,
            wrapped_i80f48_to_u128(cfg.liability_weight_maint())?,
        ))
    }
}

impl MarginfiV18Adapter {
    /// Repay flavour that passes marginfi's `repay_all = true` flag.
    /// Marginfi treats the atom argument as a cap and zeroes the
    /// liability shares — used by close-out paths that fund the EXACT
    /// post-accrue debt via the off-chain accrue predictor.
    pub fn repay_atoms_full<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError> {
        repay_atoms_inner(accounts, amount_atoms, true, signer_seeds)
    }

    /// Withdraw flavour that passes marginfi's `withdraw_all = true`
    /// flag. `amount_cap` bounds the post-CPI atom delta; returns the
    /// realized atoms drained from the marginfi balance.
    pub fn withdraw_atoms_full<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_cap: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError> {
        if accounts.len() < WITHDRAW_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        if accounts[WITHDRAW_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let destination_token_account = &accounts[4];
        let bank_liquidity_vault_authority = &accounts[5];
        let liquidity_vault = &accounts[6];
        let token_program = &accounts[7];

        let pre_destination = token_balance_of(destination_token_account)?;
        let ix = withdraw_ix(
            &WithdrawAccounts {
                group: *group.key,
                marginfi_account: *marginfi_account.key,
                authority: *authority.key,
                bank: *bank.key,
                destination_token_account: *destination_token_account.key,
                bank_liquidity_vault_authority: *bank_liquidity_vault_authority.key,
                liquidity_vault: *liquidity_vault.key,
                token_program: *token_program.key,
            },
            amount_cap,
            Some(true),
            &account_infos_to_metas(&accounts[WITHDRAW_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;
        let post_destination = token_balance_of(destination_token_account)?;
        Ok(post_destination.saturating_sub(pre_destination))
    }

    /// Ceiling-rounded variant of `amount_to_asset_shares` — bumps the
    /// floored share count by 1 when the round-trip back to atoms
    /// undershoots `amount_atoms`. Used when callers need to guarantee
    /// the share count fully covers a target atom amount.
    pub fn amount_to_asset_shares_ceil<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
    ) -> Result<u128, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value)?;
        let amount_fp48 = crate::math::to_scaled(amount_atoms as u128)?;
        let shares_floor = crate::math::div_scale(amount_fp48, asv_u128)?;

        let atoms_back =
            crate::math::from_scaled_floor(crate::math::mul_scale(shares_floor, asv_u128)?);
        if atoms_back < amount_atoms as u128 {
            shares_floor
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)
        } else {
            Ok(shares_floor)
        }
    }

    /// Convert fp48 liability `shares` to atoms at the current
    /// `liability_share_value`, floored. The complementary ceiling
    /// helper takes a pre-computed lsv (see
    /// [`Self::liability_shares_to_atoms_ceil_at_lsv`]).
    pub fn liability_shares_to_atoms_floor<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let lsv_u128 = wrapped_i80f48_to_u128(bank.liability_share_value)?;
        let atoms = crate::math::from_scaled_floor(crate::math::mul_scale(shares, lsv_u128)?);
        u64::try_from(atoms).map_err(|_| ProgramError::ArithmeticOverflow)
    }

    /// Off-chain accrue prediction: returns the bank's projected fp48
    /// `liability_share_value` at `now_unix`, replicating marginfi's
    /// interest-rate curve + accrue formula without a CPI. Delegates
    /// to [`crate::protocol::marginfi_rate_calc`].
    pub fn calc_projected_liability_share_value_fp48(
        &self,
        bank_info: &AccountInfo,
        group_info: &AccountInfo,
        now_unix: i64,
    ) -> Result<u128, ProgramError> {
        crate::protocol::marginfi_rate_calc::calc_projected_liability_share_value_fp48(
            bank_info, group_info, now_unix,
        )
    }

    /// Off-chain accrue prediction: returns the bank's projected fp48
    /// `asset_share_value` at `now_unix`. Mirrors the liability path
    /// but uses the lender APR (base × utilization).
    pub fn calc_projected_asset_share_value_fp48(
        &self,
        bank_info: &AccountInfo,
        group_info: &AccountInfo,
        now_unix: i64,
    ) -> Result<u128, ProgramError> {
        crate::protocol::marginfi_rate_calc::calc_projected_asset_share_value_fp48(
            bank_info, group_info, now_unix,
        )
    }

    /// `shares_fp48 × lsv_fp48 / 2^48`, ceiled to atoms. Pre-computed
    /// lsv lets callers pair with [`Self::calc_projected_liability_share_value_fp48`]
    /// to fund the EXACT post-accrue liability in a close-out CPI.
    pub fn liability_shares_to_atoms_ceil_at_lsv(
        &self,
        shares_fp48: u128,
        lsv_fp48: u128,
    ) -> Result<u64, ProgramError> {
        let atoms_fp48 = crate::math::mul_scale(shares_fp48, lsv_fp48)?;
        let atoms = crate::math::from_scaled_ceil(atoms_fp48)?;
        u64::try_from(atoms).map_err(|_| ProgramError::ArithmeticOverflow)
    }

    /// Compose [`Self::calc_projected_liability_share_value_fp48`] with
    /// [`Self::liability_shares_to_atoms_ceil_at_lsv`]. Returns the
    /// post-accrue atom liability for `shares_fp48` at `now_unix`; 0
    /// when `shares_fp48 == 0` short-circuits the CPI math.
    pub fn get_projected_liability_atoms_ceil(
        &self,
        bank_info: &AccountInfo,
        group_info: &AccountInfo,
        shares_fp48: u128,
        now_unix: i64,
    ) -> Result<u64, ProgramError> {
        if shares_fp48 == 0 {
            return Ok(0);
        }
        let new_lsv_fp48 =
            self.calc_projected_liability_share_value_fp48(bank_info, group_info, now_unix)?;
        self.liability_shares_to_atoms_ceil_at_lsv(shares_fp48, new_lsv_fp48)
    }

    /// Read the fp48 liability shares the marginfi account holds in
    /// the given bank. Returns 0 if the account has no balance row
    /// for `bank_pk`.
    pub fn read_account_liability_shares_fp48(
        &self,
        marginfi_account_info: &AccountInfo,
        bank_pk: &solana_program::pubkey::Pubkey,
    ) -> Result<u128, ProgramError> {
        read_liability_shares_u128(marginfi_account_info, bank_pk)
    }
}

fn repay_atoms_inner<'info>(
    accounts: &[AccountInfo<'info>],
    amount_atoms: u64,
    repay_all: bool,
    signer_seeds: &[&[&[u8]]],
) -> Result<u128, ProgramError> {
    if accounts.len() < REPAY_EXPLICIT_ACCOUNTS + 1 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    if accounts[REPAY_EXPLICIT_ACCOUNTS].key != &marginfi_mocks::ID {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let group = &accounts[0];
    let marginfi_account = &accounts[1];
    let authority = &accounts[2];
    let bank = &accounts[3];
    let signer_token_account = &accounts[4];
    let liquidity_vault = &accounts[5];
    let token_program = &accounts[6];

    let bank_pk = *bank.key;
    let pre_liability = read_liability_shares_u128(marginfi_account, &bank_pk)?;

    let ix = repay_ix(
        &RepayAccounts {
            group: *group.key,
            marginfi_account: *marginfi_account.key,
            authority: *authority.key,
            bank: bank_pk,
            signer_token_account: *signer_token_account.key,
            liquidity_vault: *liquidity_vault.key,
            token_program: *token_program.key,
        },
        amount_atoms,
        if repay_all { Some(true) } else { None },
        &account_infos_to_metas(&accounts[REPAY_EXPLICIT_ACCOUNTS + 1..]),
    );
    invoke_signed(&ix, accounts, signer_seeds)?;

    let post_liability = read_liability_shares_u128(marginfi_account, &bank_pk)?;
    pre_liability
        .checked_sub(post_liability)
        .ok_or_else(|| {
            solana_program::msg!(
                "repay_atoms_inner: post_liability ({}) > pre_liability ({}) — refusing to mask corruption",
                post_liability,
                pre_liability,
            );
            AdapterError::InvalidIntegrationAccount.into()
        })
}

pub(crate) fn token_balance_of(info: &AccountInfo) -> Result<u64, ProgramError> {
    if info.owner != &spl_token::id() && info.owner != &spl_token_2022::id() {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let data = info.try_borrow_data()?;
    if data.len() < 72 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(u64::from_le_bytes(
        data[64..72].try_into().expect("slice is 8 bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{from_scaled_floor, to_scaled, SCALE};

    #[test]
    fn one_point_zero_round_trips() {
        let one_i80f48 = WrappedI80F48::from_i128_bits(1i128 << 48);
        let scaled = wrapped_i80f48_to_u128(one_i80f48).unwrap();
        assert_eq!(scaled, SCALE);
        assert_eq!(scaled, 1u128 << 48);
        let back = u128_to_wrapped_i80f48(SCALE);
        assert_eq!(back.to_i128_bits(), 1i128 << 48);
    }

    #[test]
    fn share_price_above_one_round_trips() {
        let bits = (21i128 << 48) / 20;
        let w = WrappedI80F48::from_i128_bits(bits);
        let scaled = wrapped_i80f48_to_u128(w).unwrap();
        assert_eq!(scaled, bits as u128);
        assert_eq!(u128_to_wrapped_i80f48(scaled).to_i128_bits(), bits);
    }

    #[test]
    fn zero_round_trips() {
        let scaled = wrapped_i80f48_to_u128(WrappedI80F48::ZERO).unwrap();
        assert_eq!(scaled, 0);
    }

    #[test]
    fn negative_i80f48_errors() {
        let neg = WrappedI80F48::from_i128_bits(-1i128 << 48);
        assert!(
            wrapped_i80f48_to_u128(neg).is_err(),
            "wrapped_i80f48_to_u128 must reject negative input"
        );
        let neg_one_bit = WrappedI80F48::from_i128_bits(-1);
        assert!(wrapped_i80f48_to_u128(neg_one_bit).is_err());
        let i128_min = WrappedI80F48::from_i128_bits(i128::MIN);
        assert!(wrapped_i80f48_to_u128(i128_min).is_err());
    }

    #[test]
    fn arbitrary_bits_preserve_round_trip() {
        for raw in [
            0i128,
            1,
            (1 << 48) - 1,
            (3 << 48) / 2,
            (10_000i128 << 48) + (1 << 24),
            i128::MAX / 2,
        ] {
            let w = WrappedI80F48::from_i128_bits(raw);
            let scaled = wrapped_i80f48_to_u128(w).unwrap();
            let back = u128_to_wrapped_i80f48(scaled);
            assert_eq!(back.to_i128_bits(), raw, "round trip broken for {}", raw);
        }
    }

    #[test]
    fn shares_to_amount_at_unit_price_is_atoms_floor() {
        let atoms_in: u64 = 1_000_000;
        let shares = to_scaled(atoms_in as u128).unwrap();
        let atoms_fp48 = crate::math::mul_scale(shares, SCALE).unwrap();
        assert_eq!(from_scaled_floor(atoms_fp48), atoms_in as u128);
    }

    #[test]
    fn amount_to_shares_at_unit_price_round_trips() {
        let atoms: u64 = 5_000;
        let amount = to_scaled(atoms as u128).unwrap();
        let shares = crate::math::div_scale(amount, SCALE).unwrap();
        assert_eq!(shares, amount);
        let atoms_back = from_scaled_floor(crate::math::mul_scale(shares, SCALE).unwrap());
        assert_eq!(atoms_back, atoms as u128);
    }

    #[test]
    fn shares_to_amount_at_high_price_truncates() {
        let asv_u128 = (3u128 << 48) / 2;
        let shares = SCALE;
        let atoms_fp48 = crate::math::mul_scale(shares, asv_u128).unwrap();
        assert_eq!(from_scaled_floor(atoms_fp48), 1);
    }

    #[test]
    fn liability_shares_to_atoms_ceil_at_lsv_unit_price() {
        let adapter = MarginfiV18Adapter::default();
        let atoms_in: u64 = 1_000_000;
        let shares_fp48 = (atoms_in as u128) << 48;
        let lsv_fp48 = SCALE;
        let out = adapter
            .liability_shares_to_atoms_ceil_at_lsv(shares_fp48, lsv_fp48)
            .unwrap();
        assert_eq!(out, atoms_in);
    }

    #[test]
    fn liability_shares_to_atoms_ceil_at_lsv_ceils_fractional() {
        let adapter = MarginfiV18Adapter::default();
        let shares_fp48 = 1u128 << 48;
        let lsv_fp48 = SCALE + 1;
        let out = adapter
            .liability_shares_to_atoms_ceil_at_lsv(shares_fp48, lsv_fp48)
            .unwrap();
        assert_eq!(out, 2, "any non-zero fractional atom must ceil to next atom");
    }

    #[test]
    fn liability_shares_to_atoms_ceil_at_lsv_survives_hundred_k_usdc() {
        let adapter = MarginfiV18Adapter::default();

        let atoms: u64 = 100_000 * 1_000_000;
        let shares_fp48 = (atoms as u128) << 48;
        let lsv_fp48 = SCALE + (SCALE / 2);

        let out = adapter
            .liability_shares_to_atoms_ceil_at_lsv(shares_fp48, lsv_fp48)
            .expect("must not overflow at 100k-USDC scale (this is the C-1 regression)");

        let expected = atoms as u128 + (atoms as u128) / 2;
        assert_eq!(out as u128, expected);
    }

    #[test]
    fn liability_shares_to_atoms_ceil_at_lsv_survives_hundred_million_usdc() {
        let adapter = MarginfiV18Adapter::default();

        let atoms: u64 = 100_000_000 * 1_000_000;
        let shares_fp48 = (atoms as u128) << 48;
        let lsv_fp48 = SCALE;

        let out = adapter
            .liability_shares_to_atoms_ceil_at_lsv(shares_fp48, lsv_fp48)
            .expect("must not overflow at $100M scale");
        assert_eq!(out, atoms);
    }

    #[test]
    fn liability_shares_to_atoms_ceil_at_lsv_zero_inputs_are_zero() {
        let adapter = MarginfiV18Adapter::default();
        assert_eq!(
            adapter
                .liability_shares_to_atoms_ceil_at_lsv(0, SCALE)
                .unwrap(),
            0
        );
        assert_eq!(
            adapter
                .liability_shares_to_atoms_ceil_at_lsv(SCALE, 0)
                .unwrap(),
            0
        );
    }
}
