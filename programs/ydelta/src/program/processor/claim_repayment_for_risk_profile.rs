//! Realize a fully repaid risk-profile loan back into the vault.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke_signed,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, RepaymentClaimedForRiskProfileLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::loan::{accrue_loan, LoanFixed, LoanState, LOAN_FIXED_SIZE};
use crate::state::market::{get_mut_helper_seat, MarketFixed};
use crate::state::vault::{
    accrue_risk_profile, get_mut_helper_risk_profile, read_bank_asset_share_value_fp48,
    GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ClaimRepaymentForRiskProfileContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy, Default)]
pub struct ClaimRepaymentForRiskProfileParams {}

pub fn process_claim_repayment_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let ClaimRepaymentForRiskProfileContext {
        payer: _,
        market,
        loan,
        global_vault,
        global_vault_signer,
        global_vault_signer_bump,
        global_vault_staging,
        global_vault_integration_account,
        market_debt_vault,
        market_signer,
        market_signer_bump,
        lender_marginfi_account,
        debt_bank,
        debt_liquidity_vault,
        debt_bank_lva,
        debt_oracle_ais,
        debt_mint,
        token_program,
        marginfi_group,
        marginfi_program,
        cranker_refund,
    } = ClaimRepaymentForRiskProfileContext::load(accounts)?;

    let market_key = *market.info.key;
    let global_vault_key = *global_vault.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;
    let grace_period_seconds: u32 = market.get_fixed()?.fee_config.grace_period_seconds;

    // Snapshot loan body and zero its lender-side accumulators under one
    // mut borrow. Subsequent CPIs operate on the snapshot. Any failure
    // after this point reverts the whole tx, so the zero is undone too.
    let (
        lender_seat_index,
        lender_profile_id,
        principal_debt_atoms,
        lender_rate_bps,
        claimable_atoms,
        protocol_fee_atoms,
        curator_fee_atoms,
    ): (hypertree::DataIndex, u8, u64, u16, u64, u64, u64) = {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let header: &mut LoanFixed = bytemuck::from_bytes_mut(&mut loan_data[..LOAN_FIXED_SIZE]);
        accrue_loan(header, now_unix_ts, grace_period_seconds)?;
        require!(
            header.outstanding_debt_atoms == 0,
            YdeltaError::InvalidArgument,
            "claim_repayment_for_risk_profile: outstanding_debt_atoms is {} \
             (must be 0 — claim is post-settlement only)",
            header.outstanding_debt_atoms
        )?;
        // Fixed-term lock-up: lender cannot drain until maturity, even
        // if the borrower repaid early.
        require!(
            now_unix_ts >= header.matures_at_unix,
            YdeltaError::LoanNotMatured,
            "claim_repayment_for_risk_profile: now ({}) < matures_at_unix ({})",
            now_unix_ts,
            { header.matures_at_unix }
        )?;
        let snapshot = (
            header.lender_seat_index,
            header.lender_profile_id,
            header.principal_debt_atoms,
            header.lender_rate_bps,
            header.lender_claimable_atoms,
            header.accumulated_protocol_fee_atoms,
            header.accumulated_curator_fee_atoms,
        );
        header.lender_claimable_atoms = 0;
        header.accumulated_protocol_fee_atoms = 0;
        header.accumulated_curator_fee_atoms = 0;
        header.state = LoanState::Repaid as u8;
        snapshot
    };

    // Convert atoms → debt-bank shares.
    //   - `claim_shares`  : the lender's net realised atoms (claimable
    //     atoms = principal + lender_net_interest after curator fee).
    //     Used for the lender-seat encumbrance decrement.
    //   - `curator_shares`: curator's manager-fee atoms accrued under
    //     `curator_fee_bps_snapshot`. Routed to vault.integration so
    //     `claim_curator_fee` can later withdraw them; the lender's
    //     seat doesn't track these — they were never the LP's claim.
    //   - `protocol_fee_shares`: spread atoms swept onto market's
    //     `accumulated_protocol_fee_shares` accumulator (atoms stay
    //     in market.lender_marginfi until protocol_fee_claim).
    //   - `total_withdraw_shares`: lender + curator combined; one CPI
    //     draws both.
    let claim_shares: u128 = if claimable_atoms > 0 {
        MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], claimable_atoms)?
    } else {
        0
    };
    let curator_shares: u128 = if curator_fee_atoms > 0 {
        MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], curator_fee_atoms)?
    } else {
        0
    };
    let total_withdraw_shares: u128 = claim_shares
        .checked_add(curator_shares)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let protocol_fee_shares: u128 = if protocol_fee_atoms > 0 {
        MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], protocol_fee_atoms)?
    } else {
        0
    };

    // Update market state: lender seat (decrement encumbered shares,
    // decrement the per-market exposure-cap usage, drop the open-lend
    // count) plus the protocol-fee sweep onto the market accumulator.
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
        require!(
            seat.owner_kind == OWNER_KIND_RISK_PROFILE,
            YdeltaError::InvalidArgument,
            "claim_repayment_for_risk_profile: lender seat is not a risk-profile seat"
        )?;
        require!(
            seat.owner == global_vault_key,
            YdeltaError::InvalidArgument,
            "claim_repayment_for_risk_profile: seat.owner does not match passed global_vault"
        )?;
        require!(
            seat.risk_profile_id == lender_profile_id,
            YdeltaError::InvalidArgument,
            "claim_repayment_for_risk_profile: seat.risk_profile_id does not match loan"
        )?;
        seat.debt_encumbered_shares = seat.debt_encumbered_shares.saturating_sub(claim_shares);
        seat.open_lend_count = seat.open_lend_count.saturating_sub(1);
        let prev_deployed = seat.deployed_atoms();
        seat.set_deployed_atoms(prev_deployed.saturating_sub(principal_debt_atoms));
        if protocol_fee_shares > 0 {
            da.fixed.accumulated_protocol_fee_shares = da
                .fixed
                .accumulated_protocol_fee_shares
                .checked_add(protocol_fee_shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    // marginfi.withdraw lender_marginfi → market_debt_vault, signed by
    // market_signer. Returns the actual atoms drawn (within ±1 of
    // claimable_atoms via the adapter's drift gate).
    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];
    let actual_atoms: u64 = if total_withdraw_shares > 0 {
        let mut withdraw_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            lender_marginfi_account.info.clone(),
            market_signer.clone(),
            debt_bank.info.clone(),
            market_debt_vault.info.clone(),
            debt_bank_lva.clone(),
            debt_liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
            // Active-balance health-check tuple `(debt_bank, …debt_oracles)`.
            debt_bank.info.clone(),
        ];
        for oracle_ai in &debt_oracle_ais.ais {
            withdraw_accounts.push((*oracle_ai).clone());
        }
        MarginfiV18Adapter.withdraw(
            &withdraw_accounts,
            total_withdraw_shares,
            &[market_signer_seeds],
        )?
    } else {
        0
    };
    // Split `actual_atoms` between curator and lender. Curator gets
    // first dibs at exactly `curator_fee_atoms` (the requested amount);
    // any sub-atom drift between requested and returned absorbs into
    // the lender's portion.
    let curator_realised: u64 = curator_fee_atoms.min(actual_atoms);
    let lender_realised: u64 = actual_atoms.saturating_sub(curator_realised);

    if actual_atoms > 0 {
        // SPL transfer market_debt_vault → global_vault_staging,
        // signed by market_signer.
        if token_program.info.key == &spl_token_2022::id() {
            let ix = spl_token_2022::instruction::transfer_checked(
                token_program.info.key,
                market_debt_vault.info.key,
                debt_mint.info.key,
                global_vault_staging.info.key,
                market_signer.key,
                &[],
                actual_atoms,
                debt_mint.mint.decimals,
            )?;
            invoke_signed(
                &ix,
                &[
                    market_debt_vault.info.clone(),
                    debt_mint.info.clone(),
                    global_vault_staging.info.clone(),
                    market_signer.clone(),
                    token_program.info.clone(),
                ],
                &[market_signer_seeds],
            )?;
        } else {
            let ix = spl_token::instruction::transfer(
                token_program.info.key,
                market_debt_vault.info.key,
                global_vault_staging.info.key,
                market_signer.key,
                &[],
                actual_atoms,
            )?;
            invoke_signed(
                &ix,
                &[
                    market_debt_vault.info.clone(),
                    global_vault_staging.info.clone(),
                    market_signer.clone(),
                    token_program.info.clone(),
                ],
                &[market_signer_seeds],
            )?;
        }

        // marginfi.deposit global_vault_staging → global_vault.integration,
        // signed by global_vault_signer.
        let global_vault_bytes = global_vault_key.to_bytes();
        let global_vault_signer_bump_arr = [global_vault_signer_bump];
        let global_vault_signer_seeds: &[&[u8]] = &[
            GLOBAL_VAULT_SIGNER_SEED,
            &global_vault_bytes,
            &global_vault_signer_bump_arr,
        ];
        let deposit_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            global_vault_integration_account.info.clone(),
            global_vault_signer.clone(),
            debt_bank.info.clone(),
            global_vault_staging.info.clone(),
            debt_liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];
        let _credited: u128 = MarginfiV18Adapter.deposit(
            &deposit_accounts,
            actual_atoms,
            &[global_vault_signer_seeds],
        )?;

        // Risk-profile state: accrue, then bump idle_principal_atoms by
        // the realised atoms now back inside the GlobalVault.
        {
            let data: &mut RefMut<&mut [u8]> = &mut global_vault.info.try_borrow_mut_data()?;
            let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
            let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
            let probe = RiskProfile::new_empty(lender_profile_id, Pubkey::default(), 1, 1, 0);
            let profile_idx = {
                let tree =
                    RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
                tree.lookup_index(&probe)
            };
            require!(
                profile_idx != NIL,
                YdeltaError::VaultProfileNotFound,
                "claim_repayment_for_risk_profile: profile_id {} not found on global_vault",
                lender_profile_id
            )?;
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            let share_value_fp48 = read_bank_asset_share_value_fp48(debt_bank.info);
            // Crystallise yield up to claim time at the OLD weighted_rate
            // (the closed loan was over-contributing during the close-to-
            // claim window; the cranker incentive bounds that window).
            accrue_risk_profile(profile, now_unix_ts, share_value_fp48)?;
            // Stop yield contribution from the now-closed loan.
            let weighted_delta: u128 =
                (principal_debt_atoms as u128).saturating_mul(lender_rate_bps as u128);
            profile.total_weighted_rate_bps = profile
                .total_weighted_rate_bps
                .saturating_sub(weighted_delta);
            profile.deployed_principal_atoms = profile
                .deployed_principal_atoms
                .saturating_sub(principal_debt_atoms);
            // Realised atoms: principal portion was already counted in
            // `total_principal_atoms` (just changed location); interest
            // portion is new capital for the LPs. We use `lender_realised`
            // here, NOT `actual_atoms` — `actual_atoms` includes the
            // curator's slice, and that slice belongs to the curator's
            // separate accumulator below, not to LP yield. Bad debt
            // (`lender_realised < principal`) shrinks the pool by the
            // shortfall.
            if lender_realised >= principal_debt_atoms {
                let interest = lender_realised - principal_debt_atoms;
                profile.total_principal_atoms = profile
                    .total_principal_atoms
                    .checked_add(interest)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            } else {
                let shortfall = principal_debt_atoms - lender_realised;
                profile.total_principal_atoms =
                    profile.total_principal_atoms.saturating_sub(shortfall);
            }
            // Sweep curator's manager-fee atoms onto the per-profile
            // accumulator. The atoms themselves were just deposited
            // into vault.integration alongside the lender atoms by the
            // CPI chain above, so `claim_curator_fee` can withdraw
            // exactly this many atoms when called.
            if curator_realised > 0 {
                profile.accumulated_curator_fee_atoms = profile
                    .accumulated_curator_fee_atoms
                    .checked_add(curator_realised)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
            }
        }
    }

    // Close the loan PDA: zero its data and refund cranker rent if the
    // optional cranker_refund matches loan.created_by.
    let created_by: Pubkey = {
        let data = loan.info.try_borrow_data()?;
        let header: &LoanFixed = bytemuck::from_bytes(&data[..LOAN_FIXED_SIZE]);
        header.created_by
    };
    {
        let mut data: RefMut<&mut [u8]> = loan.info.try_borrow_mut_data()?;
        for byte in data.iter_mut() {
            *byte = 0;
        }
    }
    if let Some(refund_ai) = cranker_refund {
        if *refund_ai.key == created_by {
            let lamports = loan.info.lamports();
            **loan.info.try_borrow_mut_lamports()? = 0;
            **refund_ai.try_borrow_mut_lamports()? = refund_ai
                .lamports()
                .checked_add(lamports)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    emit_stack(RepaymentClaimedForRiskProfileLog {
        market: market_key,
        loan: *loan.info.key,
        global_vault: global_vault_key,
        risk_profile_id: lender_profile_id,
        _pad0: [0; 7],
        claimed_atoms: actual_atoms,
        principal_atoms: principal_debt_atoms,
        _pad1: [0; 8],
        protocol_fee_shares_swept: protocol_fee_shares,
    })?;

    Ok(())
}
