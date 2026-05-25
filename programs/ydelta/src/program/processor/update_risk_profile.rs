//! `UpdateRiskProfile` — curator update of a risk profile's mutable
//! parameters. Signer/payer flow is loaded via `CreateRiskProfileContext`.
//! Each `Option<>` field is an override; `None` leaves the field unchanged.
//! Rejected when the profile is sunset.

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
use crate::validation::loaders::CreateRiskProfileContext;

/// Per-field overrides for a `RiskProfile`. `None` leaves the field
/// unchanged; `Some(v)` is validated and written.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateRiskProfileParams {
    /// Risk profile ID to update (1-based; 0 is the sentinel).
    pub profile_id: u8,
    /// New max LTV in bps; must be in `(0, 10_000)`.
    pub new_max_ltv_bps: Option<u16>,
    /// New max term in seconds; must be `> 0`.
    pub new_max_term_seconds: Option<u32>,
}

/// Update mutable parameters on a risk profile. Rejected when the profile is
/// sunset.
pub fn process_update_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateRiskProfileParams::try_from_slice(data)?;

    let CreateRiskProfileContext {
        payer: _,
        vault,
        _system_program: _,
    } = CreateRiskProfileContext::load(accounts)?;

    if let Some(v) = params.new_max_ltv_bps {
        require!(
            v > 0 && v < 10_000,
            YdeltaError::VaultProfileLtvOutOfRange,
            "new_max_ltv_bps {} must be in (0, 10_000)",
            v
        )?;
    }
    if let Some(v) = params.new_max_term_seconds {
        require!(
            v > 0,
            YdeltaError::VaultProfileTermInvalid,
            "new_max_term_seconds must be > 0"
        )?;
    }

    let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

    let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
    let profile_idx = {
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        profile_idx != NIL,
        YdeltaError::VaultProfileNotFound,
        "profile_id {} not found",
        params.profile_id
    )?;

    let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
    require!(
        profile.is_sunset == 0,
        YdeltaError::VaultProfileSunset,
        "update_risk_profile: profile_id {} is sunset; parameter updates are rejected \
         during wind-down",
        params.profile_id
    )?;

    if let Some(v) = params.new_max_ltv_bps {
        profile.max_ltv_bps = v;
    }
    if let Some(v) = params.new_max_term_seconds {
        profile.max_term_seconds = v;
    }

    Ok(())
}
