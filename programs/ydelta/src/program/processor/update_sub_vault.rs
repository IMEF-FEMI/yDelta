//! `UpdateSubVault` — curator update of a sub-vault's mutable
//! parameters. Signer/payer flow is loaded via `CreateSubVaultContext`.
//! Each `Option<>` field is an override; `None` leaves the field unchanged.
//! Rejected when the profile is sunset.

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
use crate::validation::loaders::CreateSubVaultContext;

/// Per-field overrides for a `SubVault`. `None` leaves the field
/// unchanged; `Some(v)` is validated and written.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateSubVaultParams {
    /// Sub-vault ID to update (1-based; 0 is the sentinel).
    pub sub_vault_id: u8,
    /// New max LTV in bps; must be in `(0, 10_000)`.
    pub new_max_ltv_bps: Option<u16>,
    /// New max term in seconds; must be `> 0`.
    pub new_max_term_seconds: Option<u32>,
}

/// Update mutable parameters on a sub-vault. Rejected when the profile is
/// sunset.
pub fn process_update_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateSubVaultParams::try_from_slice(data)?;

    let CreateSubVaultContext {
        payer: _,
        vault,
        _system_program: _,
    } = CreateSubVaultContext::load(accounts)?;

    if let Some(v) = params.new_max_ltv_bps {
        require!(
            v > 0 && v < 10_000,
            YdeltaError::SubVaultLtvOutOfRange,
            "new_max_ltv_bps {} must be in (0, 10_000)",
            v
        )?;
    }
    if let Some(v) = params.new_max_term_seconds {
        require!(
            v > 0,
            YdeltaError::SubVaultTermInvalid,
            "new_max_term_seconds must be > 0"
        )?;
    }

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
        "sub_vault_id {} not found",
        params.sub_vault_id
    )?;

    let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
    require!(
        profile.is_sunset == 0,
        YdeltaError::SubVaultSunset,
        "update_sub_vault: sub_vault_id {} is sunset; parameter updates are rejected \
         during wind-down",
        params.sub_vault_id
    )?;

    if let Some(v) = params.new_max_ltv_bps {
        profile.max_ltv_bps = v;
    }
    if let Some(v) = params.new_max_term_seconds {
        profile.max_term_seconds = v;
    }

    Ok(())
}
