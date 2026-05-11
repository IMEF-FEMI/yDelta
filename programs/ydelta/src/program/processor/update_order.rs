//! Update a resting order via cancel-and-replace.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::DataIndex;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::logs::{emit_stack, UpdateOrderLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::{
    get_now_unix_ts,
    market::{get_helper_order, get_mut_helper_order},
    market_helpers::{
        cancel_order_by_index, get_seat_index_with_hint, lookup_order_by_seq, place_order_inner,
        PlaceOrderArgs,
    },
    MarketFixed, OrderKind, RestingOrder,
};
use crate::validation::loaders::OrderContext;

use super::shared::{expand_market_if_needed, get_mut_dynamic_account};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateOrderParams {
    pub order_sequence_number: u64,
    pub order_index_hint: Option<DataIndex>,
    pub seat_index_hint: Option<DataIndex>,
    pub new_rate_bps: Option<u16>,
    pub new_term_seconds: Option<u32>,
    pub new_principal_atoms: Option<u64>,
    pub new_collateral_atoms: Option<u64>,
    pub new_last_valid_unix_ts: Option<i64>,
    pub new_flags: Option<u8>,
    /// `SecondaryLoanSale` only. Adjust the seller's asking price for
    /// the same loan claim (rate / term / principal / collateral are
    /// loan-snapshots and may NOT be updated; doing so errors with
    /// `SecondaryFieldImmutable`). For primary orders this field is
    /// ignored.
    pub new_asking_price_atoms: Option<u64>,
}

