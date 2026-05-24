use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{DataIndex, HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanPromotedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{LoanFixed, LoanType, LOAN_FIXED_SIZE, LOAN_SEED};
use crate::state::market::{get_helper_matched_loan, MarketFixed, MatchedLoan};
use crate::state::market_helpers::release_address_on_market_fixed;
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::utils::create_account;
use crate::validation::loaders::ProcessMatchedLoanContext;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ProcessMatchedLoanParams {
    pub matched_loan_sequence: u64,

    pub matched_loan_index_hint: Option<DataIndex>,
}

pub fn process_process_matched_loan(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ProcessMatchedLoanParams::try_from_slice(data)?;
    let ctx = ProcessMatchedLoanContext::load(accounts, params.matched_loan_sequence)?;

    process_primary_promotion(program_id, ctx)
}

fn process_primary_promotion(program_id: &Pubkey, ctx: ProcessMatchedLoanContext) -> ProgramResult {
    let ProcessMatchedLoanContext {
        payer,
        market,
        loan,
        loan_bump,
        debt_bank,
        system_program,
        queue_node: node,
        queue_node_index: node_index,
        vault_settle,
    } = ctx;
    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    let loan_type = LoanType::from_u8(node.loan_type)?;

    let presettled: bool =
        node.flags & crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED != 0;
    let net_principal: u64 = node
        .principal_atoms
        .checked_sub(node.origination_atoms)
        .ok_or(YdeltaError::InvalidArgument)?;

    require!(
        net_principal > 0,
        YdeltaError::InvalidArgument,
        "process_matched_loan: net_principal is 0 (origination_atoms {} == principal_atoms {})",
        node.origination_atoms,
        node.principal_atoms
    )?;

    let seq_le = node.sequence.to_le_bytes();
    let bump_arr = [loan_bump];
    let seeds: Vec<Vec<u8>> = vec![
        LOAN_SEED.to_vec(),
        market_key.to_bytes().to_vec(),
        seq_le.to_vec(),
        bump_arr.to_vec(),
    ];
    create_account(
        payer.info,
        loan.info,
        system_program.info,
        program_id,
        &Rent::get()?,
        LOAN_FIXED_SIZE as u64,
        seeds,
    )?;

    let node_says_vault_lender: bool =
        node.flags & crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER != 0;

    let (lender_kind, lender_profile_id, lender_global_vault): (u8, u8, Pubkey) = {
        let market_data = market.info.try_borrow_data()?;
        let claimed_seats_root = {
            let fixed: &MarketFixed =
                bytemuck::from_bytes(&market_data[..std::mem::size_of::<MarketFixed>()]);
            fixed.claimed_seats_root_index
        };
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];

        crate::state::market::verify_live_seat(
            dynamic,
            claimed_seats_root,
            node.borrower_seat_index,
            crate::state::OWNER_KIND_USER,
        )?;

        if node.lender_seat_index == NIL {
            require!(
                !node_says_vault_lender,
                YdeltaError::IncorrectAccount,
                "MatchedLoan has NIL lender seat but VAULT_LENDER flag set"
            )?;
            (0u8, 0u8, Pubkey::default())
        } else {
            require!(
                node_says_vault_lender,
                YdeltaError::IncorrectAccount,
                "MatchedLoan has a lender seat but VAULT_LENDER flag clear"
            )?;
            let seat = crate::state::market::verify_live_seat(
                dynamic,
                claimed_seats_root,
                node.lender_seat_index,
                crate::state::OWNER_KIND_RISK_PROFILE,
            )?;
            (
                crate::state::OWNER_KIND_RISK_PROFILE,
                seat.risk_profile_id,
                seat.owner,
            )
        }
    };

    // H-1: read the snapshot taken at match time (stored on the MatchedLoan
    // node), NOT `market.fee_config.curator_fee_bps`. The market value can
    // change between match and promotion — a compromised admin who flips it
    // to 10_000 after the lender's capital is already encumbered would
    // otherwise lock depositors into 0% yield retroactively.
    let curator_fee_bps_snapshot: u16 = if lender_kind == crate::state::OWNER_KIND_RISK_PROFILE {
        node.curator_fee_bps_snapshot
    } else {
        0
    };

    let loan_started_at_unix = if presettled {
        node.matched_at_unix
    } else {
        now_unix_ts
    };
    let loan_fixed = LoanFixed::new_from_matched_loan_with_lender(
        market_key,
        node.sequence,
        loan_bump,
        *payer.info.key,
        node.lender_seat_index,
        node.borrower_seat_index,
        node.principal_atoms,
        net_principal,
        node.collateral_atoms,
        node.borrower_rate_bps,
        node.lender_rate_bps,
        node.term_seconds,
        loan_started_at_unix,
        node.flags,
        loan_type,
        node.borrower_marginfi_borrow_shares,
        lender_kind,
        lender_profile_id,
        lender_global_vault,
        curator_fee_bps_snapshot,
        node.lender_debt_share_price_snapshot_fp48,
        node.borrower_collateral_share_price_snapshot_fp48,
    );
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let dst: &mut [u8] = &mut loan_data[..LOAN_FIXED_SIZE];
        dst.copy_from_slice(bytemuck::bytes_of(&loan_fixed));
    }

    let credited_shares: u128 = match loan_type {
        LoanType::Fixed if !presettled => {
            MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], net_principal)?
        }
        LoanType::Fixed => 0,
        LoanType::P2Pool => 0,
    };

    let origination_shares: u128 = if loan_type == LoanType::P2Pool {
        0
    } else if node.origination_atoms > 0 {
        MarginfiV18Adapter
            .amount_to_asset_shares(&[debt_bank.info.clone()], node.origination_atoms)?
    } else {
        0
    };

    if lender_kind == crate::state::OWNER_KIND_RISK_PROFILE
        && loan_type == LoanType::Fixed
        && !presettled
    {
        let vault_settle = vault_settle
            .as_ref()
            .ok_or_else(|| {
                solana_program::msg!(
                    "process_matched_loan: risk-profile lender match requires vault settlement accounts"
                );
                YdeltaError::IncorrectAccount
            })?;
        do_vault_settle(
            node.principal_atoms,
            &debt_bank,
            vault_settle,
            node.lender_rate_bps,
            curator_fee_bps_snapshot,
            lender_profile_id,
            market_key,
            now_unix_ts,
        )?;
    }

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);

        if loan_type == LoanType::Fixed && credited_shares > 0 {
            da.deposit_to_seat(node.borrower_seat_index, credited_shares, true)?;
        }

        da.fixed.accumulated_protocol_fee_shares = da
            .fixed
            .accumulated_protocol_fee_shares
            .checked_add(origination_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        let mut tree = crate::state::market::MatchedLoanTree::new(
            da.dynamic,
            da.fixed.matched_loans_root_index,
            NIL,
        );
        tree.remove_by_index(node_index);
        da.fixed.matched_loans_root_index = tree.get_root_index();
        drop(tree);
        release_address_on_market_fixed(da.fixed, da.dynamic, node_index);
    }

    emit_stack(LoanPromotedLog {
        market: market_key,
        loan: *loan.info.key,
        sequence: node.sequence,
        cranker: *payer.info.key,
        principal_atoms: node.principal_atoms,
        net_principal_atoms: net_principal,
        origination_atoms: node.origination_atoms,
        credited_shares,
        origination_shares,
    })?;

    Ok(())
}

