//! `GlobalVaultDeposit` instruction. Lender deposits atoms into a risk
//! profile, minting profile shares pro-rata against
//! `profile.total_assets_atoms`. Atoms route through the vault staging
//! account into marginfi. Rejects when `profile.is_sunset != 0`.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, GlobalVaultDepositLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::user_account::{
    get_mut_helper_vault_position, upsert_vault_position, UserAccountFixed,
};
use crate::state::vault::{
    accrue_sub_vault, get_mut_helper_sub_vault, get_mut_helper_sub_vault_depositor_seat,
    upsert_sub_vault_depositor_seat, vault_expand_node_block, GlobalVaultFixed, SubVault,
    SubVaultTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, USER_ACCOUNT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE};
use crate::validation::loaders::GlobalVaultDepositContext;

/// Parameters for [`process_global_vault_deposit`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct GlobalVaultDepositParams {
    /// Atoms of the vault's mint to deposit. Must be `> 0`.
    pub amount_atoms: u64,
    /// Identifies the destination sub-vault.
    pub sub_vault_id: u8,
}

/// Deposit atoms into a sub-vault and mint pro-rata shares. Accrues
/// the profile, computes shares against `total_assets_atoms`, updates
/// the depositor seat + `UserAccountFixed` vault position, and emits
/// `GlobalVaultDepositLog`. Errors with `SubVaultSunset` when sunset.
pub fn process_global_vault_deposit(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = GlobalVaultDepositParams::try_from_slice(data)?;
    require!(
        params.amount_atoms > 0,
        YdeltaError::InvalidArgument,
        "global_vault_deposit: amount must be > 0"
    )?;

    let GlobalVaultDepositContext {
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
        liquidity_vault,
        marginfi_program,
        user_account_ai,
    } = GlobalVaultDepositContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let now: i64 = Clock::get()?.unix_timestamp;

    let staging_before_atoms = global_vault_staging.get_balance_atoms()?;
    super::deposit::transfer_user_to_vault(
        token_program.info,
        depositor_token.info,
        global_vault_staging.info,
        mint.info,
        payer.info,
        params.amount_atoms,
        mint.mint.decimals,
    )?;
    let received_atoms = global_vault_staging
        .get_balance_atoms()?
        .checked_sub(staging_before_atoms)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    require!(
        received_atoms > 0,
        YdeltaError::InvalidArgument,
        "global_vault_deposit: staging vault received 0 atoms"
    )?;

    let vault_bytes = vault_key.to_bytes();
    let signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] =
        &[GLOBAL_VAULT_SIGNER_SEED, &vault_bytes, &signer_bump_arr];
    let adapter_accounts = [
        marginfi_group.info.clone(),
        integration_account.info.clone(),
        global_vault_signer.clone(),
        lending_pool.info.clone(),
        global_vault_staging.info.clone(),
        liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    let credited_shares: u128 = MarginfiV18Adapter.deposit(
        &adapter_accounts,
        received_atoms,
        &[global_vault_signer_seeds],
    )?;

    let credited_atoms: u64 =
        MarginfiV18Adapter.shares_to_amount(&[lending_pool.info.clone()], credited_shares)?;
    require!(
        credited_atoms > 0,
        YdeltaError::InvalidArgument,
        "deposit {} too small — marginfi acknowledged 0 atoms after share rounding",
        received_atoms
    )?;

    let need_expand: bool = {
        let vault_data = vault.info.try_borrow_data()?;
        let header: &GlobalVaultFixed =
            bytemuck::from_bytes(&vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
        !header.has_free_node_block()
    };
    if need_expand {
        let new_size = vault.info.data_len() + VAULT_NODE_BLOCK_SIZE;
        let rent: Rent = Rent::get()?;
        let new_min = rent.minimum_balance(new_size);
        let old_min = rent.minimum_balance(vault.info.data_len());
        let lamports_diff = new_min.saturating_sub(old_min);
        if lamports_diff > 0 {
            invoke(
                &system_instruction::transfer(payer.info.key, vault.info.key, lamports_diff),
                &[payer.info.clone(), vault.info.clone()],
            )?;
        }
        #[allow(deprecated)]
        vault.info.realloc(new_size, false)?;
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        vault_expand_node_block(header, dynamic)?;
    }

    let (shares_minted, total_shares_after, total_assets_after) = {
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

        let (shares, total_shares_after, total_assets_after, snapshot_supply, snapshot_delta) = {
            let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
            require!(
                profile.is_sunset == 0,
                YdeltaError::SubVaultSunset,
                "global_vault_deposit: sub_vault_id {} is sunset; new deposits are rejected \
                 during wind-down (existing depositors may still withdraw)",
                params.sub_vault_id
            )?;
            let share_value_fp48 =
                crate::state::vault::read_bank_asset_share_value_fp48(lending_pool.info)?;
            accrue_sub_vault(profile, now, share_value_fp48)?;

            let atoms_u128 = credited_atoms as u128;
            let shares: u128 = if profile.total_shares == 0 {
                profile.total_assets_atoms = 0;
                atoms_u128
            } else {
                require!(
                    profile.total_assets_atoms != 0,
                    YdeltaError::InvalidArgument,
                    "profile fully impaired (0 assets, {} shares) — deposits disabled \
                     until existing shares are burned",
                    { profile.total_shares }
                )?;
                crate::math::mul_div(
                    atoms_u128,
                    profile.total_shares,
                    profile.total_assets_atoms as u128,
                    false,
                )?
            };

            require!(
                shares > 0,
                YdeltaError::InvalidArgument,
                "global_vault_deposit: amount {} too small to mint any shares",
                received_atoms
            )?;

            profile.total_shares = profile
                .total_shares
                .checked_add(shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_principal_atoms = profile
                .total_principal_atoms
                .checked_add(credited_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_assets_atoms = profile
                .total_assets_atoms
                .checked_add(credited_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            (
                shares,
                profile.total_shares,
                profile.total_assets_atoms,
                profile.cumulative_supply_yield_index_scaled,
                profile.cumulative_delta_yield_index_scaled,
            )
        };

        let seat_idx = upsert_sub_vault_depositor_seat(
            header,
            dynamic,
            *payer.info.key,
            params.sub_vault_id,
        )?;
        {
            let seat =
                get_mut_helper_sub_vault_depositor_seat(dynamic, seat_idx).get_mut_value();
            seat.shares = seat
                .shares
                .checked_add(shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            seat.snapshot_supply_yield_index_scaled = snapshot_supply;
            seat.snapshot_delta_yield_index_scaled = snapshot_delta;
            seat.last_updated_unix = now;
        }
        (shares, total_shares_after, total_assets_after)
    };

    {
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let pos_idx = upsert_vault_position(fixed, dynamic, vault_key, params.sub_vault_id)?;
        let pos_node = get_mut_helper_vault_position(dynamic, pos_idx);
        let pos = pos_node.get_mut_value();
        pos.shares = pos
            .shares
            .checked_add(shares_minted)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        pos.last_updated_unix = now;
    }

    emit_stack(GlobalVaultDepositLog {
        global_vault: vault_key,
        depositor: *payer.info.key,
        shares_minted,
        profile_total_shares: total_shares_after,
        atoms_in: received_atoms,
        gain_atoms: 0,
        profile_total_assets_atoms: total_assets_after,
        sub_vault_id: params.sub_vault_id,
        _padding: [0; 7],
    })?;

    Ok(())
}
