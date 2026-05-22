//! Burn vault-profile shares and redeem atoms.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke_signed,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use crate::logs::{emit_stack, GlobalVaultWithdrawLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::user_account::{
    get_mut_helper_vault_position, remove_vault_position, UserAccountFixed, VaultPosition,
    VaultPositionTreeReadOnly,
};
use crate::state::vault::{
    accrue_risk_profile, get_mut_helper_risk_profile, get_mut_helper_risk_profile_depositor_seat,
    remove_risk_profile_depositor_seat, GlobalVaultFixed, RiskProfile, RiskProfileDepositorSeat,
    RiskProfileDepositorSeatTreeReadOnly, RiskProfileTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, USER_ACCOUNT_FIXED_SIZE};
use crate::validation::loaders::GlobalVaultWithdrawContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct GlobalVaultWithdrawParams {
    pub shares_to_burn: u128,
    pub profile_id: u8,
}

pub fn process_global_vault_withdraw(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = GlobalVaultWithdrawParams::try_from_slice(data)?;
    require!(
        params.shares_to_burn > 0,
        YdeltaError::InvalidArgument,
        "global_vault_withdraw: shares_to_burn must be > 0"
    )?;

    let GlobalVaultWithdrawContext {
        payer,
        vault,
        mint,
        global_vault_signer,
        global_vault_signer_bump,
        global_vault_staging,
        depositor_token,
        token_program,
        marginfi_group,
        integration_account,
        lending_pool,
        lending_pool_oracle,
        liquidity_vault,
        bank_liquidity_vault_authority,
        marginfi_program,
        user_account_ai,
    } = GlobalVaultWithdrawContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let now: i64 = Clock::get()?.unix_timestamp;

    // Validate the depositor has enough shares. The vault-side seat is primary.
    // Cross-check the user-account mirror as a defensive read so a
    // diverged mirror surfaces with `InvalidArgument` instead of
    // silent under-burn. Returns the user-account seat index for the
    // mirror update later.
    let position_idx = {
        // Authoritative read from the vault-side depositor seat.
        let v_data: &Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (v_fixed_bytes, v_dynamic) = v_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let v_header: &GlobalVaultFixed = bytemuck::from_bytes(v_fixed_bytes);
        let probe = RiskProfileDepositorSeat::probe(*payer.info.key, params.profile_id);
        let seat_idx = {
            let tree = RiskProfileDepositorSeatTreeReadOnly::new(
                v_dynamic,
                v_header.claimed_seats_root_index,
                hypertree::NIL,
            );
            tree.lookup_index(&probe)
        };
        require!(
            seat_idx != hypertree::NIL,
            YdeltaError::InvalidArgument,
            "depositor has no RiskProfileDepositorSeat for profile_id {}",
            params.profile_id
        )?;
        let seat = crate::state::vault::get_helper_risk_profile_depositor_seat(v_dynamic, seat_idx)
            .get_value();
        require!(
            seat.shares >= params.shares_to_burn,
            YdeltaError::InvalidArgument,
            "shares_to_burn ({}) exceeds depositor's holdings ({})",
            params.shares_to_burn,
            { seat.shares }
        )?;

        // Mirror lookup on UserAccountFixed (best-effort — burn even
        // if the mirror is missing or stale).
        let data: &Ref<&mut [u8]> = &user_account_ai.try_borrow_data()?;
        let (fixed_bytes, dynamic) = data.split_at(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &UserAccountFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = VaultPosition::new_empty(vault_key, params.profile_id);
        let tree = VaultPositionTreeReadOnly::new(
            dynamic,
            fixed.vault_positions_root_index,
            hypertree::NIL,
        );
        tree.lookup_index(&probe)
    };

    // Accrue and compute atoms_out.
    let (atoms_out, new_total_shares, mut profile_total_assets_after, _) = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found in vault",
            params.profile_id
        )?;

        // Read the vault-wide marginfi liquidity ONCE up front (the
        // integration account is shared by every profile). Used by the
        // physical-sufficiency check below.
        let mfi_asset_shares: u128 = crate::protocol::marginfi::read_asset_shares_u128(
            integration_account.info,
            lending_pool.info.key,
        )?;
        let mfi_atoms: u64 =
            MarginfiV18Adapter.shares_to_amount(&[lending_pool.info.clone()], mfi_asset_shares)?;

        let profile_node = get_mut_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_mut_value();

        let share_value_fp48 =
            crate::state::vault::read_bank_asset_share_value_fp48(lending_pool.info);
        accrue_risk_profile(profile, now, share_value_fp48)?;

        // ─── Realized-only redemption ───
        //
        // A withdrawer redeems their proportional share of
        // `total_principal_atoms` — deposits plus REALIZED gains (physical
        // supply yield, already in principal; loan interest crystallised
        // into principal at claim/settle). The accrued-but-unpaid
        // loan-rate ESTIMATE lives only in `total_assets_atoms` and is NOT
        // redeemable here.
        //
        // Paying the asset-side NAV instead would (a) hand the exiter
        // other LPs' idle principal as not-yet-earned interest and let
        // them escape the loan's default/tail risk, and (b) drift the
        // principal basis off physical marginfi backing, because
        // `total_principal` would fall by LESS than the atoms removed —
        // weakening the per-profile idle isolation over time. So the
        // payout, the `total_assets` debit, and the `total_principal`
        // debit are all the realized `principal_decrement`. The estimate
        // stays in `total_assets_atoms` and is realized to whoever REMAINS
        // when the loan settles (claim_repayment trues `total_assets` up
        // against that same estimate, so nothing is double-counted).
        let principal_decrement: u64 = (params
            .shares_to_burn
            .checked_mul(profile.total_principal_atoms as u128)
            .and_then(|x| x.checked_div(profile.total_shares))
            .ok_or(ProgramError::ArithmeticOverflow)?)
            as u64;
        // Allow a zero-atom burn ONLY when the profile is fully impaired
        // (assets wiped to 0 by bad debt while shares remain). This lets
        // shareholders clear their dead shares and reclaim their seat instead
        // of the position being permanently locked. A 0 from dust shares on a
        // still-funded profile (total_assets > 0) is still rejected.
        require!(
            principal_decrement > 0 || profile.total_assets_atoms == 0,
            YdeltaError::InvalidArgument,
            "computed payout is 0 — shares too small to redeem any atoms"
        )?;

        // On a full exit (burning every share) the bookkeeping basis can
        // marginally exceed the live marginfi balance due to the adapter's
        // share-rounding on deposit/accrual. Cap the payout at what the
        // integration account actually holds — the final-burn
        // reconciliation below zeroes the per-profile totals anyway, so the
        // rounding dust is not stranded as an unredeemable last share.
        let burns_last_share: bool = profile.total_shares == params.shares_to_burn;
        let atoms_out = if burns_last_share {
            principal_decrement.min(mfi_atoms)
        } else {
            principal_decrement
        };

        // ─── Per-profile liquidity gate ───
        //
        // The marginfi integration account is shared by EVERY profile
        // in the vault. Gating only against the vault-wide marginfi
        // balance would let a depositor in one profile drain atoms that
        // economically back a DIFFERENT profile. So gate per-profile
        // against the withdrawing profile's OWN idle asset base:
        //
        //   profile_idle = total_principal − deployed − encumbered_in_orders
        //
        // `total_principal_atoms` carries the profile's current
        // withdrawable asset basis: deposits plus realised / physically
        // accrued gains already sitting in marginfi. `deployed_principal_atoms`
        // has already left marginfi to the borrower side;
        // `encumbered_in_orders_atoms` is reserved for a resting order.
        // Only the idle remainder is the profile's to redeem. We also
        // keep a vault-wide physical check so the marginfi balance
        // actually suffices to pay out.
        let profile_idle: u64 = profile
            .total_principal_atoms
            .saturating_sub(profile.deployed_principal_atoms)
            .saturating_sub(profile.encumbered_in_orders_atoms);
        require!(
            profile_idle >= atoms_out,
            YdeltaError::VaultInsufficientIdleAtoms,
            "profile idle ({} = total_principal {} − deployed {} − encumbered {}) \
             < atoms_out ({}) — wait for loans to close or orders to cancel",
            profile_idle,
            { profile.total_principal_atoms },
            { profile.deployed_principal_atoms },
            { profile.encumbered_in_orders_atoms },
            atoms_out
        )?;
        // Vault-wide physical-sufficiency check: the shared marginfi
        // integration account must actually hold enough atoms to settle
        // this withdrawal.
        require!(
            mfi_atoms >= atoms_out,
            YdeltaError::VaultInsufficientIdleAtoms,
            "vault marginfi liquidity ({} atoms) < atoms_out ({})",
            mfi_atoms,
            atoms_out
        )?;

        // ─── Forbid burning the last share with capital in flight ───
        //
        // The final-burn reconciliation below zeroes `total_principal_atoms`
        // /`total_assets_atoms` once `total_shares` hits 0. If a loan is
        // still deployed (or atoms are encumbered in a resting order) at
        // that moment, those real atoms would be orphaned: the invariant
        // `total_principal ≥ deployed + encumbered` breaks and the stranded
        // capital can never be redeemed. Reject the last-share burn until
        // every loan has closed and every order has been cancelled.
        if burns_last_share {
            require!(
                profile.deployed_principal_atoms == 0 && profile.encumbered_in_orders_atoms == 0,
                YdeltaError::VaultInsufficientIdleAtoms,
                "cannot burn the last vault share while capital is in flight \
                 (deployed {} + encumbered {} > 0) — wait for loans to close \
                 and orders to cancel",
                { profile.deployed_principal_atoms },
                { profile.encumbered_in_orders_atoms }
            )?;
        }

        profile.total_shares = profile
            .total_shares
            .checked_sub(params.shares_to_burn)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // Realized-only: both basis and NAV fall by exactly the atoms
        // paid out, so the principal basis tracks physical marginfi
        // backing 1:1 (no drift) and the unrealized loan-rate estimate
        // retained in `total_assets_atoms` is left for the remaining
        // shareholders. `atoms_out == principal_decrement` except on the
        // last-share cap, where the final-burn reconciliation zeroes both.
        profile.total_assets_atoms = profile
            .total_assets_atoms
            .checked_sub(atoms_out)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_principal_atoms = profile
            .total_principal_atoms
            .checked_sub(atoms_out)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        (
            atoms_out,
            profile.total_shares,
            profile.total_assets_atoms,
            atoms_out,
        )
    };

    let vault_bytes = vault_key.to_bytes();
    let signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] =
        &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &signer_bump_arr];

    // Withdraw from the integration account into global_vault_staging.
    // atoms_out == 0 only on a fully-impaired share burn (above) — nothing to
    // move, so skip the marginfi CPI and just burn the dead shares below.
    let (payout_atoms, surplus_atoms) = if atoms_out > 0 {
        let expected_shares: u128 =
            MarginfiV18Adapter.amount_to_asset_shares(&[lending_pool.info.clone()], atoms_out)?;
        let adapter_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            integration_account.info.clone(),
            global_vault_signer.clone(),
            lending_pool.info.clone(),
            global_vault_staging.info.clone(),
            bank_liquidity_vault_authority.clone(),
            liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
            // Active-balance health-check pair (vault has only one).
            lending_pool.info.clone(),
            lending_pool_oracle.clone(),
        ];
        let actual_atoms: u64 = MarginfiV18Adapter.withdraw(
            &adapter_accounts,
            expected_shares,
            &[global_vault_signer_seeds],
        )?;
        let payout = actual_atoms.min(atoms_out);
        (payout, actual_atoms.saturating_sub(payout))
    } else {
        (0, 0)
    };

    // We do NOT re-credit the shortfall (`atoms_out - actual_atoms`)
    // back to `total_assets_atoms` / `total_principal_atoms`. Marginfi
    // v0.1.8's `LendingAccountWithdraw` uses CEIL share-rounding while
    // our local `amount_to_asset_shares` floors, so a 1-atom under-
    // delivery means the shares were burned in marginfi without the
    // matching atom leaving — the dust is gone, not "left behind."
    // Booking it onto the per-profile totals creates phantom atoms that
    // exceed the live marginfi balance and break the subsequent
    // `mfi_atoms >= atoms_out` gate on the next withdrawal.
    //
    // Surplus (`actual_atoms > atoms_out`) IS redeposited below so it
    // does not leak cross-profile.
    if new_total_shares == 0 {
        // Final burn: the last-share-burn guard already ensured
        // `deployed == 0 && encumbered == 0`, so every atom the profile
        // still books is idle. Zero the per-profile `total_*` so any
        // residual drift is donation-like rather than becoming phantom
        // assets the next genesis depositor could mint against.
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        if profile_idx != NIL {
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            profile.total_assets_atoms = 0;
            profile.total_principal_atoms = 0;
            profile_total_assets_after = profile.total_assets_atoms;
        }
    }

    // Transfer from global_vault_staging to depositor_token, signed by
    // global_vault_signer. Skipped on a fully-impaired (0-atom) burn.
    if payout_atoms > 0 {
        transfer_staging_to_depositor(
            token_program.info,
            global_vault_staging.info,
            depositor_token.info,
            mint.info,
            global_vault_signer,
            &vault_key,
            global_vault_signer_bump,
            payout_atoms,
            mint.mint.decimals,
        )?;
    }

    if surplus_atoms > 0 {
        let deposit_accounts: Vec<AccountInfo> = vec![
            marginfi_group.info.clone(),
            integration_account.info.clone(),
            global_vault_signer.clone(),
            lending_pool.info.clone(),
            global_vault_staging.info.clone(),
            liquidity_vault.info.clone(),
            token_program.info.clone(),
            marginfi_program.info.clone(),
        ];
        let _credited_shares: u128 = MarginfiV18Adapter.deposit(
            &deposit_accounts,
            surplus_atoms,
            &[global_vault_signer_seeds],
        )?;
    }

    // Burn shares on the authoritative vault-side seat.
    let seat_zeroed: bool = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = RiskProfileDepositorSeat::probe(*payer.info.key, params.profile_id);
        let seat_idx = {
            let tree = RiskProfileDepositorSeatTreeReadOnly::new(
                dynamic,
                header.claimed_seats_root_index,
                hypertree::NIL,
            );
            tree.lookup_index(&probe)
        };
        // The earlier validation already proved the seat exists and the seat tree
        // isn't mutated between steps. A NIL here would mean the SPL
        // transfer has already paid out atoms but no share
        // burn happened — exactly the double-spend shape. Hard error.
        require!(
            seat_idx != hypertree::NIL,
            YdeltaError::InvalidArgument,
            "global_vault_withdraw: depositor seat vanished mid-ix (impossible single-threaded)"
        )?;
        let seat = get_mut_helper_risk_profile_depositor_seat(dynamic, seat_idx).get_mut_value();
        seat.shares = seat
            .shares
            .checked_sub(params.shares_to_burn)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        seat.last_updated_unix = now;
        let zeroed = seat.shares == 0;
        if zeroed {
            let _ = remove_risk_profile_depositor_seat(
                header,
                dynamic,
                *payer.info.key,
                params.profile_id,
            );
        }
        zeroed
    };

    // Mirror the burn onto UserAccountFixed.VaultPosition.
    if position_idx != hypertree::NIL {
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let pos = get_mut_helper_vault_position(dynamic, position_idx).get_mut_value();
        pos.shares = pos
            .shares
            .checked_sub(params.shares_to_burn)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        pos.last_updated_unix = now;
        if seat_zeroed {
            remove_vault_position(fixed, dynamic, vault_key, params.profile_id)?;
        }
    }

    emit_stack(GlobalVaultWithdrawLog {
        global_vault: vault_key,
        depositor: *payer.info.key,
        shares_burned: params.shares_to_burn,
        profile_total_shares: new_total_shares,
        atoms_out: payout_atoms,
        profile_total_assets_atoms: profile_total_assets_after,
        profile_id: params.profile_id,
        _padding: [0; 15],
    })?;

    Ok(())
}

