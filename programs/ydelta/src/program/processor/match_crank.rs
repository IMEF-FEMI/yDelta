//! `MatchCrank` — permissionless backstop that crosses
//! resting bids against resting sub-vault asks. Crossability changes
//! without order flow (vault deposits / repayment claims replenish idle;
//! oracle moves flip LTV gates), so a crossed-at-rest book is legal and
//! anyone may resolve it. No keeper fee: curators deploying idle,
//! borrowers wanting fills, and UIs are the natural crankers.

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, HyperTreeValueIteratorTrait, RedBlackTreeReadOnly, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::logs::emit_stack;
use crate::state::vault::{get_helper_sub_vault, GlobalVaultFixed, SubVault, SubVaultTreeReadOnly};
use crate::state::{MarketFixed, RestingOrder, GLOBAL_VAULT_FIXED_SIZE};
use crate::validation::loaders::MatchCrankContext;

use super::place_order_for_sub_vault::take_resting_bids;

/// Crank parameters.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct MatchCrankParams {
    /// Fill budget for this call; 0 is treated as 1.
    pub max_fills: u32,
}

/// One resting ask, snapshotted for the crank walk.
struct AskSnapshot {
    seat_index: hypertree::DataIndex,
    sub_vault_id: u16,
    rate_bps: u16,
    term_seconds: u32,
}

/// Permissionless crank: walks the asks best-first and runs the take
/// path for each against the resting bids, within `max_fills`.
pub fn process_match_crank(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = MatchCrankParams::try_from_slice(data)?;
    let MatchCrankContext {
        payer,
        vault,
        market,
        debt_bank,
        marginfi_group,
        collateral_bank,
        debt_oracle_ais,
        collateral_oracle_ais,
    } = MatchCrankContext::load(accounts)?;

    let now: i64 = Clock::get()?.unix_timestamp;
    let mut budget: u32 = params.max_fills.max(1);

    // the floor applies on the crank path too — stale asks below
    // the live bank APR never fill.
    let floor_bps: u16 = crate::protocol::marginfi_rate_calc::current_lending_apr_bps_ceil(
        debt_bank.info,
        marginfi_group.info,
    )?;

    // Snapshot the resting asks (best-first) so the walk is stable while
    // the take path mutates the trees.
    let asks: Vec<AskSnapshot> = {
        let market_data = market.info.try_borrow_data()?;
        let fixed_size = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..fixed_size]);
        let dynamic = &market_data[fixed_size..];
        let tree: RedBlackTreeReadOnly<RestingOrder> = RedBlackTreeReadOnly::new(
            dynamic,
            header.asks_root_index,
            header.asks_best_index,
        );
        tree.iter::<RestingOrder>()
            .map(|(_, o)| {
                let seat = crate::state::market::get_helper_seat(dynamic, o.trader_seat_index)
                    .get_value();
                AskSnapshot {
                    seat_index: o.trader_seat_index,
                    sub_vault_id: seat.sub_vault_id,
                    rate_bps: o.rate_bps,
                    term_seconds: o.term_seconds,
                }
            })
            .collect()
    };

    let mut total_fills: u32 = 0;
    for ask in asks {
        if budget == 0 {
            break;
        }
        if ask.rate_bps < floor_bps {
            emit_stack(crate::logs::AskSkippedBelowFloorLog {
                market: *market.info.key,
                sub_vault_id: ask.sub_vault_id,
                ask_rate_bps: ask.rate_bps,
                floor_bps,
                _pad0: [0; 2],
                order_sequence: 0,
            })?;
            continue;
        }
        // Per-ask sub-vault policy reads (LTV pair + fee + curator for
        // the D9 self-cross skip); sunset/idle are re-checked live
        // inside the engine.
        let (max_ltv_bps, liquidation_ltv_bps, curator_fee_bps, ask_curator): (u16, u16, u16, Pubkey) = {
            let vault_data = vault.info.try_borrow_data()?;
            let (fixed_bytes, vault_dyn) = vault_data.split_at(GLOBAL_VAULT_FIXED_SIZE);
            let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
            let probe = SubVault::new_empty(ask.sub_vault_id, Pubkey::default(), 1, 1);
            let tree = SubVaultTreeReadOnly::new(vault_dyn, header.sub_vaults_root_index, NIL);
            let idx = tree.lookup_index(&probe);
            if idx == NIL {
                continue;
            }
            let p = get_helper_sub_vault(vault_dyn, idx).get_value();
            (p.max_ltv_bps, p.liquidation_ltv_bps, p.curator_fee_bps, p.curator)
        };

        let res = take_resting_bids(
            &market,
            vault.info,
            &debt_bank,
            &collateral_bank,
            &debt_oracle_ais,
            &collateral_oracle_ais,
            &payer,
            ask.seat_index,
            ask.sub_vault_id,
            ask.rate_bps,
            ask.term_seconds,
            curator_fee_bps,
            ask_curator,
            max_ltv_bps,
            liquidation_ltv_bps,
            now,
            budget,
        )?;
        total_fills = total_fills.saturating_add(res.num_fills);
        budget = budget.saturating_sub(res.num_fills);
    }

    emit_stack(crate::logs::MatchCrankLog {
        market: *market.info.key,
        cranker: *payer.info.key,
        fills: total_fills,
        _pad0: [0; 4],
    })?;
    Ok(())
}
