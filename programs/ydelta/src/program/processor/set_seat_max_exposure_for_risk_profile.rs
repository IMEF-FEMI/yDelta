//! Update an existing vault-owned market seat's `max_exposure_atoms`.
//!
//! Counterpart to `claim_seat_for_risk_profile`'s write-once cap stamp:
//! once a seat is live, the curator can dial its per-(profile, market)
//! cap up or down without releasing + re-claiming. Used by the cranker
//! to keep the cap tracking the profile's current risk score × deposit
//! base (see `crankers::handlers::curator_keeper`).
//!
//! Rejects shrinking below `deployed_atoms`: the matching engine treats
//! `seat.max_exposure_atoms` as a hard cap on `deployed_atoms` after a
//! match, so a value below the running tally would brick all future
//! matches against this seat until loans repay down.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, SetSeatMaxExposureForRiskProfileLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::market::{get_mut_helper_seat, ClaimedSeatTreeReadOnly};
use crate::state::vault::{GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, GLOBAL_VAULT_FIXED_SIZE};
use crate::validation::loaders::CancelOrderForRiskProfileContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SetSeatMaxExposureForRiskProfileParams {
    pub profile_id: u8,
    pub max_exposure_atoms: u64,
}

pub fn process_set_seat_max_exposure_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SetSeatMaxExposureForRiskProfileParams::try_from_slice(data)?;
    // Same account shape as cancel_order_for_risk_profile — fee_payer,
    // curator, global_config, vault, market, system_program. No rent
    // expansion happens here; system_program is unused.
    let CancelOrderForRiskProfileContext {
        fee_payer: _,
        curator,
        vault,
        market,
        _system_program: _,
    } = CancelOrderForRiskProfileContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;

    // Same `> 0` invariant as `claim_seat_for_risk_profile`: the
    // matching engine treats the cap as a hard limit, and a zero cap
    // would silently disable matching.
    require!(
        params.max_exposure_atoms > 0,
        YdeltaError::InvalidArgument,
        "max_exposure_atoms must be > 0 (use a generous u64 if you intend uncapped)"
    )?;

    // ─── Curator gate (same pattern as claim_seat_for_risk_profile) ───
    {
        let vault_data = vault.info.try_borrow_data()?;
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
        let profile = crate::state::vault::get_helper_risk_profile(dynamic, profile_idx).get_value();
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "set_seat_max_exposure_for_risk_profile: signer is not profile.curator"
        )?;
    }

    // ─── Locate the market-side seat and update it in place ───
    let (seat_index_in_market, previous_max_exposure_atoms): (hypertree::DataIndex, u64) = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..dyn_offset]);
        let claimed_root = header.claimed_seats_root_index;
        let dynamic = &mut market_data[dyn_offset..];

        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let seat_idx = {
            let tree = ClaimedSeatTreeReadOnly::new(dynamic, claimed_root, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            seat_idx != NIL,
            YdeltaError::VaultProfileSeatExists,
            "no vault-owned ClaimedSeat for (vault, profile_id) — call claim_seat_for_risk_profile first"
        )?;

        let seat = get_mut_helper_seat(dynamic, seat_idx).get_mut_value();
        let deployed = seat.deployed_atoms();
        // Hard floor: shrinking below the running deployed tally would
        // leave existing matched loans over-cap. Matching loops gate on
        // `new_seat_deployed > max_exposure` after each cross, so this
        // would also brick further fills until repay catches up.
        require!(
            params.max_exposure_atoms >= deployed,
            YdeltaError::InvalidArgument,
            "new max_exposure_atoms {} < seat.deployed_atoms {}",
            params.max_exposure_atoms,
            deployed
        )?;
        let prev = seat.max_exposure_atoms();
        seat.set_max_exposure_atoms(params.max_exposure_atoms);
        (seat_idx, prev)
    };

    emit_stack(SetSeatMaxExposureForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        _pad0: [0; 7],
        previous_max_exposure_atoms,
        new_max_exposure_atoms: params.max_exposure_atoms,
        seat_index_in_market,
        _pad1: [0; 4],
    })?;

    Ok(())
}
