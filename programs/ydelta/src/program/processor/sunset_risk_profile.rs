//! `SunsetRiskProfile` — vault-admin sunset for a risk profile. Signer is
//! the vault admin (via `SunsetRiskProfileContext`). Sets `is_sunset = 1`,
//! which blocks deposits, new orders, order updates, and matches; withdrawals,
//! cancels, repay, settle, and liquidate stay open during wind-down.
//! `AdminCancelRiskProfileOrder` and `RemoveRiskProfile` both gate on this
//! flag.

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
use crate::validation::loaders::SunsetRiskProfileContext;

/// Sunset parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SunsetRiskProfileParams {
    /// Risk profile ID to sunset (1-based; 0 is the sentinel).
    pub profile_id: u8,
}

/// Set the profile's `is_sunset` flag, blocking deposits / new orders /
/// updates / matches while keeping withdrawals and close-outs open.
pub fn process_sunset_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SunsetRiskProfileParams::try_from_slice(data)?;
    let SunsetRiskProfileContext { _payer: _, vault } = SunsetRiskProfileContext::load(accounts)?;

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
        "sunset_risk_profile: profile_id {} not found",
        params.profile_id
    )?;

    let profile = get_mut_helper_risk_profile(dynamic, idx).get_mut_value();
    profile.is_sunset = 1;
    Ok(())
}