/// Signed SPL transfer: global_vault_staging → depositor, signed by global_vault_signer.
#[allow(clippy::too_many_arguments)]
fn transfer_staging_to_depositor<'info>(
    token_program: &AccountInfo<'info>,
    global_vault_staging: &AccountInfo<'info>,
    depositor_token: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    global_vault_signer: &AccountInfo<'info>,
    vault_key: &Pubkey,
    global_vault_signer_bump: u8,
    amount: u64,
    decimals: u8,
) -> ProgramResult {
    let vault_bytes = vault_key.to_bytes();
    let bump_arr = [global_vault_signer_bump];
    let signer_seeds: &[&[u8]] = &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &bump_arr];
    if token_program.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            global_vault_staging.key,
            mint.key,
            depositor_token.key,
            global_vault_signer.key,
            &[],
            amount,
            decimals,
        )?;
        invoke_signed(
            &ix,
            &[
                global_vault_staging.clone(),
                mint.clone(),
                depositor_token.clone(),
                global_vault_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.key,
            global_vault_staging.key,
            depositor_token.key,
            global_vault_signer.key,
            &[],
            amount,
        )?;
        invoke_signed(
            &ix,
            &[
                global_vault_staging.clone(),
                depositor_token.clone(),
                global_vault_signer.clone(),
                token_program.clone(),
            ],
            &[signer_seeds],
        )
    }
}

use std::cell::Ref;
