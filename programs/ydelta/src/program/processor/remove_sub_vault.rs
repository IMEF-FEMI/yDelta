//! `RemoveSubVault` — vault-admin removal of a sub-vault from the global
//! vault. Only allowed when `is_sunset == 1`; admin must first run
//! `SunsetSubVault` and complete the wind-down (depositors withdrawn,
//! remaining orders cancelled) before calling this. Frees the
//! `SubVault` node from the vault's tree and emits a
//! `SubVaultRemovedLog`.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, SubVaultRemovedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_helper_sub_vault, remove_sub_vault, GlobalVaultFixed, SubVault,
    SubVaultTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::RemoveSubVaultContext;

/// Risk-profile removal parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct RemoveSubVaultParams {
    /// Sub-vault ID to remove (1-based; 0 is the sentinel).
    pub sub_vault_id: u8,
}

/// Remove a sunset sub-vault from the global vault. Errors with
/// `SubVaultNotSunset` if the profile is still active.
pub fn process_remove_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = RemoveSubVaultParams::try_from_slice(data)?;
    let RemoveSubVaultContext { _payer: _, vault } = RemoveSubVaultContext::load(accounts)?;

    let vault_key = *vault.info.key;

    let curator: Pubkey = {
        let vault_data = vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
        let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::SubVaultNotFound,
            "remove_sub_vault: sub_vault_id {} not found in vault",
            params.sub_vault_id
        )?;
        let profile: &SubVault = get_helper_sub_vault(dynamic, idx).get_value();
        require!(
            profile.is_sunset != 0,
            YdeltaError::SubVaultNotSunset,
            "remove_sub_vault: sub_vault_id {} is not sunset; call SunsetSubVault and \
             complete the wind-down (depositors withdraw, admin force-cancels remaining orders) \
             before removing",
            params.sub_vault_id
        )?;
        profile.curator
    };

    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let freed = remove_sub_vault(header, dynamic, params.sub_vault_id)?;
        require!(
            freed != NIL,
            YdeltaError::SubVaultNotFound,
            "remove_sub_vault: sub_vault_id {} disappeared between gate and removal",
            params.sub_vault_id
        )?;
    }

    emit_stack(SubVaultRemovedLog {
        global_vault: vault_key,
        curator,
        sub_vault_id: params.sub_vault_id,
        _pad0: [0; 7],
    })?;

    Ok(())
}
