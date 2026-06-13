//! `UpdateOrderForSubVault` — atomic cancel-and-replace of the existing
//! vault-owned ask for `(market, sub_vault_id)`. Signer is the profile's
//! curator; blocked when the sub-vault is sunset (curators may still cancel
//! during wind-down). Looks up the old order via the vault's
//! `SubVaultOrderRef`, cancels it on the market, rests a new ask with the
//! supplied params, and swaps the ref entry on the vault.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, CancelOrderForSubVaultLog, PlaceOrderForSubVaultLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_SUB_VAULT;
use crate::state::market::ClaimedSeatTreeReadOnly;
use crate::state::market_helpers::{cancel_order_by_index, rest_vault_ask, RestVaultAskArgs};
use crate::state::vault::{
    insert_sub_vault_order_ref, remove_sub_vault_order_ref, vault_expand_node_block,
    GlobalVaultFixed, SubVault, SubVaultOrderRef, SubVaultOrderRefTreeReadOnly,
    SubVaultTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, Side, GLOBAL_VAULT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE};
use crate::validation::loaders::PlaceOrderForSubVaultContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

/// Cancel-and-replace parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateOrderForSubVaultParams {
    /// Sub-vault ID that owns the order (1-based; 0 is the sentinel).
    pub sub_vault_id: u16,
    /// Replacement order flag bits (see `state::market_helpers` flag constants).
    pub new_flags: u8,
}

