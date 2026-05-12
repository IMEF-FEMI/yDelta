//! Place a vault profile's market-side resting order.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, PlaceOrderForRiskProfileLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use crate::state::market::{get_helper_seat, ClaimedSeatTreeReadOnly};
use crate::state::market_helpers::place_order_inner;
use crate::state::vault::{
    insert_risk_profile_order_ref, vault_expand_node_block, GlobalVaultFixed, RiskProfile,
    RiskProfileTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{
    MarketFixed, OrderKind, OrderType, Side, GLOBAL_VAULT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE,
};
use crate::validation::loaders::CancelOrderForRiskProfileContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct PlaceOrderForRiskProfileParams {
    pub profile_id: u8,
    pub rate_bps: u16,
    pub term_seconds: u32,
    pub flags: u8,
}

pub fn process_place_order_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = PlaceOrderForRiskProfileParams::try_from_slice(data)?;
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

    // ─── Curator gate + profile policy (read from vault) ───
    {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
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
            "profile_id {} not found",
            params.profile_id
        )?;
        let profile_node = crate::state::vault::get_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_value();
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "place_order_for_risk_profile: signer is not profile.curator"
        )?;
        require!(
            params.term_seconds <= profile.max_term_seconds,
            YdeltaError::VaultOrderTermExceedsProfileMax,
            "term_seconds {} > profile.max_term_seconds {}",
            params.term_seconds,
            { profile.max_term_seconds }
        )?;
    }

    // ─── Per-market cap (read from market-side ClaimedSeat) ───
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
            YdeltaError::VaultProfileSeatExists,
            "no vault-owned ClaimedSeat for (vault, profile_id) — call claim_seat_for_risk_profile first"
        )?;
        let seat = get_helper_seat(dynamic, seat_idx).get_value();
        (seat.max_exposure_atoms(), seat_idx)
    };

    // ─── Insert the market-side RestingOrder ───
    expand_market_if_needed(fee_payer.info, &market)?;
    let order_sequence: u64 = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);

        let args = crate::state::market_helpers::PlaceOrderArgs {
            market_pubkey: market_key,
            taker_seat_index,
            side: Side::Ask, // vaults only post asks
            kind: OrderKind::Primary,
            // Risk-profile orders are makers by design — never takers.
            // PostOnly enforces this at the engine layer; without it,
            // a fresh vault Ask could immediately cross any compatible
            // wallet Bid, bypassing the idle / exposure gate (which
            // only fires when the taker is a Bid that crosses a
            // resting risk-profile maker).
            order_type: OrderType::PostOnly,
            rate_bps: params.rate_bps,
            term_seconds: params.term_seconds,
            principal_atoms: max_exposure_atoms,
            collateral_atoms: 0, // ask side — no collateral
            // Risk-profile orders are non-expiring; only the curator
            // removes them via cancel_order_for_risk_profile.
            last_valid_unix_ts: crate::state::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
            flags: params.flags,
            now_unix_ts: now,
            // No LTV / oracle gate at vault placement — vault is the
            // lender, no liability being opened.
            //
            // `share_price_snapshot_fp48` is unused on the vault path:
            // `is_vault_lender = true` makes `place_order_inner` skip
            // the per-seat encumber, so no decrement-by-snapshot ever
            // fires against this resting order. Pass 0 to make that
            // explicit; settlement of vault-funded loans goes through
            // `vault.integration`, not seat shares.
            share_price_snapshot_fp48: 0,
            debt_oracle_price_fp48: 0,
            collateral_oracle_price_fp48: 0,
            debt_liability_weight_init_fp48: 0,
            collateral_asset_weight_init_fp48: 0,
            enforce_ltv: false,
            is_vault_lender: true,
            // Vault asks don't borrow — borrower-LTV gate is N/A.
            borrower_ltv_bps: 0,
        };
        // Vault asks rest as makers — they don't take. No vault-side
        // matching to gate, None is fine.
        let result = place_order_inner(da.fixed, da.dynamic, args, None)?;
        require!(
            result.rested,
            YdeltaError::InvalidArgument,
            "vault order did not rest (matched against itself or PostOnly cross)"
        )?;
        result.sequence
    };

    // ─── Insert the vault-side RiskProfileOrderRef ───
    // Make sure there's a free node block.
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
            /*side=*/ Side::Ask as u8,
            params.rate_bps,
            params.term_seconds,
            order_sequence,
            now,
        )?;
    }

    let _ = OWNER_KIND_RISK_PROFILE;
    emit_stack(PlaceOrderForRiskProfileLog {
        global_vault: vault_key,
        market: market_key,
        profile_id: params.profile_id,
        side: Side::Ask as u8,
        _pad0: [0; 6],
        rate_bps: params.rate_bps,
        _pad1: [0; 2],
        term_seconds: params.term_seconds,
        order_sequence_in_market: order_sequence,
    })?;

    Ok(())
}
