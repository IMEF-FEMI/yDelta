//! Marginfi v0.1.8 adapter. This is the only yDelta core file that
//! interprets marginfi wire formats directly.
//!
//! CPI helpers take account slices as:
//! `[explicit..., marginfi_program, remaining...]`.
//!
//! For marginfi v0.1.8:
//! - `deposit` / `repay`: N = 7. Remaining accounts may be empty.
//! - `withdraw` / `borrow`: N = 8. Remaining accounts MUST carry
//!   `(bank, oracle)` pairs for every active balance on the marginfi
//!   account so the upstream health check can run.
//!
//! Adapter implementations validate `accounts[..N]` against the IDL's
//! shape, forward `accounts[N+1..]` verbatim as `remaining_accounts`, and
//! pass the full slice to `invoke_signed` (which finds the program at
//! index `N` via the ix's `program_id`).

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

/// Account-list lengths per marginfi v0.1.8 ix. Matches the explicit
/// `Accounts` struct in the IDL — anything beyond is `remaining_accounts`.
const DEPOSIT_EXPLICIT_ACCOUNTS: usize = 7;
const WITHDRAW_EXPLICIT_ACCOUNTS: usize = 8;
const BORROW_EXPLICIT_ACCOUNTS: usize = 8;
const REPAY_EXPLICIT_ACCOUNTS: usize = 7;

/// Zero-sized adapter — all integration state lives in the accounts callers
/// pass to each method.
#[derive(Default, Clone, Copy)]
pub struct MarginfiV18Adapter;

// ─────────────────── Conversions ───────────────────

/// Bit-pattern reinterpret: marginfi's `WrappedI80F48` ↔ yDelta's
/// `u128` (fp48). Both representations are 128 bits with implicit 48-bit
/// fractional, so the positive-value cast is lossless and free.
///
/// Marginfi's `asset_share_value`, `liability_share_value`, and weight
/// fields should always be non-negative in healthy state. A negative
/// I80F48 (high bit set) would sign-extend to a near-`u128::MAX`
/// positive in a naive `as u128` cast, producing nonsense in
/// downstream `mul_scale` / `from_scaled_floor` math. Guard against
/// it by clamping negative values to 0 — caller code then sees a
/// degenerate (but bounded) share value rather than ~2^127 atoms.
pub fn wrapped_i80f48_to_u128(w: WrappedI80F48) -> u128 {
    let bits = w.to_i128_bits();
    if bits < 0 {
        solana_program::msg!("wrapped_i80f48_to_u128: negative bank value clamped to 0");
        return 0;
    }
    bits as u128
}

pub fn u128_to_wrapped_i80f48(scaled: u128) -> WrappedI80F48 {
    WrappedI80F48::from_i128_bits(scaled as i128)
}

// ─────────────────── Helpers ───────────────────

fn borrow_bank(bank_data: &[u8]) -> Result<&Bank, ProgramError> {
    Bank::try_from_account_data(bank_data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount.into())
}

fn borrow_marginfi_account(data: &[u8]) -> Result<&MarginfiAccount, ProgramError> {
    MarginfiAccount::try_from_account_data(data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount.into())
}

/// Read the asset_shares balance of `marginfi_account` for `bank_pk`.
/// Returns 0 if the account has no active balance for this bank yet (first
/// deposit case).
pub(crate) fn read_asset_shares_u128(
    marginfi_account_info: &AccountInfo,
    bank_pk: &solana_program::pubkey::Pubkey,
) -> Result<u128, ProgramError> {
    let data = marginfi_account_info.try_borrow_data()?;
    let mfi = borrow_marginfi_account(&data)?;
    Ok(mfi
        .find_balance(bank_pk)
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares))
        .unwrap_or(0))
}

fn read_liability_shares_u128(
    marginfi_account_info: &AccountInfo,
    bank_pk: &solana_program::pubkey::Pubkey,
) -> Result<u128, ProgramError> {
    let data = marginfi_account_info.try_borrow_data()?;
    let mfi = borrow_marginfi_account(&data)?;
    Ok(mfi
        .find_balance(bank_pk)
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares))
        .unwrap_or(0))
}

/// Convert the trail of an `&[AccountInfo]` into a `Vec<AccountMeta>` for
/// the `Instruction` the CPI builder constructs. Preserves the writable +
/// signer flags of each `AccountInfo`.
fn account_infos_to_metas(infos: &[AccountInfo]) -> Vec<AccountMeta> {
    infos
        .iter()
        .map(|a| {
            if a.is_writable {
                AccountMeta::new(*a.key, a.is_signer)
            } else {
                AccountMeta::new_readonly(*a.key, a.is_signer)
            }
        })
        .collect()
}

