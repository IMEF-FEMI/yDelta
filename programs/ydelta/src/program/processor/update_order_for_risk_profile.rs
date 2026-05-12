//! Update a vault profile's resting market order.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, CancelOrderForRiskProfileLog, PlaceOrderForRiskProfileLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::market::{get_helper_seat, ClaimedSeatTreeReadOnly};
use crate::state::market_helpers::{cancel_order_by_index, place_order_inner};
use crate::state::vault::{
    insert_risk_profile_order_ref, remove_risk_profile_order_ref, vault_expand_node_block,
    GlobalVaultFixed, RiskProfile, RiskProfileOrderRef, RiskProfileOrderRefTreeReadOnly,
    RiskProfileTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{
    MarketFixed, OrderKind, OrderType, Side, GLOBAL_VAULT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE,
};
use crate::validation::loaders::CancelOrderForRiskProfileContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateOrderForRiskProfileParams {
    pub profile_id: u8,
    pub new_rate_bps: u16,
    pub new_term_seconds: u32,
    pub new_flags: u8,
}

pub fn process_update_order_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateOrderForRiskProfileParams::try_from_slice(data)?;
    let CancelOrderForRiskProfileContext {
        fee_payer,
        curator,
        vault,
        market,
        _system_program,
    } = CancelOrderForRiskProfileContext::load(accounts)?;

    let vault_key = *vault.info.key;
    let market_key = *market.info.key;
    let now: i64 = Clock::get()?.unix_timestamp;

    // Re-stamp the market-side vault seat's risk_profile_max_ltv_bps
    // from the live RiskProfile so the borrower-LTV gate uses the
    // current cap.
    super::shared::sync_vault_seat_from_profile(market.info, vault.info, params.profile_id)?;

    // Validate curator, profile, and the existing vault order.
    let old_order_sequence: u64 = {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);

        let profile_probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
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
        let profile = profile_node.get_value();
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "update_order_for_risk_profile: signer is not profile.curator"
        )?;
        require!(
            params.new_term_seconds <= profile.max_term_seconds,
            YdeltaError::VaultOrderTermExceedsProfileMax,
            "new_term_seconds {} > profile.max_term_seconds {}",
            params.new_term_seconds,
            { profile.max_term_seconds }
        )?;

        // Order ref (must exist).
        let order_probe = RiskProfileOrderRef::probe(market_key, params.profile_id);
        let order_idx = {
            let tree =
                RiskProfileOrderRefTreeReadOnly::new(dynamic, header.market_orders_root_index, NIL);
            tree.lookup_index(&order_probe)
        };
        require!(
            order_idx != NIL,
            YdeltaError::InvalidArgument,
            "no RiskProfileOrderRef for (market, profile_id={}) — nothing to update",
            params.profile_id
        )?;
        crate::state::vault::get_helper_risk_profile_order_ref(dynamic, order_idx)
            .get_value()
            .order_sequence_in_market
    };

    // Read the per-market cap from the market-side vault ClaimedSeat.
    let (max_exposure_atoms, taker_seat_index): (u64, hypertree::DataIndex) = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_RISK_PROFILE, params.profile_id);
        let seat_idx = {
            let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            seat_idx != NIL,
            YdeltaError::IncorrectAccount,
            "no vault-owned ClaimedSeat for (vault, profile_id)"
        )?;
        let seat = get_helper_seat(dynamic, seat_idx).get_value();
        (seat.max_exposure_atoms(), seat_idx)
    };

    // Cancel and replace on the market side.
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
        // Vault orders are always primary — discard the
        // secondary-loan-pda return value.
        let _ = cancel_order_by_index(
            da.fixed,
            da.dynamic,
            taker_seat_index,
            order_index_in_market,
        )?;

        let args = crate::state::market_helpers::PlaceOrderArgs {
            market_pubkey: market_key,
            taker_seat_index,
            side: Side::Ask,
            kind: OrderKind::Primary,
            // Risk-profile orders are makers by design — never takers.
            // PostOnly mirrors `place_order_for_risk_profile`'s placement.
            order_type: OrderType::PostOnly,
            rate_bps: params.new_rate_bps,
            term_seconds: params.new_term_seconds,
            principal_atoms: max_exposure_atoms,
            collateral_atoms: 0,
            // Risk-profile orders are non-expiring; only the curator
            // removes them via cancel_order_for_risk_profile.
            last_valid_unix_ts: crate::state::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
            flags: params.new_flags,
            now_unix_ts: now,
            // Unused on the vault path (see place_order_for_risk_profile).
            share_price_snapshot_fp48: 0,
            debt_oracle_price_fp48: 0,
            collateral_oracle_price_fp48: 0,
            debt_liability_weight_init_fp48: 0,
            collateral_asset_weight_init_fp48: 0,
            enforce_ltv: false,
            is_vault_lender: true,
            borrower_ltv_bps: 0,
        };
        // Vault asks rest as makers — they don't take. No vault-side
        // matching to gate, None is fine.
        let result = place_order_inner(da.fixed, da.dynamic, args, None)?;
        require!(
            result.rested,
            YdeltaError::InvalidArgument,
            "vault update did not rest"
        )?;
        result.sequence
    };

    // Rebuild the RiskProfileOrderRef.
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        let _ = remove_risk_profile_order_ref(header, dynamic, market_key, params.profile_id);
    }
    // Re-expand if the cancel emptied the free list — defensive.
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
        let _ = insert_risk_profile_order_ref(
            header,
            dynamic,
            market_key,
            params.profile_id,
            Side::Ask as u8,
            params.new_rate_bps,
            params.new_term_seconds,
            new_order_sequence,
            now,
        )?;
    }

    // Two log events: cancel-leg (is_replace=1) + place-leg.
    emit_stack(CancelOrderForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        is_replace: 1,
        _pad0: [0; 6],
        order_sequence_in_market: old_order_sequence,
    })?;
    emit_stack(PlaceOrderForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        side: Side::Ask as u8,
        _pad0: [0; 6],
        rate_bps: params.new_rate_bps,
        _pad1: [0; 2],
        term_seconds: params.new_term_seconds,
        order_sequence_in_market: new_order_sequence,
    })?;

    Ok(())
}
