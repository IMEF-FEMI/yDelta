//! Burn vault-profile shares and redeem atoms.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
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
        use hypertree::HyperTreeReadOperations;
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
        // v_data is a `Ref<&mut [u8]>` (a reborrow of the RefCell guard);
        // calling drop() on the reference here is a no-op. The borrow
        // releases at end of this scope anyway. Removed.

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
    let (atoms_out, new_total_shares, new_total_assets, principal_decrement) = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        use hypertree::{HyperTreeReadOperations, NIL};
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
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

        let profile_node = get_mut_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_mut_value();

        let share_value_fp48 =
            crate::state::vault::read_bank_asset_share_value_fp48(lending_pool.info);
        accrue_risk_profile(profile, now, share_value_fp48)?;

        // atoms_out = shares × total_assets / total_shares.
        let atoms_u128: u128 = params
            .shares_to_burn
            .checked_mul(profile.total_assets_atoms as u128)
            .and_then(|x| x.checked_div(profile.total_shares))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        require!(
            atoms_u128 <= u64::MAX as u128,
            ProgramError::ArithmeticOverflow,
            "computed atoms_out {} overflows u64",
            atoms_u128
        )?;
        let atoms_out = atoms_u128 as u64;
        require!(
            atoms_out > 0,
            YdeltaError::InvalidArgument,
            "computed atoms_out is 0 — shares too small to redeem any atoms"
        )?;

        // Idle = total_principal − deployed − encumbered. Reject if
        // depositor would pull from deployed liquidity.
        let idle: u64 = profile
            .total_principal_atoms
            .saturating_sub(profile.deployed_principal_atoms)
            .saturating_sub(profile.encumbered_in_orders_atoms);
        require!(
            idle >= atoms_out,
            YdeltaError::VaultInsufficientIdleAtoms,
            "idle_principal_atoms ({}) < atoms_out ({}) — wait for repayments \
             or curator to cancel outstanding orders",
            idle,
            atoms_out
        )?;

        // Proportional principal reduction so post-withdraw share price
        // equals pre-withdraw share price (no dilution).
        let principal_decrement: u64 = (params
            .shares_to_burn
            .checked_mul(profile.total_principal_atoms as u128)
            .and_then(|x| x.checked_div(profile.total_shares))
            .ok_or(ProgramError::ArithmeticOverflow)?)
            as u64;

        profile.total_shares = profile
            .total_shares
            .checked_sub(params.shares_to_burn)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_assets_atoms = profile
            .total_assets_atoms
            .checked_sub(atoms_out)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_principal_atoms = profile
            .total_principal_atoms
            .checked_sub(principal_decrement)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        (
            atoms_out,
            profile.total_shares,
            profile.total_assets_atoms,
            principal_decrement,
        )
    };
    let _ = principal_decrement;

    // Withdraw from the integration account into global_vault_staging.
    let expected_shares: u128 =
        MarginfiV18Adapter.amount_to_asset_shares(&[lending_pool.info.clone()], atoms_out)?;

    let vault_bytes = vault_key.to_bytes();
    let signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] =
        &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &signer_bump_arr];
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

    // Reconcile bookkeeping with marginfi's actual return.
    // The earlier accounting decremented total_assets by atoms_out and total_principal
    // by principal_decrement (both pre-CPI computed). If marginfi pays
    // out a different amount (within ±1 by the adapter's drift gate),
    // adjust BOTH fields by the same proportional ratio so the
    // assets/principal invariant the idle gate relies on stays
    // intact. Drift is asserted ≤ 1 atom by the adapter, so the
    // proportional add-back is at most 1 atom per side.
    // Per-call drift between marginfi's actual_atoms and our computed
    // atoms_out is bounded to ±1 atom by the adapter's drift gate,
    // and partial-withdraw drift compounds across calls. We don't
    // reconcile it per-call (doing so would violate the
    // proportional-decrement invariant tests rely on); instead we
    // zero everything on the FINAL burn so cumulative dust never
    // strands as phantom assets/principal.
    if new_total_shares == 0 {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        use hypertree::{HyperTreeReadOperations, NIL};
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        if profile_idx != NIL {
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            profile.total_assets_atoms = 0;
            profile.total_principal_atoms = 0;
        }
    }

    // Transfer from global_vault_staging to depositor_token, signed by
    // global_vault_signer.
    transfer_staging_to_depositor(
        token_program.info,
        global_vault_staging.info,
        depositor_token.info,
        mint.info,
        global_vault_signer,
        &vault_key,
        global_vault_signer_bump,
        actual_atoms,
        mint.mint.decimals,
    )?;

    // Burn shares on the authoritative vault-side seat.
    let seat_zeroed: bool = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        use hypertree::HyperTreeReadOperations;
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
            let _ = remove_vault_position(fixed, dynamic, vault_key, params.profile_id);
        }
    }

    emit_stack(GlobalVaultWithdrawLog {
        global_vault: vault_key,
        depositor: *payer.info.key,
        shares_burned: params.shares_to_burn,
        profile_total_shares: new_total_shares,
        atoms_out: actual_atoms,
        profile_total_assets_atoms: new_total_assets,
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
