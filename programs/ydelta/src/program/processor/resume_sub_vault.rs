//! `ResumeSubVault` — vault-admin un-sunset for a sub-vault. Signer is
//! the vault admin (via `ResumeSubVaultContext`). Clears `is_sunset`, which
//! re-enables deposits, new orders, order updates, and matching for the
//! profile.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_mut_helper_sub_vault, GlobalVaultFixed, SubVault, SubVaultTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ResumeSubVaultContext;

/// Resume parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ResumeSubVaultParams {
    /// Sub-vault ID to un-sunset (1-based; 0 is the sentinel).
    pub sub_vault_id: u16,
}

/// Clear the profile's `is_sunset` flag, re-enabling deposits, new orders,
/// updates, and matches.
pub fn process_resume_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ResumeSubVaultParams::try_from_slice(data)?;
    let ResumeSubVaultContext { _payer: _, vault } = ResumeSubVaultContext::load(accounts)?;

    let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

    let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
    let idx = {
        let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        idx != NIL,
        YdeltaError::SubVaultNotFound,
        "resume_sub_vault: sub_vault_id {} not found",
        params.sub_vault_id
    )?;

    let profile = get_mut_helper_sub_vault(dynamic, idx).get_mut_value();
    profile.is_sunset = 0;
    Ok(())
}
