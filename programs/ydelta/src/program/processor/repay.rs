//! `Repay` — borrower-driven repayment for Fixed and P2Pool loans. Signer is
//! the borrower (verified against the borrower seat's owner). Handles partial
//! and full repayment; on full repay this is the only place that closes the
//! loan PDA, decrements seat encumbrance/counters, releases collateral,
//! applies sub-vault bookkeeping (weighted-rate, principal, NAV, pending
//! claim, curator fee accumulators), and refunds rent to the original
//! cranker. The companion `claim_repayment_for_sub_vault` is a stateless
//! seat→vault sweeper and never re-accrues or touches the loan PDA.

use std::cell::{Ref, RefMut};

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use hypertree::{HyperTreeReadOperations, NIL};

use crate::logs::{emit_stack, LoanRepaidLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{
    accrue_loan, apply_partial_resolution, LoanFixed, LoanState, LoanType, BPS_PER_UNIT,
    LOAN_FIXED_SIZE, SECONDS_PER_YEAR,
};
use crate::state::market::{get_helper_seat, get_mut_helper_seat, MarketFixed};
use crate::state::user_account::{remove_open_loan, UserAccountFixed};
use crate::state::vault::{
    accrue_sub_vault, get_mut_helper_sub_vault, read_bank_asset_share_value_fp48,
    GlobalVaultFixed, SubVault, SubVaultTreeReadOnly,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, USER_ACCOUNT_FIXED_SIZE};
use crate::validation::loaders::RepayContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::{get_mut_dynamic_account, invoke};

/// Repayment parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct RepayParams {
    /// Atoms to repay for a partial repay; ignored when `full_repay == true`.
    pub repay_atoms: u64,

    /// When true, pays the entire post-accrue outstanding (P2Pool uses marginfi's
    /// `repay_all` on single-loan accounts with a +1 atom accrue cushion).
    pub full_repay: bool,
    /// Optional borrower `ClaimedSeat` hint; fallback lookup runs when stale.
    pub borrower_seat_index_hint: Option<DataIndex>,
}