pub fn process_update_order(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateOrderParams::try_from_slice(data)?;

    let OrderContext {
        payer,
        market,
        _system_program,
        user_account_ai,
        secondary_loan_ai: _,
    } = OrderContext::load(accounts)?;

    expand_market_if_needed(payer.info, &market)?;

    let market_key = *market.info.key;
    let now = get_now_unix_ts()?;

    let (old_sequence, new_sequence) = {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat_index =
            get_seat_index_with_hint(da.fixed, da.dynamic, payer.info.key, params.seat_index_hint)?;
        let order_index = lookup_order_by_seq(
            da.fixed,
            da.dynamic,
            seat_index,
            params.order_sequence_number,
            params.order_index_hint,
        )?;
        let snapshot: RestingOrder = *get_helper_order(da.dynamic, order_index).get_value();

        // SecondaryLoanSale: in-place mutation of the two mutable
        // fields (asking_price_atoms, last_valid_unix_ts). Reject any
        // mutation of rate/term/principal/collateral (those are
        // loan-snapshots). Sequence number and tree position are
        // preserved, so the seller does NOT lose their price-time
        // priority slot on a price change.
        if snapshot.kind == OrderKind::SecondaryLoanSale {
            require!(
                params.new_rate_bps.is_none(),
                YdeltaError::SecondaryFieldImmutable,
                "SecondaryLoanSale: rate is loan-snapshot, cannot be updated"
            )?;
            require!(
                params.new_term_seconds.is_none(),
                YdeltaError::SecondaryFieldImmutable,
                "SecondaryLoanSale: term is loan-snapshot, cannot be updated"
            )?;
            require!(
                params.new_principal_atoms.is_none(),
                YdeltaError::SecondaryFieldImmutable,
                "SecondaryLoanSale: principal is loan-snapshot, cannot be updated"
            )?;
            require!(
                params.new_collateral_atoms.is_none(),
                YdeltaError::SecondaryFieldImmutable,
                "SecondaryLoanSale: collateral_atoms must stay 0 on a secondary bid"
            )?;
            // Pricing is fixed at par; the user can't override
            // `asking_price_atoms` either.
            require!(
                params.new_asking_price_atoms.is_none(),
                YdeltaError::SecondaryFieldImmutable,
                "SecondaryLoanSale: asking_price is fixed at principal (par exit), \
                 not user-controllable"
            )?;

            // Secondary bids must stay non-expiring. Allow the caller
            // to "touch" the expiry only if the new value is also
            // NO_EXPIRATION — any non-zero value is rejected so the
            // matching engine's expiry-sweep can never remove a
            // secondary bid out from under its loan's
            // `has_resting_secondary_bid` flag.
            if let Some(new_lvt) = params.new_last_valid_unix_ts {
                require!(
                    new_lvt == crate::state::constants::NO_EXPIRATION_LAST_VALID_UNIX_TS,
                    YdeltaError::SecondaryFieldImmutable,
                    "SecondaryLoanSale: expiry is hardcoded to NO_EXPIRATION; \
                     cancel the bid via cancel_order to remove it"
                )?;
            }
            let order_mut = get_mut_helper_order(da.dynamic, order_index).get_mut_value();
            if let Some(new_flags) = params.new_flags {
                order_mut.flags = new_flags;
            }
            // sequence_number unchanged on in-place updates.
            (snapshot.sequence_number, snapshot.sequence_number)
        } else {
            // **Safety gate:** `update_order`'s `OrderContext`
            // doesn't carry oracle/bank accounts, so we cannot run
            // the LTV check that `place_order` runs. To preserve the
            // LTV invariant from the original placement, reject any
            // update that mutates `principal_atoms` or
            // `collateral_atoms` for a primary order. Users who need
            // to change those must cancel and re-place via
            // `place_order` (which has the full LTV gate). Rate /
            // term / expiry / flags changes are still allowed since
            // they don't change the principal-to-collateral ratio
            // that the LTV check guards.
            require!(
                params.new_principal_atoms.is_none(),
                YdeltaError::InvalidArgument,
                "update_order: changing principal_atoms requires cancel + place_order \
                 (no LTV gate available in update path)"
            )?;
            require!(
                params.new_collateral_atoms.is_none(),
                YdeltaError::InvalidArgument,
                "update_order: changing collateral_atoms requires cancel + place_order \
                 (no LTV gate available in update path)"
            )?;
            // Changing rate on a Bid would let a borrower place at a
            // safe LTV, wait for oracle drift, then update the rate
            // to match against vault makers whose max_ltv was sized
            // for the ORIGINAL safer LTV — bypassing the
            // current-oracle LTV gate that place_order would run.
            // Force cancel + re-place (via the user's own
            // place_order ix, which loads oracles and runs the gate
            // against current prices) for any rate change — both
            // sides. The Ask-side concern: a wallet Ask re-rated
            // downward could cross resting Bids that have become
            // undercollateralized since their original placement;
            // their LTV must re-check against current oracles, not
            // the borrower's place-time snapshot.
            require!(
                params.new_rate_bps.is_none(),
                YdeltaError::InvalidArgument,
                "update_order: changing rate_bps requires cancel + \
                 place_order so LTV re-checks against current oracles"
            )?;

            // update_order is primary-only — the snapshot.kind check
            // upstream rejects secondary orders, so the return value
            // (None for primary) is intentionally discarded.
            let _ = cancel_order_by_index(da.fixed, da.dynamic, seat_index, order_index)?;

            let new_args = PlaceOrderArgs {
                market_pubkey: market_key,
                taker_seat_index: seat_index,
                side: snapshot.side,
                kind: OrderKind::Primary,
                order_type: snapshot.order_type,
                rate_bps: params.new_rate_bps.unwrap_or(snapshot.rate_bps),
                term_seconds: params.new_term_seconds.unwrap_or(snapshot.term_seconds),
                // principal/collateral are forced to the snapshot
                // values per the safety gate above.
                principal_atoms: snapshot.principal_atoms,
                collateral_atoms: snapshot.collateral_atoms,
                last_valid_unix_ts: params
                    .new_last_valid_unix_ts
                    .unwrap_or(snapshot.last_valid_unix_ts),
                flags: params.new_flags.unwrap_or(snapshot.flags),
                now_unix_ts: now,
                // Reuse the order's original snapshot since principal/
                // collateral are unchanged — encumber math is identical.
                share_price_snapshot_fp48: snapshot.share_price_snapshot(),
                // No LTV gate needed: principal/collateral ratio is
                // unchanged from the original placement which already
                // passed the gate.
                debt_oracle_price_fp48: 0,
                collateral_oracle_price_fp48: 0,
                debt_liability_weight_init_fp48: 0,
                collateral_asset_weight_init_fp48: 0,
                enforce_ltv: false,
                is_vault_lender: false,
                // Carry the original bid's borrower_ltv forward — the
                // updated order represents the same position. Asks have
                // 0 (gate is no-op).
                borrower_ltv_bps: snapshot.borrower_ltv_bps,
            };

            // update_order's account list doesn't carry the vault
            // account; the safety gate above forces principal/
            // collateral unchanged so a re-place can't trigger fresh
            // vault matching at scale. None is fine — the
            // risk-profile gate becomes a no-op for non-vault
            // matches.
            let result = place_order_inner(da.fixed, da.dynamic, new_args, None)?;
            (snapshot.sequence_number, result.sequence)
        }
    };

    emit_stack(UpdateOrderLog {
        market: market_key,
        trader: *payer.info.key,
        old_sequence,
        new_sequence,
    })?;

    // Sync the signer's MarketPosition mirror after the
    // cancel-and-replace (or in-place asking-price tweak) reshapes
    // their seat encumbrances.
    super::shared::sync_signer_market_position(market.info, user_account_ai, payer.info.key)?;
    Ok(())
}