// ─────────────────── LendingProtocol impl ───────────────────

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
        // Need at least the explicit IDL accounts + the marginfi program
        // (at index DEPOSIT_EXPLICIT_ACCOUNTS).
        if accounts.len() < DEPOSIT_EXPLICIT_ACCOUNTS + 1 {
            return Err(AdapterError::InvalidIntegrationAccount.into());
        }
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let signer_token_account = &accounts[4];
        let liquidity_vault = &accounts[5];
        let token_program = &accounts[6];
        // accounts[7] = marginfi_program (forwarded via invoke_signed).
        // accounts[8..] = remaining_accounts (empty for deposit).

        // Validate the bank up front so a bad pointer fails before the CPI.
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
    ) -> Result<u64, ProgramError> {
        if accounts.len() < WITHDRAW_EXPLICIT_ACCOUNTS + 1 {
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

        // Convert shares → expected atoms via the current bank share price.
        let bank_pk = *bank.key;
        let expected_atoms_u128 = {
            let data = bank.try_borrow_data()?;
            let bank_view = borrow_bank(&data)?;
            let asv_u128 = wrapped_i80f48_to_u128(bank_view.asset_share_value);
            let atoms = crate::math::mul_scale(shares, asv_u128)?;
            crate::math::from_scaled_floor(atoms)
        };
        if expected_atoms_u128 > u64::MAX as u128 {
            return Err(ProgramError::ArithmeticOverflow);
        }
        let expected_atoms = expected_atoms_u128 as u64;

        // Snapshot the destination balance pre-CPI so we can verify the
        // post-CPI atom delta is within ±1 of `expected_atoms`.
        let pre_destination = token_balance_of(destination_token_account)?;

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
            None, // we pass the exact atom count; `withdraw_all` is a separate flow
            &account_infos_to_metas(&accounts[WITHDRAW_EXPLICIT_ACCOUNTS + 1..]),
        );
        invoke_signed(&ix, accounts, signer_seeds)?;

        let post_destination = token_balance_of(destination_token_account)?;
        let actual_atoms = post_destination.saturating_sub(pre_destination);
        // Drift safety net (mirrors marginfi's `assert_within_one_token`).
        let diff = (actual_atoms as i128 - expected_atoms as i128).abs();
        if diff > 1 {
            return Err(AdapterError::UnexpectedAtomDelta.into());
        }
        Ok(actual_atoms)
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
        let group = &accounts[0];
        let marginfi_account = &accounts[1];
        let authority = &accounts[2];
        let bank = &accounts[3];
        let signer_token_account = &accounts[4];
        let liquidity_vault = &accounts[5];
        let token_program = &accounts[6];

        // shares → expected_atoms via current liability share price.
        // Ceil-round on the liability side: a floor would let the caller
        // settle a debt for `expected_atoms - 1` and the ±1 drift gate
        // would still accept it, leaving the protocol short.
        let bank_pk = *bank.key;
        let expected_atoms_u128 = {
            let data = bank.try_borrow_data()?;
            let bank_view = borrow_bank(&data)?;
            let lsv_u128 = wrapped_i80f48_to_u128(bank_view.liability_share_value);
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
        repay_atoms_inner(
            accounts,
            amount_atoms,
            /*repay_all=*/ false,
            signer_seeds,
        )
    }

    fn shares_to_amount<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        shares: u128,
    ) -> Result<u64, ProgramError> {
        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
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
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
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
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let lsv_u128 = wrapped_i80f48_to_u128(bank.liability_share_value);
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
        let data = bank_ai.try_borrow_data()?;
        let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&data)
            .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
        Ok((
            wrapped_i80f48_to_u128(cfg.asset_weight_init()),
            wrapped_i80f48_to_u128(cfg.liability_weight_init()),
        ))
    }

    fn maint_weight<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
    ) -> Result<(u128, u128), ProgramError> {
        let bank_ai = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        let data = bank_ai.try_borrow_data()?;
        let cfg = marginfi_mocks::state::BankConfigView::try_from_account_data(&data)
            .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
        Ok((
            wrapped_i80f48_to_u128(cfg.asset_weight_maint()),
            wrapped_i80f48_to_u128(cfg.liability_weight_maint()),
        ))
    }
}

impl MarginfiV18Adapter {
    /// Repay marginfi liability with `repay_all = Some(true)` semantics
    /// — retires the entire `liability_shares` for this bank regardless
    /// of the `amount_atoms` cap. The atom cap still applies to the
    /// internal SPL transfer, so the staging account must hold enough
    /// to cover the live liability.
    ///
    /// Returns the share count actually burned (always equal to the
    /// pre-CPI `liability_shares`).
    ///
    /// Used by `convert_p2pool_to_fixed` on the full-conversion path:
    /// `repay_atoms` floor-rounds atoms → shares inside marginfi, so
    /// passing exactly `live_outstanding_atoms` can leave sub-1-atom
    /// dust shares on the borrower's account. `repay_all = true`
    /// closes the position cleanly.
    pub fn repay_atoms_full<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_atoms: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u128, ProgramError> {
        repay_atoms_inner(
            accounts,
            amount_atoms,
            /*repay_all=*/ true,
            signer_seeds,
        )
    }