/// Borrower repay for Fixed and P2Pool loans. Accrues once (the only call site
/// that does so for repay/claim), applies the repay via marginfi, and on full
/// repay performs the entire close-out (loan PDA, seats, sub-vault).
pub fn process_repay(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let params: RepayParams = RepayParams::try_from_slice(data)?;
    let ctx = RepayContext::load(accounts)?;
    let RepayContext {
        payer,
        market,
        loan,
        borrower_token,
        vault,
        token_program,
        debt_mint,
        marginfi_group,
        marginfi_account,
        debt_bank,
        liquidity_vault,

        collateral_bank: _,
        borrower_marginfi_account,
        market_signer,
        market_signer_bump,
        marginfi_program,
        user_account_ai,
        cranker_refund,
        global_vault,
    } = ctx;

    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    // Snapshot every loan-body value we'll need post-CPI. accrue_loan runs
    // ONCE here — per the repay/claim split, this is the LAST place loan
    // interest is accrued (claim never re-accrues). For Fixed loans we
    // also capture the lender-side facts needed for the full-repay close-out
    // (lender_seat_index, lender_sub_vault_id, principal, lender_rate,
    // curator_fee_bps_snapshot, started_at, accumulated fee buckets).
    let (
        borrower_seat_index,
        loan_type,
        body_outstanding,
        collateral_atoms,
        collateral_snapshot_fp48,
        lender_seat_index,
        lender_sub_vault_id,
        loan_principal,
        loan_lender_rate,
        loan_curator_fee_bps,
        loan_started_at,
        loan_lender_claimable,
        loan_accumulated_curator_fee_atoms,
        loan_accumulated_protocol_fee_atoms,
    ) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        require!(
            header.state != LoanState::Repaid as u8,
            YdeltaError::InvalidArgument,
            "Loan is already Repaid (state={})",
            header.state
        )?;

        accrue_loan(header, now_unix_ts, grace_period_seconds)?;
        (
            header.borrower_seat_index,
            header.loan_type()?,
            header.outstanding_debt_atoms,
            header.collateral_atoms,
            header.borrower_collateral_share_price_snapshot_fp48,
            header.lender_seat_index,
            header.lender_sub_vault_id,
            header.principal_debt_atoms,
            header.lender_rate_bps,
            header.curator_fee_bps_snapshot,
            header.started_at_unix,
            header.lender_claimable_atoms,
            header.accumulated_curator_fee_atoms,
            header.accumulated_protocol_fee_atoms,
        )
    };

    {
        let market_data: Ref<&mut [u8]> = market.info.try_borrow_data()?;
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
        let seat = get_helper_seat(dynamic, borrower_seat_index).get_value();
        require!(
            seat.owner == *payer.info.key,
            YdeltaError::OrderNotOwnedBySigner,
            "signer is not the borrower of this loan"
        )?;
    }

    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    if loan_type == LoanType::P2Pool {
        let loan_shares: u128 = {
            let l: Ref<LoanFixed> = loan.get_fixed()?;
            l.borrower_marginfi_borrow_shares
        };
        require!(
            loan_shares > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: loan has no recorded marginfi shares (already settled?)"
        )?;

        let account_shares_pre: u128 = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        require!(
            account_shares_pre > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: live marginfi liability is 0 (already settled?)"
        )?;

        // Per-loan attribution (C-2): clamp loan_shares to account total
        // defensively so accounting drift in either direction can't
        // misattribute.
        let this_loan_shares: u128 = loan_shares.min(account_shares_pre);
        let this_loan_is_account: bool = this_loan_shares == account_shares_pre;

        // Predict marginfi's post-accrue LSV (same-slot Clock + same
        // bank.last_update → exact match with what marginfi will compute
        // inside the repay CPI).
        let predicted_lsv_fp48 = MarginfiV18Adapter.calc_projected_liability_share_value_fp48(
            debt_bank.info,
            marginfi_group.info,
            now_unix_ts,
        )?;

        let loan_live_atoms: u64 = MarginfiV18Adapter
            .liability_shares_to_atoms_ceil_at_lsv(this_loan_shares, predicted_lsv_fp48)?;
        require!(
            loan_live_atoms > 0,
            YdeltaError::InvalidArgument,
            "P2Pool repay: this loan's live debt rounds to 0"
        )?;

        // Atoms the borrower will pay to marginfi:
        //   - full_repay: pay live debt; on single-loan path add +1 atom
        //     so marginfi's repay_all clears the account cleanly (marginfi
        //     pulls only what's needed, the extra atom — if any — stays in
        //     market_debt_vault as dust for the next op to consume).
        //   - partial: pay user-specified amount (validated ≤ loan_live).
        const ACCRUE_SAFETY_ATOMS: u64 = 1;
        let repay_atoms: u64 = if params.full_repay {
            loan_live_atoms
        } else {
            require!(
                params.repay_atoms > 0,
                YdeltaError::InvalidArgument,
                "repay_atoms must be > 0"
            )?;
            require!(
                params.repay_atoms <= loan_live_atoms,
                YdeltaError::InvalidArgument,
                "repay_atoms ({}) exceeds this loan's live debt ({})",
                params.repay_atoms,
                loan_live_atoms
            )?;
            params.repay_atoms
        };
        let use_repay_all = params.full_repay && this_loan_is_account;
        let funded_atoms: u64 = if use_repay_all {
            repay_atoms
                .checked_add(ACCRUE_SAFETY_ATOMS)
                .ok_or(ProgramError::ArithmeticOverflow)?
        } else {
            repay_atoms
        };

        transfer_user_to_vault(
            token_program.info,
            borrower_token.info,
            vault.info,
            debt_mint.info,
            payer.info,
            funded_atoms,
            debt_mint.mint.decimals,
        )?;

        let repay_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            borrower_marginfi_account.info.clone(),
            market_signer.clone(),
            debt_bank.info.clone(),
            vault.info.clone(),
            liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];

        let _shares_burned: u128 = if use_repay_all {
            // Single-loan account: marginfi.repay_all computes exact
            // post-accrue liability and pulls only that much from the
            // borrower's token account (amount param is ignored). Burns
            // all account shares (= this loan, no siblings).
            MarginfiV18Adapter.repay_atoms_full(
                &repay_accounts,
                funded_atoms,
                &[market_signer_seeds],
            )?
        } else {
            // Multi-loan account OR partial repay: marginfi.repay_atoms
            // pays exact amount. Sibling shares are never touched.
            MarginfiV18Adapter.repay_atoms(&repay_accounts, funded_atoms, &[market_signer_seeds])?
        };

        // Per-loan attribution post-CPI: compute burned via account delta,
        // subtract from this loan's pre-CPI shares. ≤1 atom of residual
        // on this loan is marginfi's accrue-during-repay rounding cost —
        // treat as logically fully repaid.
        let account_shares_post: u128 = crate::state::ltv::read_borrower_liability_shares(
            borrower_marginfi_account.info,
            debt_bank.info.key,
        )?;
        let burned: u128 = account_shares_pre.saturating_sub(account_shares_post);
        let loan_shares_post: u128 = this_loan_shares.saturating_sub(burned);
        let loan_shares_post_atoms_floor: u64 = MarginfiV18Adapter
            .liability_shares_to_atoms_floor(&[debt_bank.info.clone()], loan_shares_post)?;
        let did_full_repay: bool = loan_shares_post_atoms_floor <= 1;

        let loan_remaining_shares: u128 = if did_full_repay { 0 } else { loan_shares_post };
        let post_repay_outstanding_atoms: u64 = if did_full_repay {
            0
        } else {
            crate::state::ltv::liability_shares_to_atoms_ceil(
                debt_bank.info,
                loan_remaining_shares,
            )?
        };
        {
            let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
            let header: &mut LoanFixed =
                bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
            header.outstanding_debt_atoms = post_repay_outstanding_atoms;
            header.borrower_marginfi_borrow_shares = loan_remaining_shares;
            header.last_accrued_unix = now_unix_ts;
            if did_full_repay {
                header.state = LoanState::Repaid as u8;
            }
        }

        emit_stack(LoanRepaidLog {
            market: market_key,
            loan: *loan.info.key,
            borrower: *payer.info.key,
            repay_atoms,
            outstanding_after: post_repay_outstanding_atoms,
            full_repay: did_full_repay as u8,
            _padding: [0; 7],
        })?;

        super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;

        if did_full_repay {
            {
                let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
                let da = get_mut_dynamic_account::<MarketFixed>(market_data);

                if collateral_atoms > 0 {
                    let collateral_shares =
                        crate::state::market_helpers::atoms_to_shares_at_snapshot(
                            collateral_atoms,
                            collateral_snapshot_fp48,
                        )?;
                    crate::state::market_helpers::release_loan_collateral(
                        da.dynamic,
                        borrower_seat_index,
                        collateral_shares,
                        collateral_shares,
                    )?;
                }

                let seat = get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
                seat.open_borrow_count = seat.open_borrow_count.saturating_sub(1);
            }
            drop_open_loan_ref(user_account_ai, loan.info.key)?;
            super::shared::close_account_and_refund(loan.info, cranker_refund)?;
        }

        return Ok(());
    }

    // ===== Fixed-loan repay path =====
    // Sizing: `full_repay` ignores `repay_atoms` and pays the entire
    // accrued outstanding; `partial` requires `repay_atoms > 0` and caps
    // it at `outstanding_pre`. For Fixed loans `outstanding_debt_atoms`
    // is the canonical ledger (accrue_loan maintains it — see H-4 doc).
    let outstanding_pre: u64 = body_outstanding;
    let repay_atoms: u64 = if params.full_repay {
        outstanding_pre
    } else {
        require!(
            params.repay_atoms > 0,
            YdeltaError::InvalidArgument,
            "repay_atoms must be > 0"
        )?;
        require!(
            params.repay_atoms <= outstanding_pre,
            YdeltaError::InvalidArgument,
            "repay_atoms ({}) exceeds outstanding_debt_atoms ({})",
            params.repay_atoms,
            outstanding_pre
        )?;
        params.repay_atoms
    };

    transfer_user_to_vault(
        token_program.info,
        borrower_token.info,
        vault.info,
        debt_mint.info,
        payer.info,
        repay_atoms,
        debt_mint.mint.decimals,
    )?;

    let adapter_accounts = [
        marginfi_group.info.clone(),
        marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        vault.info.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    // marginfi.deposit returns asset shares credited to the lender's
    // per-market integration account. These shares represent the lender's
    // (vault depositors + curator + protocol) claim on the repaid atoms.
    let credited_shares: u128 =
        MarginfiV18Adapter.deposit(&adapter_accounts, repay_atoms, &[market_signer_seeds])?;

    let did_full_repay: bool = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        apply_partial_resolution(header, repay_atoms)?;
        let full = header.outstanding_debt_atoms == 0;
        if full {
            header.state = LoanState::Repaid as u8;
        }
        full
    };

    let protocol_fee_shares: u128 = if did_full_repay && loan_accumulated_protocol_fee_atoms > 0 {
        MarginfiV18Adapter
            .amount_to_asset_shares(&[debt_bank.info.clone()], loan_accumulated_protocol_fee_atoms)?
    } else {
        0
    };
    let lender_claim_shares: u128 = credited_shares.saturating_sub(protocol_fee_shares);

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);

        // Bump the market's protocol-fee accumulator (full-repay only).
        if protocol_fee_shares > 0 {
            da.fixed.accumulated_protocol_fee_shares = da
                .fixed
                .accumulated_protocol_fee_shares
                .checked_add(protocol_fee_shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        // Credit the lender seat with the claim shares. This is the data
        // the curator's stateless sweep reads.
        require!(
            lender_seat_index != NIL,
            YdeltaError::InvalidArgument,
            "Fixed loan has no lender_seat_index — should be set at promotion"
        )?;
        let lender_seat =
            get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
        if lender_claim_shares > 0 {
            lender_seat.debt_withdrawable_shares = lender_seat
                .debt_withdrawable_shares
                .checked_add(lender_claim_shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }

        // Full-repay close-out: retire the lender's open-lend counter,
        // release borrower collateral, decrement borrower's
        // open_borrow_count.
        if did_full_repay {
            // Vault asks take no seat-level debt encumbrance — the profile's
            // idle/deployed atom counters are the lender-side ledger — so the
            // only seat-side close-out is retiring the open-lend counter the
            // matching engine stamped at fill time.
            lender_seat.open_lend_count = lender_seat
                .open_lend_count
                .checked_sub(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            // Borrower seat: release collateral encumbrance + decrement
            // open_borrow_count.
            if collateral_atoms > 0 {
                let collateral_shares = crate::state::market_helpers::atoms_to_shares_at_snapshot(
                    collateral_atoms,
                    collateral_snapshot_fp48,
                )?;
                crate::state::market_helpers::release_loan_collateral(
                    da.dynamic,
                    borrower_seat_index,
                    collateral_shares,
                    collateral_shares,
                )?;
            }
            let borrower_seat =
                get_mut_helper_seat(da.dynamic, borrower_seat_index).get_mut_value();
            borrower_seat.open_borrow_count =
                borrower_seat.open_borrow_count.saturating_sub(1);
        }
    }

    // Full-repay close-out: apply per-loan-derived decrements to the lender
    // vault's sub-vault, then close the loan PDA. The loan's per-loan
    // facts (principal, lender_rate, curator_fee_bps_snapshot, started_at)
    // are needed here BEFORE the PDA is zeroed; we snapshotted them above.
    // claim_repayment_for_sub_vault never reads the loan PDA after this.
    if did_full_repay {
        let global_vault = global_vault.as_ref().ok_or_else(|| {
            solana_program::msg!(
                "repay: Fixed full-repay requires global_vault account (omitted by SDK)"
            );
            YdeltaError::IncorrectAccount
        })?;

        // Apply per-loan-derived sub-vault updates.
        {
            let vault_data: &mut RefMut<&mut [u8]> =
                &mut global_vault.info.try_borrow_mut_data()?;
            let (fixed_bytes, dynamic) = vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
            let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
            let probe = SubVault::new_empty(lender_sub_vault_id, Pubkey::default(), 1, 1);
            let profile_idx = {
                let tree =
                    SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
                tree.lookup_index(&probe)
            };
            require!(
                profile_idx != NIL,
                YdeltaError::SubVaultNotFound,
                "repay: sub_vault_id {} not found on global_vault",
                lender_sub_vault_id
            )?;
            let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
            let share_value_fp48 = read_bank_asset_share_value_fp48(debt_bank.info)?;
            accrue_sub_vault(profile, now_unix_ts, share_value_fp48)?;

            // Per-loan weighted-rate accumulator decrements. These can ONLY
            // be applied at close (the per-loan rate + principal facts go
            // away with the PDA).
            let weighted_delta: u128 = (loan_principal as u128)
                .checked_mul(loan_lender_rate as u128)
                .ok_or(YdeltaError::MathOverflow)?;
            profile.total_weighted_rate_bps = profile
                .total_weighted_rate_bps
                .checked_sub(weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            let net_weighted_delta: u128 = crate::math::mul_div(
                weighted_delta,
                (BPS_PER_UNIT as u128) - loan_curator_fee_bps as u128,
                BPS_PER_UNIT as u128,
                false,
            )?;
            profile.total_weighted_net_rate_bps = profile
                .total_weighted_net_rate_bps
                .checked_sub(net_weighted_delta)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.deployed_principal_atoms = profile
                .deployed_principal_atoms
                .checked_sub(loan_principal)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            // retire the open-loan counter stamped at fill.
            profile.open_loans_count = profile
                .open_loans_count
                .checked_sub(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            // Reconcile realized vs estimated for NAV (total_assets_atoms)
            // and cost basis (total_principal_atoms). lender_claimable_atoms
            // is what the LENDER (vault) actually gets — principal + their
            // net interest slice (curator fees already excluded).
            let loan_lifetime: u128 =
                (now_unix_ts.saturating_sub(loan_started_at)).max(0) as u128;
            let yield_denom: u128 = (BPS_PER_UNIT as u128)
                .checked_mul(SECONDS_PER_YEAR as u128)
                .ok_or(YdeltaError::MathOverflow)?;
            let estimated_accrued_atoms: u128 =
                crate::math::mul_div(net_weighted_delta, loan_lifetime, yield_denom, false)?;

            let realized_net: i128 =
                (loan_lender_claimable as i128) - (loan_principal as i128);
            if realized_net >= 0 {
                let interest = realized_net as u64;
                profile.total_principal_atoms = profile
                    .total_principal_atoms
                    .checked_add(interest)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                let shortfall = (-realized_net) as u64;
                profile.total_principal_atoms =
                    profile.total_principal_atoms.saturating_sub(shortfall);
            }
            let assets_delta: i128 = realized_net - (estimated_accrued_atoms as i128);
            if assets_delta >= 0 {
                profile.total_assets_atoms = profile
                    .total_assets_atoms
                    .checked_add(assets_delta as u64)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                profile.total_assets_atoms = profile
                    .total_assets_atoms
                    .saturating_sub((-assets_delta) as u64);
            }
            crate::state::vault::restore_assets_principal_invariant(profile);

            // Pending-claim bucket — atoms now physically in
            // lender_marginfi_account but not yet swept to this vault's
            // integration_account. Excluded from idle-MTM in
            // accrue_sub_vault. claim_repayment_for_sub_vault
            // decrements this as it sweeps.
            profile.pending_claim_atoms = profile
                .pending_claim_atoms
                .checked_add(loan_lender_claimable)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            // Curator fee accumulator — earmarks how many of the eventual
            // swept atoms belong to the curator. Curator's separate
            // `claim_curator_fee` ix pulls from the vault integration
            // account using this counter.
            if loan_accumulated_curator_fee_atoms > 0 {
                profile.accumulated_curator_fee_atoms = profile
                    .accumulated_curator_fee_atoms
                    .checked_add(loan_accumulated_curator_fee_atoms)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }
    }

    let outstanding_after = outstanding_pre.saturating_sub(repay_atoms);
    emit_stack(LoanRepaidLog {
        market: market_key,
        loan: *loan.info.key,
        borrower: *payer.info.key,
        repay_atoms,
        outstanding_after,
        full_repay: did_full_repay as u8,
        _padding: [0; 7],
    })?;

    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    if did_full_repay {
        drop_open_loan_ref(user_account_ai, loan.info.key)?;
        // Close the loan PDA + refund rent to the original cranker
        // (loan.created_by, validated by RepayContext::load).
        super::shared::close_account_and_refund(loan.info, cranker_refund)?;
    }

    Ok(())
}

fn drop_open_loan_ref(user_account_ai: &AccountInfo, loan_pda_key: &Pubkey) -> ProgramResult {
    let loan_pda_key = *loan_pda_key;
    let Ok(mut data) = user_account_ai.try_borrow_mut_data() else {
        return Ok(());
    };
    let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
    let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);

    remove_open_loan(fixed, dynamic, loan_pda_key)?;
    Ok(())
}

fn transfer_user_to_vault<'info>(
    token_program: &AccountInfo<'info>,
    trader_token: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            trader_token.key,
            mint.key,
            vault.key,
            owner.key,
            &[],
            amount,
            decimals,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                mint.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            trader_token.key,
            vault.key,
            owner.key,
            &[],
            amount,
        )?;
        invoke(
            &ix,
            &[
                trader_token.clone(),
                vault.clone(),
                owner.clone(),
                token_program.clone(),
            ],
        )
    }
}
