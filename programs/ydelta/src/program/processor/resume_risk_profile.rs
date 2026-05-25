//! `ResumeRiskProfile` — vault-admin un-sunset for a risk profile. Signer is
//! the vault admin (via `ResumeRiskProfileContext`). Clears `is_sunset`, which
//! re-enables deposits, new orders, order updates, and matching for the
//! profile.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ResumeRiskProfileContext;

/// Resume parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ResumeRiskProfileParams {
    /// Risk profile ID to un-sunset (1-based; 0 is the sentinel).
    pub profile_id: u8,
}

/// Clear the profile's `is_sunset` flag, re-enabling deposits, new orders,
/// updates, and matches.
pub fn process_resume_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ResumeRiskProfileParams::try_from_slice(data)?;
    let ResumeRiskProfileContext { _payer: _, vault } = ResumeRiskProfileContext::load(accounts)?;

    let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

    let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
    let idx = {
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        idx != NIL,
        YdeltaError::VaultProfileNotFound,
        "resume_risk_profile: profile_id {} not found",
        params.profile_id
    )?;

    let profile = get_mut_helper_risk_profile(dynamic, idx).get_mut_value();
    profile.is_sunset = 0;
    Ok(())
}
