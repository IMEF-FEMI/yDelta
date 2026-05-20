//! Cancel a vault profile's resting market order.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, CancelOrderForRiskProfileLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::market::ClaimedSeatTreeReadOnly;
use crate::state::market_helpers::cancel_order_by_index;
use crate::state::vault::{
    remove_risk_profile_order_ref, GlobalVaultFixed, RiskProfile, RiskProfileOrderRef,
    RiskProfileOrderRefTreeReadOnly, RiskProfileTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, GLOBAL_VAULT_FIXED_SIZE};
use crate::validation::loaders::CancelOrderForRiskProfileContext;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct CancelOrderForRiskProfileParams {
    pub profile_id: u8,
}

pub fn process_cancel_order_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CancelOrderForRiskProfileParams::try_from_slice(data)?;
    let CancelOrderForRiskProfileContext {
        fee_payer: _,
        curator,
        vault,
        market,
        _system_program,
    } = CancelOrderForRiskProfileContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;

    // ─── Curator gate + locate the vault order ───
    let order_sequence: u64 = {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);

        let profile_probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&profile_probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found",
            params.profile_id
        )?;
        let profile_node = crate::state::vault::get_helper_risk_profile(dynamic, profile_idx);
        require!(
            *curator.info.key == profile_node.get_value().curator,
            YdeltaError::VaultCuratorRequired,
            "cancel_order_for_risk_profile: signer is not profile.curator"
        )?;

        let order_probe = RiskProfileOrderRef::probe(market_key, params.profile_id);
        let order_idx = {
            let tree =
                RiskProfileOrderRefTreeReadOnly::new(dynamic, header.market_orders_root_index, NIL);
            tree.lookup_index(&order_probe)
        };
        require!(
            order_idx != NIL,
            YdeltaError::InvalidArgument,
            "no RiskProfileOrderRef for (market, profile_id={}) — nothing to cancel",
            params.profile_id
        )?;
        let order_node = crate::state::vault::get_helper_risk_profile_order_ref(dynamic, order_idx);
        order_node.get_value().order_sequence_in_market
    };

    // ─── Read taker_seat_index from market-side ClaimedSeat ───
    let taker_seat_index: hypertree::DataIndex = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::IncorrectAccount,
            "no vault-owned ClaimedSeat for (vault, profile_id) — invariant violation"
        )?;
        idx
    };

    // ─── Cancel market-side RestingOrder ───
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let order_index_in_market = crate::state::market_helpers::lookup_order_by_seq(
            da.fixed,
            da.dynamic,
            taker_seat_index,
            order_sequence,
            None,
        )?;
        cancel_order_by_index(
            da.fixed,
            da.dynamic,
            taker_seat_index,
            order_index_in_market,
        )?;
    }

    // ─── Remove vault-side RiskProfileOrderRef ───
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let removed_idx =
            remove_risk_profile_order_ref(header, dynamic, market_key, params.profile_id);
        require!(
            removed_idx != NIL,
            YdeltaError::InvalidArgument,
            "vault order ref removal failed — race condition?"
        )?;
    }

    emit_stack(CancelOrderForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        is_replace: 0,
        _pad0: [0; 6],
        order_sequence_in_market: order_sequence,
    })?;

    Ok(())
}
