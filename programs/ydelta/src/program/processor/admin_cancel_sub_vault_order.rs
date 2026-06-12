//! `AdminCancelSubVaultOrder` instruction. Vault-admin-gated force
//! cancel of a sunset sub-vault's resting market ask. Rejects when
//! `profile.is_sunset == 0` — admin force-cancel is only available
//! during wind-down; curators use `CancelOrderForSubVault` otherwise.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, CancelOrderForSubVaultLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_SUB_VAULT;
use crate::state::market::ClaimedSeatTreeReadOnly;
use crate::state::market_helpers::cancel_order_by_index;
use crate::state::vault::{
    remove_sub_vault_order_ref, GlobalVaultFixed, SubVault, SubVaultOrderRef,
    SubVaultOrderRefTreeReadOnly, SubVaultTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, GLOBAL_VAULT_FIXED_SIZE};
use crate::validation::loaders::AdminCancelSubVaultOrderContext;

use super::shared::get_mut_dynamic_account;

/// Parameters for [`process_admin_cancel_sub_vault_order`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct AdminCancelSubVaultOrderParams {
    /// Identifies the sunset sub-vault whose resting market ask
    /// should be force-cancelled.
    pub sub_vault_id: u8,
}

/// Vault-admin force-cancel a sunset profile's resting ask. Removes
/// the market resting order and the vault-side `SubVaultOrderRef`,
/// then emits `CancelOrderForSubVaultLog`. Errors when the profile
/// is not sunset or the order ref is missing.
pub fn process_admin_cancel_sub_vault_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = AdminCancelSubVaultOrderParams::try_from_slice(data)?;
    let AdminCancelSubVaultOrderContext {
        _payer: _,
        vault,
        market,
    } = AdminCancelSubVaultOrderContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;

    let order_sequence: u64 = {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);

        let profile_probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
            tree.lookup_index(&profile_probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::SubVaultNotFound,
            "admin_cancel_sub_vault_order: sub_vault_id {} not found",
            params.sub_vault_id
        )?;
        let profile = crate::state::vault::get_helper_sub_vault(dynamic, profile_idx).get_value();
        require!(
            profile.is_sunset != 0,
            YdeltaError::SubVaultNotSunset,
            "admin_cancel_sub_vault_order: sub_vault_id {} is not sunset; admin force-cancel is \
             only allowed during sunset wind-down (call SunsetSubVault first, or have the \
             curator cancel via CancelOrderForSubVault)",
            params.sub_vault_id
        )?;

        let order_probe = SubVaultOrderRef::probe(market_key, params.sub_vault_id);
        let order_idx = {
            let tree =
                SubVaultOrderRefTreeReadOnly::new(dynamic, header.market_orders_root_index, NIL);
            tree.lookup_index(&order_probe)
        };
        require!(
            order_idx != NIL,
            YdeltaError::InvalidArgument,
            "admin_cancel_sub_vault_order: no order ref for (market, sub_vault_id={})",
            params.sub_vault_id
        )?;
        let order_node =
            crate::state::vault::get_helper_sub_vault_order_ref(dynamic, order_idx);
        order_node.get_value().order_sequence_in_market
    };

    let taker_seat_index: hypertree::DataIndex = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::IncorrectAccount,
            "admin_cancel_sub_vault_order: no vault-owned ClaimedSeat for (vault, sub_vault_id)"
        )?;
        idx
    };

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

    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let removed_idx =
            remove_sub_vault_order_ref(header, dynamic, market_key, params.sub_vault_id)?;
        require!(
            removed_idx != NIL,
            YdeltaError::InvalidArgument,
            "admin_cancel_sub_vault_order: vault order ref removal failed"
        )?;
    }

    emit_stack(CancelOrderForSubVaultLog {
        global_vault: vault_key,
        market: market_key,
        sub_vault_id: params.sub_vault_id,
        is_replace: 0,
        _pad0: [0; 6],
        order_sequence_in_market: order_sequence,
    })?;

    Ok(())
}
