//! Remove an empty `RiskProfile` from a vault.
//!
//! Vault-admin-gated. The profile's `profile_id` is passed in
//! instruction data. The ix rejects with `VaultProfileNotEmpty` unless
//! every balance field on the profile is zero, so deployed loans,
//! resting orders, idle principal, and unclaimed curator fees cannot
//! be silently stranded by a teardown.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, RiskProfileRemovedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_helper_risk_profile, remove_risk_profile, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::RemoveRiskProfileContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct RemoveRiskProfileParams {
    pub profile_id: u8,
}

pub fn process_remove_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = RemoveRiskProfileParams::try_from_slice(data)?;
    let RemoveRiskProfileContext { _payer: _, vault } = RemoveRiskProfileContext::load(accounts)?;

    let vault_key = *vault.info.key;

    // ─── Look up + emptiness gate ───
    //
    // Scoped read-only borrow so the mut borrow below for the actual
    // removal can re-acquire.
    let curator: Pubkey = {
        let vault_data = vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "remove_risk_profile: profile_id {} not found in vault",
            params.profile_id
        )?;
        let profile: &RiskProfile = get_helper_risk_profile(dynamic, idx).get_value();
        require!(
            profile.is_empty(),
            YdeltaError::VaultProfileNotEmpty,
            "remove_risk_profile: profile_id {} carries live balances \
             (shares={}, assets={}, principal={}, deployed={}, encumbered={}, \
             curator_fee={}); refusing to strand them",
            params.profile_id,
            { profile.total_shares },
            profile.total_assets_atoms,
            profile.total_principal_atoms,
            profile.deployed_principal_atoms,
            profile.encumbered_in_orders_atoms,
            profile.accumulated_curator_fee_atoms
        )?;
        profile.curator
    };

    // ─── Remove ───
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let freed = remove_risk_profile(header, dynamic, params.profile_id);
        require!(
            freed != NIL,
            YdeltaError::VaultProfileNotFound,
            "remove_risk_profile: profile_id {} disappeared between gate and removal \
             (concurrent mutation — should be impossible inside a single ix)",
            params.profile_id
        )?;
    }

    emit_stack(RiskProfileRemovedLog {
        global_vault: vault_key,
        curator,
        profile_id: params.profile_id,
        _pad0: [0; 7],
    })?;

    Ok(())
}
