//! `ydelta-test-harness` — a separate program loaded alongside `ydelta.so`
//! and `marginfi.so` in integration tests. Exposes test-only wrapper ixs
//! around each `ydelta::protocol::marginfi::MarginfiV18Adapter` method so
//! the trait surface can be exercised end-to-end through real CPI without
//! adding test-only ixs to ydelta's production surface.
//!
//! Mirrors marginfi-v2's `programs/mocks/` pattern (see
//! `reference/marginfi-v2/programs/mocks/` for the same shape).
//!
//! Each wrapper ix:
//! 1. Borsh-deserialises a small param struct.
//! 2. Calls the corresponding adapter method with the supplied account
//!    list (per the trait's slice convention: `accounts[..N]` IDL-explicit,
//!    `accounts[N..]` health/remaining).
//! 3. Emits the result as a `sol_log_data` event so tests can read it back
//!    via `BanksClient::get_transaction()`.

use borsh::{BorshDeserialize, BorshSerialize};
use num_enum::TryFromPrimitive;
use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use marginfi_mocks::cpi::{initialize_marginfi_account_ix, InitializeMarginfiAccounts};
use solana_program::program::invoke;
use ydelta::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};

declare_id!("Fm3cEUk47y2NWZhDzpL8mRVTg8JY1QCwbzCPjGLgvty9");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// Test-only ixs. Tag values are arbitrary and stable for this crate.
#[derive(TryFromPrimitive, Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum HarnessInstruction {
    /// Call `MarginfiV18Adapter::shares_to_amount(shares)`. Accounts:
    /// `[bank]`. Param: `shares: u128`.
    SharesToAmount = 0,
    /// Call `MarginfiV18Adapter::amount_to_shares(amount)`. Accounts:
    /// `[bank]`. Param: `amount: u64`.
    AmountToShares = 1,
    /// CPI marginfi's `marginfi_account_initialize`. Accounts mirror the
    /// IDL: `[marginfi_group, marginfi_account (writable+signer),
    /// authority (signer), fee_payer (writable+signer), system_program,
    /// marginfi_program]`. The harness forwards via `invoke` so the test's
    /// signers are honoured.
    InitMarginfiAccount = 2,
    /// Call `MarginfiV18Adapter::deposit(amount_atoms)`. Accounts mirror
    /// the deposit IDL list: `[group, marginfi_account, authority, bank,
    /// signer_token_account, liquidity_vault, token_program,
    /// marginfi_program]` plus any remaining health accounts (deposit
    /// usually empty).
    ///
    /// **Phase 3 retirement candidate.** `ydelta::process_deposit` now
    /// covers this codepath end-to-end via real production CPI; the
    /// `cases/deposit_withdraw.rs` SBF tests exercise it directly. The
    /// harness arm stays in place to keep `adapter_deposit.rs` compiling
    /// (which uses the harness as a thin trampoline). Deletion is
    /// scheduled for the Phase 11 cleanup sweep.
    Deposit = 3,
    /// Call `MarginfiV18Adapter::withdraw(shares)`. Accounts mirror the
    /// withdraw IDL list: `[group, marginfi_account, authority, bank,
    /// destination_token_account, bank_liquidity_vault_authority,
    /// liquidity_vault, token_program, marginfi_program]` plus
    /// `(bank, oracle)` pairs in remaining for the health check.
    ///
    /// **Phase 3 retirement candidate.** Covered by
    /// `ydelta::process_withdraw`; see Deposit's note above.
    Withdraw = 4,
    /// Call `MarginfiV18Adapter::borrow(amount_atoms)`. Accounts mirror
    /// the borrow IDL list (same shape as withdraw) plus `(bank, oracle)`
    /// remaining for the health check.
    ///
    /// **Stays through Phase 3.** Ydelta has no `process_borrow` ix.
    /// Borrows happen as a side effect of the matching engine once the
    /// P2Pool fallback path is active. Until then, this harness arm is
    /// the only way to drive the adapter's `borrow` from a test.
    Borrow = 5,
    /// Call `MarginfiV18Adapter::repay(shares)`. Accounts mirror the
    /// repay IDL list: `[group, marginfi_account, authority, bank,
    /// signer_token_account, liquidity_vault, token_program,
    /// marginfi_program]`. Repay doesn't run a health check; remaining
    /// accounts are normally empty.
    ///
    /// **Phase 3 retirement candidate.** `ydelta::process_repay` covers
    /// the borrower-side flow (using `marginfi.deposit` rather than
    /// `marginfi.repay` — Phase 3 is wallet-vs-wallet so no liability
    /// exists on the marginfi account; see `process_repay`'s doc).
    /// This harness arm only exercises the raw `marginfi.repay`
    /// primitive, which is what `MarginfiV18Adapter::repay` calls. It
    /// stays for adapter-level coverage of the `borrow → repay` round
    /// trip in `adapter_borrow_repay.rs`. Retire alongside Borrow when
    /// Phase 4 lands.
    Repay = 6,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SharesToAmountParams {
    pub shares: u128,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct AmountToSharesParams {
    pub amount: u64,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct DepositParams {
    pub amount_atoms: u64,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct WithdrawParams {
    pub shares: u128,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct BorrowParams {
    pub amount_atoms: u64,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct RepayParams {
    pub shares: u128,
}

/// Result event emitted by every harness ix. Tests grep for this in the
/// program log to read back the adapter's return value.
#[derive(BorshSerialize)]
pub struct AdapterResult {
    /// 0 = shares_to_amount, 1 = amount_to_shares.
    pub which: u8,
    /// `shares_to_amount` puts atoms here; `amount_to_shares` puts shares
    /// (low 128 bits — currently always fits since yDelta's u128 is the
    /// natural type).
    pub atoms_or_shares: u128,
}

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let ix: HarnessInstruction =
        HarnessInstruction::try_from(*tag).or(Err(ProgramError::InvalidInstructionData))?;

    let adapter = MarginfiV18Adapter;

    match ix {
        HarnessInstruction::SharesToAmount => {
            let params = SharesToAmountParams::try_from_slice(data)?;
            let atoms = adapter.shares_to_amount(accounts, params.shares)?;
            emit_result(0, atoms as u128);
        }
        HarnessInstruction::AmountToShares => {
            let params = AmountToSharesParams::try_from_slice(data)?;
            let shares = adapter.amount_to_asset_shares(accounts, params.amount)?;
            emit_result(1, shares);
        }
        HarnessInstruction::InitMarginfiAccount => {
            // Account list: marginfi_group, marginfi_account, authority,
            // fee_payer, system_program, marginfi_program.
            if accounts.len() < 6 {
                return Err(ProgramError::NotEnoughAccountKeys);
            }
            let ix = initialize_marginfi_account_ix(&InitializeMarginfiAccounts {
                marginfi_group: *accounts[0].key,
                marginfi_account: *accounts[1].key,
                authority: *accounts[2].key,
                fee_payer: *accounts[3].key,
                system_program: *accounts[4].key,
            });
            // accounts[5] is marginfi_program; pass through to invoke so
            // the runtime sees it as required.
            invoke(&ix, &accounts[..5])?;
            emit_result(2, 0);
        }
        HarnessInstruction::Deposit => {
            let params = DepositParams::try_from_slice(data)?;
            let shares = adapter.deposit(accounts, params.amount_atoms, &[])?;
            emit_result(3, shares);
        }
        HarnessInstruction::Withdraw => {
            let params = WithdrawParams::try_from_slice(data)?;
            let atoms = adapter.withdraw(accounts, params.shares, &[])?;
            emit_result(4, atoms as u128);
        }
        HarnessInstruction::Borrow => {
            let params = BorrowParams::try_from_slice(data)?;
            let liability_shares = adapter.borrow(accounts, params.amount_atoms, &[])?;
            emit_result(5, liability_shares);
        }
        HarnessInstruction::Repay => {
            let params = RepayParams::try_from_slice(data)?;
            let atoms = adapter.repay(accounts, params.shares, &[])?;
            emit_result(6, atoms as u128);
        }
    }

    let _ = program_id;
    Ok(())
}

/// Emit the adapter's return value via `sol_log_data` so the integration
/// test can decode it back through `BanksClient::get_transaction()`.
fn emit_result(which: u8, value: u128) {
    let payload = AdapterResult {
        which,
        atoms_or_shares: value,
    };
    let mut buf = Vec::with_capacity(1 + 16);
    payload.serialize(&mut buf).ok();
    solana_program::log::sol_log_data(&[&buf]);
}
