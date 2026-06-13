//! `PlaceOrderForSubVault` — curator rests a vault-owned ask on a market
//! for one of their sub-vaults. Signer is the sub-vault's `curator`.
//! v1 D4: the ix takes NO rate or term — the stored rate is computed as
//! `live marginfi lending APR (ceil bps) + sub_vault.spread_bps` and the
//! term is `sub_vault.max_term_seconds`, so repricing every market is a
//! parameterless re-sync. Rejected if the sub-vault is sunset. Claims a
//! vault-owned `ClaimedSeat` on first use, rests an ask via
//! `rest_vault_ask`, and inserts a `SubVaultOrderRef` on the global
//! vault.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, PlaceOrderForSubVaultLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::claimed_seat::OWNER_KIND_SUB_VAULT;
use crate::state::market::{ClaimedSeatTreeReadOnly, MarketRefMut};
use crate::state::market_helpers::{rest_vault_ask, RestVaultAskArgs};
use crate::state::vault::{
    insert_sub_vault_order_ref, vault_expand_node_block, GlobalVaultFixed, SubVault,
    SubVaultTreeReadOnly,
};
use crate::state::ClaimedSeat;
use crate::state::{MarketFixed, Side, GLOBAL_VAULT_FIXED_SIZE, VAULT_NODE_BLOCK_SIZE};
use crate::validation::loaders::PlaceOrderForSubVaultContext;

use super::shared::{expand_market_to_free_blocks, get_mut_dynamic_account};

/// Vault-owned ask parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct PlaceOrderForSubVaultParams {
    /// Sub-vault ID on the global vault (1-based; 0 is the sentinel).
    pub sub_vault_id: u16,
    /// Order flag bits (see `state::market_helpers` flag constants).
    pub flags: u8,
}

/// Rest a vault-owned ask on a market on behalf of a sub-vault. Signer is
/// the profile's curator; blocked when the profile is sunset.
pub fn process_place_order_for_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = PlaceOrderForSubVaultParams::try_from_slice(data)?;
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

    let (spread_bps, term_seconds, profile_max_ltv_bps, profile_liquidation_ltv_bps, profile_curator_fee_bps): (u16, u32, u16, u16, u16) = {
        let vault_data: &std::cell::Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);

        let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
            tree.lookup_index(&probe)
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
            "place_order_for_sub_vault: sub_vault_id {} is sunset; no new orders during \
             wind-down (curator may still cancel existing orders)",
            params.sub_vault_id
        )?;
        require!(
            *curator.info.key == profile.curator,
            YdeltaError::VaultCuratorRequired,
            "place_order_for_sub_vault: signer is not profile.curator"
        )?;
        (
            profile.spread_bps,
            profile.max_term_seconds,
            profile.max_ltv_bps,
            profile.liquidation_ltv_bps,
            profile.curator_fee_bps,
        )
    };

    // v1 D4: stored rate = live bank lending APR (ceil) + sub-vault
    // spread. Overflow-checked: a stored rate must fit u16.
    let bank_apr_bps: u16 = crate::protocol::marginfi_rate_calc::current_lending_apr_bps_ceil(
        debt_bank.info,
        marginfi_group.info,
    )?;
    let rate_bps: u16 = bank_apr_bps.checked_add(spread_bps).ok_or_else(|| {
        solana_program::msg!(
            "place_order_for_sub_vault: bank APR {} + spread {} overflows u16",
            bank_apr_bps,
            spread_bps
        );
        solana_program::program_error::ProgramError::from(YdeltaError::MathOverflow)
    })?;

    let seat_exists: bool = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        tree.lookup_index(&probe) != NIL
    };

    let blocks_needed = if seat_exists { 1 } else { 2 };
    expand_market_to_free_blocks(fee_payer.info, &market, blocks_needed)?;

    let order_sequence: u64 = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);

        let taker_seat_index = if seat_exists {
            let tree =
                ClaimedSeatTreeReadOnly::new(da.dynamic, da.fixed.claimed_seats_root_index, NIL);
            let probe =
                ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
            tree.lookup_index(&probe)
        } else {
            {
                let mut market_ref = MarketRefMut {
                    fixed: da.fixed,
                    dynamic: da.dynamic,
                };
                market_ref.claim_seat_with_profile(
                    &vault_key,
                    OWNER_KIND_SUB_VAULT,
                    params.sub_vault_id,
                )?;
            }
            let seat_idx = {
                let tree = ClaimedSeatTreeReadOnly::new(
                    da.dynamic,
                    da.fixed.claimed_seats_root_index,
                    NIL,
                );
                let probe =
                    ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
                tree.lookup_index(&probe)
            };
            require!(
                seat_idx != NIL,
                YdeltaError::IncorrectAccount,
                "post-insert vault ClaimedSeat lookup returned NIL"
            )?;
            seat_idx
        };

        rest_vault_ask(
            da.fixed,
            da.dynamic,
            RestVaultAskArgs {
                market_pubkey: market_key,
                maker_seat_index: taker_seat_index,
                rate_bps,
                term_seconds,
                flags: params.flags,
                now_unix_ts: now,
            },
        )?
    };

    // v1 D7: the placement TAKES — fill crossable resting bids now that
    // the ask's seat exists and the rate is known.
    {
        let seat_idx = {
            let market_data = market.info.try_borrow_data()?;
            let fixed_size = std::mem::size_of::<MarketFixed>();
            let header: &MarketFixed = bytemuck::from_bytes(&market_data[..fixed_size]);
            let dynamic = &market_data[fixed_size..];
            let probe =
                ClaimedSeat::new_empty(vault_key, OWNER_KIND_SUB_VAULT, params.sub_vault_id);
            let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
            tree.lookup_index(&probe)
        };
        let _ = take_resting_bids(
            &market,
            vault.info,
            &debt_bank,
            &collateral_bank,
            &debt_oracle_ais,
            &collateral_oracle_ais,
            &fee_payer,
            seat_idx,
            params.sub_vault_id,
            rate_bps,
            term_seconds,
            profile_curator_fee_bps,
            *curator.info.key,
            profile_max_ltv_bps,
            profile_liquidation_ltv_bps,
            now,
            u32::MAX,
        )?;
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
            rate_bps,
            term_seconds,
            order_sequence,
            now,
        )?;
    }

    emit_stack(PlaceOrderForSubVaultLog {
        global_vault: vault_key,
        market: market_key,
        sub_vault_id: params.sub_vault_id,
        side: Side::Ask as u8,
        _pad0: [0; 5],
        rate_bps,
        _pad1: [0; 2],
        term_seconds,
        order_sequence_in_market: order_sequence,
    })?;

    Ok(())
}