    /// Withdraw with `withdraw_all = Some(true)` semantics — closes the
    /// authority's entire asset-share balance on this bank in a single
    /// CPI, returning whatever atoms marginfi delivers.
    ///
    /// Why this exists: the floor/ceil-rounding `withdraw` path can
    /// over-request shares by 1 when caller code adds a small atom
    /// cushion to absorb marginfi's ±1 drift. When the bank balance is
    /// already tiny (e.g. a vault funded with N atoms quoting an ask of
    /// up to N atoms — the test scenario `bid_partial_match_residual_
    /// p2pool_borrows` exercises with N=40), that 1-share overshoot
    /// drains the position, and marginfi v0.1.8 hard-errors with
    /// `OperationWithdrawOnly` (6020) demanding `withdraw_all=Some(true)`
    /// whenever a withdraw zeros a balance. This helper takes that
    /// codepath: pass it the live atom balance (or `u64::MAX`) and it
    /// returns the actual atoms transferred. No ±1 drift gate — the
    /// caller is closing the position, so a smaller-than-expected
    /// delivery is the bank's terminal state, not a drift error.
    ///
    /// Account layout matches the trait `withdraw`. `amount_cap` is the
    /// transfer cap marginfi enforces on its SPL transfer out.
    pub fn withdraw_atoms_full<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        amount_cap: u64,
        signer_seeds: &[&[&[u8]]],
    ) -> Result<u64, ProgramError> {
        if accounts.len() < WITHDRAW_EXPLICIT_ACCOUNTS + 1 {
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

    /// CEIL variant of `amount_to_asset_shares` — converts `amount_atoms`
    /// to asset shares rounding the share count UP whenever the division
    /// leaves a fractional remainder.
    ///
    /// `convert_p2pool_to_fixed` withdraws asset shares from the
    /// lender vault and repays the borrower's P2Pool liability with the
    /// resulting atoms, while the new fixed loans are sized from
    /// `total_filled_principal`. The floor-rounding `amount_to_asset_shares`
    /// can yield a share count whose atom value is `total_filled_principal
    /// − 1` (or −2 with the `withdraw` drift band), so the borrower would
    /// end up owing MORE fixed debt than the variable debt actually
    /// retired. Rounding the withdraw shares UP guarantees the withdrawn
    /// atoms (hence the repaid variable debt) is `>= total_filled_principal`
    /// — the borrower never owes phantom debt. The worst-case over-withdraw
    /// is a sub-atom-scale dust amount.
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
        let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
        let amount_fp48 = crate::math::to_scaled(amount_atoms as u128)?;
        let shares_floor = crate::math::div_scale(amount_fp48, asv_u128)?;
        // Round up iff the floor-rounded shares convert back to fewer
        // atoms than requested. `mul_scale` floors, mirroring the
        // bank's own share→atom conversion in `withdraw`, so this test
        // detects exactly the rounding loss that would shortchange the
        // borrower-side liability retirement.
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

    /// The atom amount to pass to `repay_atoms` so the borrower's
    /// retired marginfi LIABILITY is `>= target_liability_atoms`.
    ///
    /// marginfi's repay processor floor/round-down-converts atoms →
    /// liability shares (it rounds in the protocol's favour), so
    /// repaying exactly `target_liability_atoms` can retire a share
    /// count whose atom value is `target_liability_atoms − 1` (or −2).
    /// `convert_p2pool_to_fixed` sizes the borrower's NEW fixed debt
    /// from `total_filled_principal`, so an under-retired liability
    /// would leave the borrower owing more fixed debt than the variable
    /// debt destroyed.
    ///
    /// This helper rounds `target_liability_atoms` UP to a whole number
    /// of liability shares and adds a small fixed cushion to absorb
    /// marginfi's internal repay-rounding (its exact rounding mode is
    /// an implementation detail of the dumped v0.1.8 program — the
    /// cushion bounds the residual without depending on it). The
    /// over-repay is a sub-atom-scale dust amount; marginfi clamps the
    /// repay at the borrower's live liability, so a padded amount can
    /// never over-burn — at worst it retires the whole liability and
    /// the unspent cushion stays in the staging vault.
    pub fn liability_atoms_to_fully_cover<'info>(
        &self,
        accounts: &[AccountInfo<'info>],
        target_liability_atoms: u64,
    ) -> Result<u64, ProgramError> {
        /// Atoms of cushion added on top of the share-boundary
        /// round-up — bounds marginfi's internal repay-rounding loss.
        /// 8 atoms is economically negligible (e.g. 8e-6 USDC).
        const REPAY_ROUNDING_CUSHION_ATOMS: u64 = 8;

        let bank_info = accounts
            .first()
            .ok_or(AdapterError::InvalidIntegrationAccount)?;
        let data = bank_info.try_borrow_data()?;
        let bank = borrow_bank(&data)?;
        let lsv_u128 = wrapped_i80f48_to_u128(bank.liability_share_value);
        // shares = ceil(target / lsv) = ceil((target << 48) / lsv).
        let target_fp48 = crate::math::to_scaled(target_liability_atoms as u128)?;
        let shares_floor = crate::math::div_scale(target_fp48, lsv_u128)?;
        let shares_back =
            crate::math::from_scaled_floor(crate::math::mul_scale(shares_floor, lsv_u128)?);
        let shares_ceil = if shares_back < target_liability_atoms as u128 {
            shares_floor
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?
        } else {
            shares_floor
        };
        // atoms = ceil(shares_ceil × lsv) + cushion.
        let atoms = crate::math::from_scaled_ceil(crate::math::mul_scale(shares_ceil, lsv_u128)?)?;
        let atoms = atoms
            .checked_add(REPAY_ROUNDING_CUSHION_ATOMS as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        u64::try_from(atoms).map_err(|_| ProgramError::ArithmeticOverflow)
    }

    /// Atom value of a LIABILITY share count, floored —
    /// `floor(shares × liability_share_value)`. The `shares_to_amount`
    /// trait method uses `asset_share_value`; this is its liability-side
    /// counterpart. Used to measure the variable debt actually
    /// retired by a `repay_atoms` CPI.
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
        let lsv_u128 = wrapped_i80f48_to_u128(bank.liability_share_value);
        let atoms = crate::math::from_scaled_floor(crate::math::mul_scale(shares, lsv_u128)?);
        u64::try_from(atoms).map_err(|_| ProgramError::ArithmeticOverflow)
    }
}

/// Shared CPI body for `repay_atoms` and `repay_atoms_full`. The
/// `repay_all` boolean toggles marginfi's `LendingAccountRepay`
/// `repay_all` flag — when `true`, marginfi retires the entire
/// `liability_shares` and ignores any rounding-residual dust from the
/// `amount_atoms` cap.
fn repay_atoms_inner<'info>(
    accounts: &[AccountInfo<'info>],
    amount_atoms: u64,
    repay_all: bool,
    signer_seeds: &[&[&[u8]]],
) -> Result<u128, ProgramError> {
    if accounts.len() < REPAY_EXPLICIT_ACCOUNTS + 1 {
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
    Ok(pre_liability.saturating_sub(post_liability))
}

/// Read the SPL token account balance at offset 64..72 (the standard layout
/// for both spl-token and spl-token-2022 token accounts; the first 64 bytes
/// are mint + owner).
fn token_balance_of(info: &AccountInfo) -> Result<u64, ProgramError> {
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

    // ─────────────────── Conversion round-trips ───────────────────

    #[test]
    fn one_point_zero_round_trips() {
        let one_i80f48 = WrappedI80F48::from_i128_bits(1i128 << 48);
        let scaled = wrapped_i80f48_to_u128(one_i80f48);
        assert_eq!(scaled, SCALE);
        assert_eq!(scaled, 1u128 << 48);
        let back = u128_to_wrapped_i80f48(SCALE);
        assert_eq!(back.to_i128_bits(), 1i128 << 48);
    }

    #[test]
    fn share_price_above_one_round_trips() {
        let bits = (21i128 << 48) / 20;
        let w = WrappedI80F48::from_i128_bits(bits);
        let scaled = wrapped_i80f48_to_u128(w);
        assert_eq!(scaled, bits as u128);
        assert_eq!(u128_to_wrapped_i80f48(scaled).to_i128_bits(), bits);
    }

    #[test]
    fn zero_round_trips() {
        let scaled = wrapped_i80f48_to_u128(WrappedI80F48::ZERO);
        assert_eq!(scaled, 0);
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
            let scaled = wrapped_i80f48_to_u128(w);
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
        let asv_u128 = (3u128 << 48) / 2; // 1.5 in fp48
        let shares = SCALE; // 1.0 in fp48 (= 1 share)
        let atoms_fp48 = crate::math::mul_scale(shares, asv_u128).unwrap();
        assert_eq!(from_scaled_floor(atoms_fp48), 1);
    }
}
