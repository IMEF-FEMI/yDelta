//! `SetVaultPause` — vault-admin toggle for `GlobalVaultFixed::is_paused`.
//! Signer must equal `GlobalVaultFixed.global_vault_admin`. Sets `is_paused`
//! to 1 if `paused != 0`, else 0; consulted as a gate by deposit/order
//! instructions on the global vault.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::GlobalVaultFixed;
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::SetVaultPauseContext;

/// Vault-pause parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SetVaultPauseParams {
    /// Non-zero pauses the vault, zero unpauses.
    pub paused: u8,
}

/// Toggle the global vault's `is_paused` flag. Signer must be the vault admin.
pub fn process_set_vault_pause(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SetVaultPauseParams::try_from_slice(data)?;
    let SetVaultPauseContext { payer, vault } = SetVaultPauseContext::load(accounts)?;
    let vault_data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let header: &mut GlobalVaultFixed =
        bytemuck::from_bytes_mut(&mut vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
    require!(
        header.global_vault_admin == *payer.info.key,
        YdeltaError::VaultAdminRequired,
        "set_vault_pause: signer != GlobalVaultFixed.global_vault_admin"
    )?;
    header.is_paused = if params.paused != 0 { 1 } else { 0 };
    Ok(())
}
