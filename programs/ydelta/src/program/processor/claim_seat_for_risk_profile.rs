//! Create a vault-owned market seat for a risk profile.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, ClaimSeatForRiskProfileLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::market::{get_mut_helper_seat, ClaimedSeatTreeReadOnly};
use crate::state::vault::{GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, GLOBAL_VAULT_FIXED_SIZE};
use crate::validation::loaders::ClaimSeatForRiskProfileContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ClaimSeatForRiskProfileParams {
    pub profile_id: u8,
    pub max_exposure_atoms: u64,
}

pub fn process_claim_seat_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ClaimSeatForRiskProfileParams::try_from_slice(data)?;
    let ClaimSeatForRiskProfileContext {
        fee_payer,
        curator,
        vault,
        market,
        system_program,
    } = ClaimSeatForRiskProfileContext::load(accounts)?;
    let _ = system_program;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;

    // Reject `max_exposure_atoms == 0` so the matching engine can
    // treat the field as a hard cap without an "unlimited" sentinel.
    require!(
        params.max_exposure_atoms > 0,
        YdeltaError::InvalidArgument,
        "max_exposure_atoms must be > 0 (use a generous u64 if you intend uncapped)"
    )?;

    // ─── Curator gate + profile validation + market-cap check ───
    // Curator-gated (not admin-gated): only the profile's own
    // curator can claim seats on behalf of that profile. Same
    // signer semantics as `place_order_for_risk_profile` /
    // `update_order_for_risk_profile` — curators have full
    // operational authority over their own profile's footprint
    // without involving the vault admin per (profile, market).
    //
    // Also snapshot `max_ltv_bps` so we can stamp it on the
    // market-side seat for the borrower-LTV risk-tier gate (the
    // matching loop reads it from market state — vault state is
    // not borrowable mid-match), and enforce the per-profile cap
    // on simultaneous market participation.
    let profile_max_ltv_bps: u16 = {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);

        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        let profile_idx = tree.lookup_index(&probe);
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found",
            params.profile_id
        )?;

        let profile_node = crate::state::vault::get_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_value();
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "claim_seat_for_risk_profile: signer is not profile.curator"
        )?;
        require!(
            profile.allowed_market_count < profile.allowed_market_max,
            YdeltaError::VaultProfileAllowedMarketsExceeded,
            "profile {} already holds seats in {} markets (max {})",
            params.profile_id,
            profile.allowed_market_count,
            profile.allowed_market_max
        )?;
        profile.max_ltv_bps
    };

    // ─── Insert market-side ClaimedSeat with cap fields populated ───
    expand_market_if_needed(fee_payer.info, &market)?;
    let seat_index_in_market: hypertree::DataIndex;
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);
        da.claim_seat_with_profile(&vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id)?;

        // Look up the index we just inserted, then stamp the cap.
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let tree = ClaimedSeatTreeReadOnly::new(da.dynamic, da.fixed.claimed_seats_root_index, NIL);
        seat_index_in_market = tree.lookup_index(&probe);
        require!(
            seat_index_in_market != NIL,
            YdeltaError::IncorrectAccount,
            "post-insert ClaimedSeat lookup returned NIL"
        )?;
        drop(tree);

        let seat = get_mut_helper_seat(da.dynamic, seat_index_in_market).get_mut_value();
        seat.set_max_exposure_atoms(params.max_exposure_atoms);
        seat.set_deployed_atoms(0);
        seat.risk_profile_max_ltv_bps = profile_max_ltv_bps;
    }

    // ─── Bump profile's market-participation counter ───
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        if profile_idx != NIL {
            let profile = crate::state::vault::get_mut_helper_risk_profile(dynamic, profile_idx)
                .get_mut_value();
            // Append to active_markets (idempotent, bounded by
            // allowed_market_max). The list lets the curator-callable
            // sync ix enumerate which markets need re-stamping after
            // an `update_risk_profile` policy mutation.
            profile.push_active_market(market_key)?;
        }
    }

    emit_stack(ClaimSeatForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        _pad0: [0; 7],
        max_exposure_atoms: params.max_exposure_atoms,
        seat_index_in_market,
        _pad1: [0; 4],
    })?;

    Ok(())
}
