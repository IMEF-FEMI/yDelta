//! `GlobalVaultWithdraw` instruction. Burn sub-vault shares to
//! redeem realized principal atoms from the vault. Gated by the
//! sub-vault's idle pool (`total_principal - deployed - encumbered`);
//! the last-share burn additionally requires zero in-flight capital.

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
    accrue_sub_vault, get_mut_helper_sub_vault, get_mut_helper_sub_vault_depositor_seat,
    remove_sub_vault_depositor_seat, GlobalVaultFixed, SubVault, SubVaultDepositorSeat,
    SubVaultDepositorSeatTreeReadOnly, SubVaultTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, USER_ACCOUNT_FIXED_SIZE};
use crate::validation::loaders::GlobalVaultWithdrawContext;

/// Parameters for [`process_global_vault_withdraw`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct GlobalVaultWithdrawParams {
    /// Sub-vault shares to redeem. Must be `> 0` and `<= seat.shares`.
    pub shares_to_burn: u128,
    /// Identifies the sub-vault to withdraw from.
    pub sub_vault_id: u16,
}

/// Burn `shares_to_burn` from the signer's depositor seat and
/// withdraw the realized principal share to the signer's wallet ATA.
/// Errors with `VaultInsufficientIdleAtoms` when the burn would drop
/// idle below the payout. Removes the seat / position when it zeroes.
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

    let position_idx = {
        let v_data: &Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (v_fixed_bytes, v_dynamic) = v_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let v_header: &GlobalVaultFixed = bytemuck::from_bytes(v_fixed_bytes);
        let probe = SubVaultDepositorSeat::probe(*payer.info.key, params.sub_vault_id);
        let seat_idx = {
            let tree = SubVaultDepositorSeatTreeReadOnly::new(
                v_dynamic,
                v_header.claimed_seats_root_index,
                hypertree::NIL,
            );
            tree.lookup_index(&probe)
        };
        require!(
            seat_idx != hypertree::NIL,
            YdeltaError::InvalidArgument,
            "depositor has no SubVaultDepositorSeat for sub_vault_id {}",
            params.sub_vault_id
        )?;
        let seat = crate::state::vault::get_helper_sub_vault_depositor_seat(v_dynamic, seat_idx)
            .get_value();
        require!(
            seat.shares >= params.shares_to_burn,
            YdeltaError::InvalidArgument,
            "shares_to_burn ({}) exceeds depositor's holdings ({})",
            params.shares_to_burn,
            { seat.shares }
        )?;

        let data: &Ref<&mut [u8]> = &user_account_ai.try_borrow_data()?;
        let (fixed_bytes, dynamic) = data.split_at(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &UserAccountFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = VaultPosition::new_empty(vault_key, params.sub_vault_id);
        let tree = VaultPositionTreeReadOnly::new(
            dynamic,
            fixed.vault_positions_root_index,
            hypertree::NIL,
        );
        tree.lookup_index(&probe)
    };

    let (atoms_out, new_total_shares, mut profile_total_assets_after, _) = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::SubVaultNotFound,
            "sub_vault_id {} not found in vault",
            params.sub_vault_id
        )?;

        let mfi_asset_shares: u128 = crate::protocol::marginfi::read_asset_shares_u128(
            integration_account.info,
            lending_pool.info.key,
        )?;
        let mfi_atoms: u64 =
            MarginfiV18Adapter.shares_to_amount(&[lending_pool.info.clone()], mfi_asset_shares)?;

        let profile_node = get_mut_helper_sub_vault(dynamic, profile_idx);
        let profile = profile_node.get_mut_value();
        // Private sub-vaults pay out only to their owner. (The
        // depositor-seat key already scopes shares to the signer; this is
        // defense-in-depth so a stray Private seat can never exist.)
        require!(
            profile.kind != crate::state::vault::SUB_VAULT_KIND_PRIVATE
                || profile.curator == *payer.info.key,
            YdeltaError::VaultCuratorRequired,
            "global_vault_withdraw: sub_vault_id {} is Private; only its \
             owner may withdraw",
            params.sub_vault_id
        )?;

        let share_value_fp48 =
            crate::state::vault::read_bank_asset_share_value_fp48(lending_pool.info)?;
        accrue_sub_vault(profile, now, share_value_fp48)?;

        let principal_decrement: u64 = u64::try_from(crate::math::mul_div(
            params.shares_to_burn,
            profile.total_principal_atoms as u128,
            profile.total_shares,
            false,
        )?)
        .map_err(|_| crate::program::YdeltaError::MathOverflow)?;

        require!(
            principal_decrement > 0 || profile.total_assets_atoms == 0,
            YdeltaError::InvalidArgument,
            "computed payout is 0 — shares too small to redeem any atoms"
        )?;

        let burns_last_share: bool = profile.total_shares == params.shares_to_burn;
        let atoms_out = if burns_last_share {
            principal_decrement.min(mfi_atoms)
        } else {
            principal_decrement
        };

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

        require!(
            mfi_atoms >= atoms_out,
            YdeltaError::VaultInsufficientIdleAtoms,
            "vault marginfi liquidity ({} atoms) < atoms_out ({})",
            mfi_atoms,
            atoms_out
        )?;

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
            lending_pool.info.clone(),
            lending_pool_oracle.clone(),
        ];
        let (actual_atoms, _shares_burned) = MarginfiV18Adapter.withdraw(
            &adapter_accounts,
            expected_shares,
            &[global_vault_signer_seeds],
        )?;
        let payout = actual_atoms.min(atoms_out);
        (payout, actual_atoms.saturating_sub(payout))
    } else {
        (0, 0)
    };

    if new_total_shares == 0 {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
            tree.lookup_index(&probe)
        };
        if profile_idx != NIL {
            let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
            profile.total_assets_atoms = 0;
            profile.total_principal_atoms = 0;
            profile_total_assets_after = profile.total_assets_atoms;
        }
    }

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

    let seat_zeroed: bool = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = SubVaultDepositorSeat::probe(*payer.info.key, params.sub_vault_id);
        let seat_idx = {
            let tree = SubVaultDepositorSeatTreeReadOnly::new(
                dynamic,
                header.claimed_seats_root_index,
                hypertree::NIL,
            );
            tree.lookup_index(&probe)
        };

        require!(
            seat_idx != hypertree::NIL,
            YdeltaError::InvalidArgument,
            "global_vault_withdraw: depositor seat vanished mid-ix (impossible single-threaded)"
        )?;
        let seat = get_mut_helper_sub_vault_depositor_seat(dynamic, seat_idx).get_mut_value();
        seat.shares = seat
            .shares
            .checked_sub(params.shares_to_burn)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        seat.last_updated_unix = now;
        let zeroed = seat.shares == 0;
        if zeroed {
            remove_sub_vault_depositor_seat(
                header,
                dynamic,
                *payer.info.key,
                params.sub_vault_id,
            )?;
        }
        zeroed
    };

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
            remove_vault_position(fixed, dynamic, vault_key, params.sub_vault_id)?;
        }
    }

    emit_stack(GlobalVaultWithdrawLog {
        global_vault: vault_key,
        depositor: *payer.info.key,
        shares_burned: params.shares_to_burn,
        profile_total_shares: new_total_shares,
        atoms_out: payout_atoms,
        profile_total_assets_atoms: profile_total_assets_after,
        sub_vault_id: params.sub_vault_id,
        _padding: [0; 14],
    })?;

    Ok(())
}

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
