//! Deposit atoms into a vault profile and mint shares.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
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
    accrue_risk_profile, get_mut_helper_risk_profile, get_mut_helper_risk_profile_depositor_seat,
    upsert_risk_profile_depositor_seat, vault_expand_node_block, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly, GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, USER_ACCOUNT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE};
use crate::validation::loaders::GlobalVaultDepositContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct GlobalVaultDepositParams {
    pub amount_atoms: u64,
    pub profile_id: u8,
}

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

    // Transfer depositor_token to global_vault_staging.
    super::deposit::transfer_user_to_vault(
        token_program.info,
        depositor_token.info,
        global_vault_staging.info,
        mint.info,
        payer.info,
        params.amount_atoms,
        mint.mint.decimals,
    )?;

    // Deposit from global_vault_staging into the liquidity vault,
    // signed by global_vault_signer.
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
    let _credited_shares: u128 = MarginfiV18Adapter.deposit(
        &adapter_accounts,
        params.amount_atoms,
        &[global_vault_signer_seeds],
    )?;

    // Expand the vault by one node block if needed.
    // The depositor seat upsert below needs a free 128-byte vault
    // node block. Realloc + extend free list before the mutating
    // borrow.
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

    // Accrue the profile, mint shares, and write the depositor seat.
    // Vault state is the authoritative store of the user's stake; the
    // UserAccountFixed mirror update below is bookkeeping only.
    let (shares_minted, total_shares_after, total_assets_after) = {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        // Locate profile node.
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

        // Accrue + share-mint math under a profile-only borrow.
        let (shares, total_shares_after, total_assets_after, snapshot_supply, snapshot_delta) = {
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            let share_value_fp48 =
                crate::state::vault::read_bank_asset_share_value_fp48(lending_pool.info);
            accrue_risk_profile(profile, now, share_value_fp48)?;

            let atoms_u128 = params.amount_atoms as u128;
            let shares: u128 = if profile.total_shares == 0 {
                // Genesis (or post-drain) state. Reset accumulated assets
                // to neutralize any donations / phantom yield credited
                // while no shares existed — otherwise the first depositor
                // would mint 1:1 against an inflated asset base and
                // capture the donation.
                profile.total_assets_atoms = 0;
                atoms_u128
            } else {
                atoms_u128
                    .checked_mul(profile.total_shares)
                    .and_then(|x| x.checked_div(profile.total_assets_atoms as u128))
                    .ok_or(ProgramError::ArithmeticOverflow)?
            };
            // Reject zero-share mints: dust deposits at high share-prices
            // would credit total_principal/total_assets while issuing 0
            // shares to the depositor — pure grief vector.
            require!(
                shares > 0,
                YdeltaError::InvalidArgument,
                "global_vault_deposit: amount {} too small to mint any shares",
                params.amount_atoms
            )?;

            profile.total_shares = profile
                .total_shares
                .checked_add(shares)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_principal_atoms = profile
                .total_principal_atoms
                .checked_add(params.amount_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            profile.total_assets_atoms = profile
                .total_assets_atoms
                .checked_add(params.amount_atoms)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            (
                shares,
                profile.total_shares,
                profile.total_assets_atoms,
                profile.cumulative_supply_yield_index_scaled,
                profile.cumulative_delta_yield_index_scaled,
            )
        };

        // Authoritative depositor-seat write. Upsert under owner key.
        let seat_idx = upsert_risk_profile_depositor_seat(
            header,
            dynamic,
            *payer.info.key,
            params.profile_id,
        )?;
        {
            let seat =
                get_mut_helper_risk_profile_depositor_seat(dynamic, seat_idx).get_mut_value();
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

    // Mirror onto UserAccountFixed.VaultPosition.
    // Secondary projection of the vault-side seat for cheap
    // user-account-side reads. Diverges from the vault-side seat only
    // transiently across atomic ix boundaries.
    {
        let data: &mut RefMut<&mut [u8]> = &mut user_account_ai.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(USER_ACCOUNT_FIXED_SIZE);
        let fixed: &mut UserAccountFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let pos_idx = upsert_vault_position(fixed, dynamic, vault_key, params.profile_id)?;
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
        atoms_in: params.amount_atoms,
        gain_atoms: 0,
        profile_total_assets_atoms: total_assets_after,
        profile_id: params.profile_id,
        _padding: [0; 7],
    })?;

    Ok(())
}
