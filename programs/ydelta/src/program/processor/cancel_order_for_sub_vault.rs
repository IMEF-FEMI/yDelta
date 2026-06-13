//! `CancelOrderForSubVault` instruction. Curator-gated removal of a
//! sub-vault's resting market ask. Removes both the market-side
//! resting order and the vault-side `SubVaultOrderRef`, then emits
//! `CancelOrderForSubVaultLog`. Allowed regardless of sunset state.

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
use crate::validation::loaders::CancelOrderForSubVaultContext;

use super::shared::get_mut_dynamic_account;

/// Parameters for [`process_cancel_order_for_sub_vault`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct CancelOrderForSubVaultParams {
    /// Identifies the sub-vault whose resting ask to cancel.
    pub sub_vault_id: u16,
}

/// Cancel a sub-vault's resting market ask. Signer must equal the
/// sub-vault's `curator`; errors if no `SubVaultOrderRef` exists for
/// the `(market, sub_vault_id)` pair.
pub fn process_cancel_order_for_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CancelOrderForSubVaultParams::try_from_slice(data)?;
    let CancelOrderForSubVaultContext {
        _fee_payer: _,
        curator,
        vault,
        market,
        _system_program,
    } = CancelOrderForSubVaultContext::load(accounts)?;

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
            "sub_vault_id {} not found",
            params.sub_vault_id
        )?;
        let profile_node = crate::state::vault::get_helper_sub_vault(dynamic, profile_idx);
        require!(
            *curator.info.key == profile_node.get_value().curator,
            YdeltaError::VaultCuratorRequired,
            "cancel_order_for_sub_vault: signer is not sub_vault.curator"
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
            "no SubVaultOrderRef for (market, sub_vault_id={}) — nothing to cancel",
            params.sub_vault_id
        )?;
        let order_node = crate::state::vault::get_helper_sub_vault_order_ref(dynamic, order_idx);
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
            "no vault-owned ClaimedSeat for (vault, sub_vault_id) — invariant violation"
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
            "vault order ref removal failed — race condition?"
        )?;
    }

    emit_stack(CancelOrderForSubVaultLog {
        global_vault: vault_key,
        market: market_key,
        sub_vault_id: params.sub_vault_id,
        is_replace: 0,
        _pad0: [0; 5],
        order_sequence_in_market: order_sequence,
    })?;

    Ok(())
}
