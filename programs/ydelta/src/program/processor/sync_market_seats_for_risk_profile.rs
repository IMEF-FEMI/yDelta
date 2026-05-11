//! Re-stamp cached risk-profile policy onto market-side vault seats.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::{ClaimedSeat, OWNER_KIND_RISK_PROFILE};
use crate::state::market::{get_mut_helper_seat, ClaimedSeatTreeReadOnly, MarketFixed};
use crate::state::vault::{
    get_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::SyncMarketSeatsForRiskProfileContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SyncMarketSeatsForRiskProfileParams {
    pub profile_id: u8,
}

pub fn process_sync_market_seats_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SyncMarketSeatsForRiskProfileParams::try_from_slice(data)?;
    let SyncMarketSeatsForRiskProfileContext {
        payer: _,
        global_vault,
        markets,
    } = SyncMarketSeatsForRiskProfileContext::load(accounts)?;

    let global_vault_key = *global_vault.info.key;

    // Read the live `max_ltv_bps` from the risk_profile, plus the
    // active_markets array for membership validation.
    let (live_max_ltv_bps, active_markets, allowed_market_count): (u16, [Pubkey; 8], u8) = {
        let vault_data = global_vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "sync_market_seats_for_risk_profile: profile_id {} not found",
            params.profile_id
        )?;
        let profile = get_helper_risk_profile(dynamic, profile_idx).get_value();
        (
            profile.max_ltv_bps,
            profile.active_markets,
            profile.allowed_market_count,
        )
    };

    // Re-stamp each passed market's vault-owned seat. Markets not in
    // `active_markets` are rejected (defense against a stranger
    // sneaking in foreign market accounts to write spoofed seats).
    for market_ai in markets.iter() {
        let market_key = *market_ai.key;
        require!(
            active_markets[..allowed_market_count as usize].contains(&market_key),
            YdeltaError::IncorrectAccount,
            "sync_market_seats_for_risk_profile: market not in profile.active_markets"
        )?;

        let market_data: &mut RefMut<&mut [u8]> = &mut market_ai.try_borrow_mut_data()?;
        let dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..dyn_offset]);
        let claimed_root = header.claimed_seats_root_index;
        let dynamic = &mut market_data[dyn_offset..];

        let probe =
            ClaimedSeat::new_empty(global_vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let seat_idx = {
            let tree = ClaimedSeatTreeReadOnly::new(dynamic, claimed_root, NIL);
            tree.lookup_index(&probe)
        };
        // Silently skip markets where the seat was released between
        // active_markets snapshot and now (shouldn't happen single-
        // threaded, but defensive).
        if seat_idx == NIL {
            continue;
        }
        let seat = get_mut_helper_seat(dynamic, seat_idx).get_mut_value();
        seat.risk_profile_max_ltv_bps = live_max_ltv_bps;
    }

    Ok(())
}