/// Parameterless re-sync: cancels the sub-vault's existing ask
/// for this market and rests a fresh one at `live bank lending APR +
/// sub_vault.spread_bps` / `sub_vault.max_term_seconds` in a single
/// instruction. Curator-signed; rejected when the sub-vault is sunset.
pub fn process_update_order_for_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateOrderForSubVaultParams::try_from_slice(data)?;
    let PlaceOrderForSubVaultContext {
        fee_payer,
        curator,
        vault,
        market,
        debt_bank,
        marginfi_group,
        collateral_bank,
        debt_oracle_ais,
        collateral_oracle_ais,
        _system_program,
    } = PlaceOrderForSubVaultContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;
    let now: i64 = Clock::get()?.unix_timestamp;

    let sub_vault_spread_bps: u16;
    let sub_vault_term_seconds: u32;
    let sub_vault_max_ltv_bps: u16;
    let sub_vault_liquidation_ltv_bps: u16;
    let sub_vault_curator_fee_bps: u16;
    let old_order_sequence: u64 = {
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
        let profile = profile_node.get_value();
        require!(
            profile.is_sunset == 0,
            YdeltaError::SubVaultSunset,
            "update_order_for_sub_vault: sub_vault_id {} is sunset; updates are rejected \
             during wind-down (cancel is allowed)",
            params.sub_vault_id
        )?;
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "update_order_for_sub_vault: signer is not sub_vault.curator"
        )?;
        sub_vault_spread_bps = profile.spread_bps;
        sub_vault_term_seconds = profile.max_term_seconds;
        sub_vault_max_ltv_bps = profile.max_ltv_bps;
        sub_vault_liquidation_ltv_bps = profile.liquidation_ltv_bps;
        sub_vault_curator_fee_bps = profile.curator_fee_bps;

        let order_probe = SubVaultOrderRef::probe(market_key, params.sub_vault_id);
        let order_idx = {
            let tree =
                SubVaultOrderRefTreeReadOnly::new(dynamic, header.market_orders_root_index, NIL);
            tree.lookup_index(&order_probe)
        };
        require!(
            order_idx != NIL,
            YdeltaError::InvalidArgument,
            "no SubVaultOrderRef for (market, sub_vault_id={}) — nothing to update",
            params.sub_vault_id
        )?;
        crate::state::vault::get_helper_sub_vault_order_ref(dynamic, order_idx)
            .get_value()
            .order_sequence_in_market
    };

    // stored rate = live bank lending APR (ceil) + spread.
    let bank_apr_bps: u16 = crate::protocol::marginfi_rate_calc::current_lending_apr_bps_ceil(
        debt_bank.info,
        marginfi_group.info,
    )?;
    let new_rate_bps: u16 = bank_apr_bps
        .checked_add(sub_vault_spread_bps)
        .ok_or(YdeltaError::MathOverflow)?;
    let new_term_seconds: u32 = sub_vault_term_seconds;

    let taker_seat_index: hypertree::DataIndex = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
        let seat_idx = {
            let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            seat_idx != NIL,
            YdeltaError::IncorrectAccount,
            "no vault-owned ClaimedSeat for (vault, sub_vault_id)"
        )?;
        seat_idx
    };

    expand_market_if_needed(fee_payer.info, &market)?;
    let new_order_sequence: u64 = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);

        let order_index_in_market = crate::state::market_helpers::lookup_order_by_seq(
            da.fixed,
            da.dynamic,
            taker_seat_index,
            old_order_sequence,
            None,
        )?;
        cancel_order_by_index(
            da.fixed,
            da.dynamic,
            taker_seat_index,
            order_index_in_market,
        )?;

        rest_vault_ask(
            da.fixed,
            da.dynamic,
            RestVaultAskArgs {
                market_pubkey: market_key,
                maker_seat_index: taker_seat_index,
                rate_bps: new_rate_bps,
                term_seconds: new_term_seconds,
                flags: params.new_flags,
                now_unix_ts: now,
            },
        )?
    };

    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        remove_sub_vault_order_ref(header, dynamic, market_key, params.sub_vault_id)?;
    }

    let need_expand: bool = {
        let vault_data = vault.info.try_borrow_data()?;
        let header: &GlobalVaultFixed =
            bytemuck::from_bytes(&vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
        !header.has_free_node_block()
    };
    if need_expand {
        let new_size = vault.info.data_len() + VAULT_NODE_BLOCK_SIZE;
        let rent: Rent = Rent::get()?;
        let new_min = rent.minimum_balance(new_size);
        let old_min = rent.minimum_balance(vault.info.data_len());
        let lamports_diff = new_min.saturating_sub(old_min);
        if lamports_diff > 0 {
            invoke(
                &system_instruction::transfer(fee_payer.info.key, vault.info.key, lamports_diff),
                &[fee_payer.info.clone(), vault.info.clone()],
            )?;
        }
        #[allow(deprecated)]
        vault.info.realloc(new_size, false)?;
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        vault_expand_node_block(header, dynamic)?;
    }
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let _ = insert_sub_vault_order_ref(
            header,
            dynamic,
            market_key,
            params.sub_vault_id,
            Side::Ask as u8,
            new_rate_bps,
            new_term_seconds,
            new_order_sequence,
            now,
        )?;
    }

    // the re-sync TAKES — a repriced ask sweeps newly-crossable
    // resting bids in the same instruction.
    let _ = crate::program::processor::place_order_for_sub_vault::take_resting_bids(
        &market,
        vault.info,
        &debt_bank,
        &collateral_bank,
        &debt_oracle_ais,
        &collateral_oracle_ais,
        &fee_payer,
        taker_seat_index,
        params.sub_vault_id,
        new_rate_bps,
        new_term_seconds,
        sub_vault_curator_fee_bps,
        *curator.info.key,
        sub_vault_max_ltv_bps,
        sub_vault_liquidation_ltv_bps,
        now,
        u32::MAX,
    )?;

    emit_stack(CancelOrderForSubVaultLog {
        global_vault: vault_key,
        market: market_key,
        sub_vault_id: params.sub_vault_id,
        is_replace: 1,
        _pad0: [0; 5],
        order_sequence_in_market: old_order_sequence,
    })?;
    emit_stack(PlaceOrderForSubVaultLog {
        global_vault: vault_key,
        market: market_key,
        sub_vault_id: params.sub_vault_id,
        side: Side::Ask as u8,
        _pad0: [0; 5],
        rate_bps: new_rate_bps,
        _pad1: [0; 2],
        term_seconds: new_term_seconds,
        order_sequence_in_market: new_order_sequence,
    })?;

    Ok(())
}