/// Shared v1 D7 take pass: walk the bids tree and fill crossable resting
/// bids against this sub-vault ask before it rests. Returns fills made.
#[allow(clippy::too_many_arguments)]
pub(crate) fn take_resting_bids<'a, 'info>(
    market: &crate::validation::YdeltaAccountInfo<'a, 'info, MarketFixed>,
    vault_ai: &'a solana_program::account_info::AccountInfo<'info>,
    debt_bank: &crate::validation::MarginfiBankInfo<'a, 'info>,
    collateral_bank: &crate::validation::MarginfiBankInfo<'a, 'info>,
    debt_oracle_ais: &crate::validation::MarginfiOracleAis<'a, 'info>,
    collateral_oracle_ais: &crate::validation::MarginfiOracleAis<'a, 'info>,
    fee_payer: &crate::validation::Signer<'a, 'info>,
    ask_seat_index: hypertree::DataIndex,
    ask_sub_vault_id: u16,
    ask_rate_bps: u16,
    ask_term_seconds: u32,
    ask_curator_fee_bps: u16,
    ask_curator: Pubkey,
    profile_max_ltv_bps: u16,
    profile_liquidation_ltv_bps: u16,
    now: i64,
    max_fills: u32,
) -> Result<crate::state::market_helpers::MatchRestingBidsResult, solana_program::program_error::ProgramError>
{
    use crate::protocol::marginfi::{wrapped_i80f48_to_u128, MarginfiV18Adapter};
    use crate::protocol::LendingProtocol;

    let market_key = *market.info.key;

    // Anything crossable at all? Cheap pre-check avoids the oracle reads
    // on the (common) empty-bids path.
    let (bids_count, fee_floor_bps, origination_bps, dmd, cmd) = {
        let market_data = market.info.try_borrow_data()?;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..fixed_size]);
        let dynamic = &market_data[fixed_size..];
        (
            crate::state::market_helpers::count_resting_bids(header, dynamic),
            header.fee_config.protocol_fee_bps_floor,
            header.fee_config.origination_bps,
            header.debt_mint_decimals,
            header.collateral_mint_decimals,
        )
    };
    if bids_count == 0 {
        return Ok(Default::default());
    }

    // Each fill consumes one MatchedLoan block — budget for the worst
    // case up front.
    super::shared::expand_market_to_free_blocks(fee_payer.info, market, bids_count)?;

    let debt_oracle_price_fp48 = crate::math::Fp48::from_raw(MarginfiV18Adapter.oracle_price(
        &crate::validation::oracle_price_args(debt_bank.info, debt_oracle_ais),
    )?);
    let collateral_oracle_price_fp48 = crate::math::Fp48::from_raw(
        MarginfiV18Adapter.oracle_price(&crate::validation::oracle_price_args(
            collateral_bank.info,
            collateral_oracle_ais,
        ))?,
    );
    let lender_debt_snapshot_fp48 = {
        let data = debt_bank.info.try_borrow_data()?;
        let bank = marginfi_mocks::state::Bank::try_from_account_data(&data)
            .map_err(|_| crate::program::YdeltaError::IncorrectAccount)?;
        crate::math::Fp48::from_raw(wrapped_i80f48_to_u128(bank.asset_share_value)?)
    };

    let market_data: &mut std::cell::RefMut<&mut [u8]> =
        &mut market.info.try_borrow_mut_data()?;
    let da = super::shared::get_mut_dynamic_account::<MarketFixed>(market_data);
    crate::state::market_helpers::match_resting_bids(
        da.fixed,
        da.dynamic,
        crate::state::market_helpers::MatchRestingBidsArgs {
            market_pubkey: market_key,
            ask_seat_index,
            ask_sub_vault_id,
            ask_rate_bps,
            ask_term_seconds,
            ask_curator_fee_bps,
            ask_curator,
            profile_max_ltv_bps,
            profile_liquidation_ltv_bps,
            fee_floor_bps,
            origination_bps,
            now_unix_ts: now,
            lender_debt_share_price_snapshot_fp48: lender_debt_snapshot_fp48,
            debt_oracle_price_fp48,
            collateral_oracle_price_fp48,
            debt_mint_decimals: dmd,
            collateral_mint_decimals: cmd,
            max_fills,
        },
        vault_ai,
    )
}