#[allow(dead_code)]
fn lookup_matched_loan(
    fixed: &MarketFixed,
    dynamic: &[u8],
    sequence: u64,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    if let Some(hint_idx) = hint {
        if hint_idx != NIL {
            let node = get_helper_matched_loan(dynamic, hint_idx).get_value();
            if node.sequence == sequence {
                return Ok(hint_idx);
            }
        }
    }
    let tree = crate::state::market::MatchedLoanTreeReadOnly::new(
        dynamic,
        fixed.matched_loans_root_index,
        NIL,
    );
    let mut probe = MatchedLoan::default();
    probe.sequence = sequence;
    Ok(tree.lookup_index(&probe))
}

#[allow(clippy::too_many_arguments)]
fn do_vault_settle<'a, 'info>(
    principal_atoms: u64,
    debt_bank: &'a crate::validation::MarginfiBankInfo<'a, 'info>,
    settle: &'a crate::validation::loaders::VaultSettleAccounts<'a, 'info>,
    lender_rate_bps: u16,
    curator_fee_bps: u16,
    profile_id: u8,
    market_key: Pubkey,
    now_unix_ts: i64,
) -> ProgramResult {
    if principal_atoms == 0 {
        return Ok(());
    }

    let vault_key = *settle.vault.info.key;
    let vault_bytes = vault_key.to_bytes();
    let vault_signer_bump_arr = [settle.global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] = &[
        crate::state::vault::GLOBAL_VAULT_SIGNER_SEED,
        &vault_bytes,
        &vault_signer_bump_arr,
    ];

    let market_bytes = market_key.to_bytes();
    let market_signer_bump_arr = [settle.market_signer_bump];
    let market_signer_seeds: &[&[u8]] = &[
        crate::validation::MARKET_SIGNER_SEED,
        &market_bytes,
        &market_signer_bump_arr,
    ];

    let withdraw_cover_atoms = principal_atoms
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let withdraw_accounts: Vec<AccountInfo> = vec![
        settle.marginfi_group.info.clone(),
        settle.global_vault_integration_account.info.clone(),
        settle.global_vault_signer.clone(),
        debt_bank.info.clone(),
        settle.global_vault_staging.info.clone(),
        settle.bank_liquidity_vault_authority.clone(),
        settle.liquidity_vault.info.clone(),
        settle.token_program.info.clone(),
        settle.marginfi_program.info.clone(),
        debt_bank.info.clone(),
        settle.bank_oracle.clone(),
    ];

    let mfi_asset_shares_pre: u128 = crate::protocol::marginfi::read_asset_shares_u128(
        settle.global_vault_integration_account.info,
        debt_bank.info.key,
    )?;
    let mfi_atoms_pre: u64 =
        MarginfiV18Adapter.shares_to_amount(&[debt_bank.info.clone()], mfi_asset_shares_pre)?;
    // M-12: fail fast if the vault's pre-withdraw balance can't even
    // cover the loan's principal. The drain path then re-checks
    // `actual_atoms >= principal_atoms` post-CPI, but that's CU wasted
    // on a doomed marginfi.withdraw_atoms_full. Short-circuit here.
    require!(
        mfi_atoms_pre >= principal_atoms,
        YdeltaError::InvalidArgument,
        "vault settle: pre-withdraw vault balance {} < principal {} — refuse to enter drain path",
        mfi_atoms_pre,
        principal_atoms,
    )?;
    let actual_atoms = if withdraw_cover_atoms >= mfi_atoms_pre {
        MarginfiV18Adapter.withdraw_atoms_full(
            &withdraw_accounts,
            mfi_atoms_pre,
            &[global_vault_signer_seeds],
        )?
    } else {
        let expected_shares: u128 = MarginfiV18Adapter
            .amount_to_asset_shares_ceil(&[debt_bank.info.clone()], withdraw_cover_atoms)?;
        let (atoms, _shares_burned) = MarginfiV18Adapter.withdraw(
            &withdraw_accounts,
            expected_shares,
            &[global_vault_signer_seeds],
        )?;
        atoms
    };
    require!(
        actual_atoms >= principal_atoms,
        ProgramError::ArithmeticOverflow,
        "vault settle underfunded: actual_atoms {} < principal_atoms {}",
        actual_atoms,
        principal_atoms
    )?;
    let surplus_atoms: u64 = actual_atoms.saturating_sub(principal_atoms);

    if settle.token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            settle.token_program.info.key,
            settle.global_vault_staging.info.key,
            settle.mint.info.key,
            settle.market_debt_vault.info.key,
            settle.global_vault_signer.key,
            &[],
            principal_atoms,
            settle.mint.mint.decimals,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                settle.global_vault_staging.info.clone(),
                settle.mint.info.clone(),
                settle.market_debt_vault.info.clone(),
                settle.global_vault_signer.clone(),
                settle.token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    } else {
        let ix = spl_token::instruction::transfer(
            settle.token_program.info.key,
            settle.global_vault_staging.info.key,
            settle.market_debt_vault.info.key,
            settle.global_vault_signer.key,
            &[],
            principal_atoms,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                settle.global_vault_staging.info.clone(),
                settle.market_debt_vault.info.clone(),
                settle.global_vault_signer.clone(),
                settle.token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    }

    let deposit_accounts: Vec<AccountInfo> = vec![
        settle.marginfi_group.info.clone(),
        settle.market_lender_integration_account.info.clone(),
        settle.market_signer.clone(),
        debt_bank.info.clone(),
        settle.market_debt_vault.info.clone(),
        settle.liquidity_vault.info.clone(),
        settle.token_program.info.clone(),
        settle.marginfi_program.info.clone(),
    ];
    let _credited: u128 =
        MarginfiV18Adapter.deposit(&deposit_accounts, principal_atoms, &[market_signer_seeds])?;

    if surplus_atoms > 0 {
        let redeposit_accounts: Vec<AccountInfo> = vec![
            settle.marginfi_group.info.clone(),
            settle.global_vault_integration_account.info.clone(),
            settle.global_vault_signer.clone(),
            debt_bank.info.clone(),
            settle.global_vault_staging.info.clone(),
            settle.liquidity_vault.info.clone(),
            settle.token_program.info.clone(),
            settle.marginfi_program.info.clone(),
        ];
        let _returned_shares: u128 = MarginfiV18Adapter.deposit(
            &redeposit_accounts,
            surplus_atoms,
            &[global_vault_signer_seeds],
        )?;
    }

    {
        let data: &mut RefMut<&mut [u8]> = &mut settle.vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found during vault settlement",
            profile_id
        )?;
        let profile_node = get_mut_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_mut_value();

        let share_value_fp48 =
            crate::state::vault::read_bank_asset_share_value_fp48(debt_bank.info)?;
        crate::state::vault::accrue_risk_profile(profile, now_unix_ts, share_value_fp48)?;

        profile.deployed_principal_atoms = profile
            .deployed_principal_atoms
            .checked_add(principal_atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let weighted_delta: u128 = (principal_atoms as u128)
            .checked_mul(lender_rate_bps as u128)
            .ok_or(crate::program::YdeltaError::MathOverflow)?;
        profile.total_weighted_rate_bps = profile
            .total_weighted_rate_bps
            .checked_add(weighted_delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        let net_weighted_delta: u128 = crate::math::mul_div(
            weighted_delta,
            (crate::state::loan::BPS_PER_UNIT as u128) - curator_fee_bps as u128,
            crate::state::loan::BPS_PER_UNIT as u128,
            false,
        )?;
        profile.total_weighted_net_rate_bps = profile
            .total_weighted_net_rate_bps
            .checked_add(net_weighted_delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.encumbered_in_orders_atoms = profile
            .encumbered_in_orders_atoms
            .saturating_sub(principal_atoms);
    }

    Ok(())
}
