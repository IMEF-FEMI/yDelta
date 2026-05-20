//! Set or clear the per-vault pause flag.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::GlobalVaultFixed;
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::SetVaultPauseContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SetVaultPauseParams {
    /// Non-zero → pause the vault; zero → unpause.
    pub paused: u8,
}

/// Toggle `GlobalVaultFixed.is_paused`. Vault-admin gated. While paused,
/// every vault-scoped state-mutating ix rejects at the loader with
/// `VaultPaused`; the two-step vault-admin transfer ixs stay live so a
/// stuck vault can still be recovered. Mirrors `set_market_pause`.
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
