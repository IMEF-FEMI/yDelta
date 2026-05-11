//! Release a vault-owned market seat when it has no live state.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::{ClaimedSeat, OWNER_KIND_RISK_PROFILE};
use crate::state::market::{
    get_helper_seat, ClaimedSeatTree, ClaimedSeatTreeReadOnly, MarketFixed,
};
use crate::state::market_helpers::release_address_on_market_fixed;
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ClaimSeatForRiskProfileContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ReleaseSeatForRiskProfileParams {
    pub profile_id: u8,
}

pub fn process_release_seat_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ReleaseSeatForRiskProfileParams::try_from_slice(data)?;
    // Reuse ClaimSeatForRiskProfileContext: same admin gate (global_vault_admin),
    // same accounts (payer + global_config + vault + market +
    // system_program), same mint-bind invariant.
    let ClaimSeatForRiskProfileContext {
        payer: _,
        vault,
        market,
        system_program: _,
    } = ClaimSeatForRiskProfileContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;

    // ─── Locate the seat in the market and verify it is empty. ───
    let seat_index = {
        let market_data = market.info.try_borrow_data()?;
        let dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..dyn_offset]);
        let dynamic = &market_data[dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::IncorrectAccount,
            "no vault-owned ClaimedSeat for (vault={}, profile_id={}) in this market",
            vault_key,
            params.profile_id
        )?;
        let seat = get_helper_seat(dynamic, idx).get_value();
        // All-empty gate.
        require!(
            seat.deployed_atoms() == 0,
            YdeltaError::SeatHasOpenObligations,
            "release_seat_for_risk_profile: seat.deployed_atoms = {} (loan principal in flight)",
            seat.deployed_atoms()
        )?;
        require!(
            seat.open_lend_count == 0,
            YdeltaError::SeatHasOpenObligations,
            "release_seat_for_risk_profile: seat.open_lend_count = {} (resting vault asks)",
            { seat.open_lend_count }
        )?;
        require!(
            seat.open_borrow_count == 0,
            YdeltaError::SeatHasOpenObligations,
            "release_seat_for_risk_profile: seat.open_borrow_count = {}",
            { seat.open_borrow_count }
        )?;
        require!(
            seat.debt_withdrawable_shares == 0,
            YdeltaError::SeatHasOpenObligations,
            "release_seat_for_risk_profile: seat.debt_withdrawable_shares = {} (curator must \
             sweep matured-loan proceeds via claim_repayment + sweep ix first)",
            { seat.debt_withdrawable_shares }
        )?;
        require!(
            seat.debt_encumbered_shares == 0,
            YdeltaError::SeatHasOpenObligations,
            "release_seat_for_risk_profile: seat.debt_encumbered_shares = {}",
            { seat.debt_encumbered_shares }
        )?;
        idx
    };

    // ─── Remove the seat from the market's tree + free its block. ───
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let dyn_offset = std::mem::size_of::<MarketFixed>();
        let (fixed_bytes, dynamic) = market_data.split_at_mut(dyn_offset);
        let header: &mut MarketFixed = bytemuck::from_bytes_mut(fixed_bytes);
        {
            let mut tree = ClaimedSeatTree::new(dynamic, header.claimed_seats_root_index, NIL);
            tree.remove_by_index(seat_index);
            header.claimed_seats_root_index = tree.get_root_index();
        }
        release_address_on_market_fixed(header, dynamic, seat_index);
        header.position_count = header.position_count.saturating_sub(1);
    }

    // ─── Drop the market from active_markets (decrements
    // `allowed_market_count` via swap-remove). Idempotent.
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
            let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
            profile.remove_active_market(&market_key);
        }
    }

    Ok(())
}
