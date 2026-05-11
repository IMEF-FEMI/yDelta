//! Finalize a `MatchedLoan` queue node into live loan state.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{is_not_nil, DataIndex, HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanPromotedLog, LoanTransferredLog, SecondaryStaleBidDroppedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{LoanFixed, LoanType, LOAN_FIXED_SIZE, LOAN_SEED};
use crate::state::market::{
    get_helper_matched_loan, MarketFixed, MatchedLoan, MATCHED_LOAN_FLAG_SECONDARY_SPLIT,
};
use crate::state::market_helpers::release_address_on_market_fixed;
use crate::utils::create_account;
use crate::validation::loaders::{ProcessMatchedLoanContext, ProcessMatchedLoanMode};

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct ProcessMatchedLoanParams {
    pub matched_loan_sequence: u64,
    /// Optional hint to skip the tree walk if the cranker already knows
    /// the node's index in the dynamic region.
    pub matched_loan_index_hint: Option<DataIndex>,
}

pub fn process_process_matched_loan(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = ProcessMatchedLoanParams::try_from_slice(data)?;
    let ctx = ProcessMatchedLoanContext::load(accounts, params.matched_loan_sequence)?;

    match ctx.mode {
        ProcessMatchedLoanMode::Primary => process_primary_promotion(program_id, ctx),
        ProcessMatchedLoanMode::SecondaryFull | ProcessMatchedLoanMode::SecondarySplit => {
            process_secondary_finalize(program_id, ctx)
        }
    }
}

// ────────────────── Primary cross ──────────────────

fn process_primary_promotion(program_id: &Pubkey, ctx: ProcessMatchedLoanContext) -> ProgramResult {
    let ProcessMatchedLoanContext {
        payer,
        market,
        loan,
        loan_bump,
        debt_bank,
        system_program,
        queue_node: node,
        queue_node_index: node_index,
        vault_settle,
        ..
    } = ctx;
    let loan = loan.expect("primary mode → empty loan PDA must be passed");
    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    let loan_type = LoanType::from_u8(node.loan_type)?;
    let net_principal: u64 = node
        .principal_atoms
        .checked_sub(node.origination_atoms)
        .ok_or(YdeltaError::InvalidArgument)?;

    let seq_le = node.sequence.to_le_bytes();
    let bump_arr = [loan_bump];
    let seeds: Vec<Vec<u8>> = vec![
        LOAN_SEED.to_vec(),
        market_key.to_bytes().to_vec(),
        seq_le.to_vec(),
        bump_arr.to_vec(),
    ];
    create_account(
        payer.info,
        loan.info,
        system_program.info,
        program_id,
        &Rent::get()?,
        LOAN_FIXED_SIZE as u64,
        seeds,
    )?;

    // Read the lender's `ClaimedSeat` to determine whether the lender
    // is a wallet (User) or a vault (GlobalVault). For vault lenders,
    // capture the vault PDA + profile_id; the `LoanFixed` fields
    // (`lender_kind`, `lender_profile_id`, `lender_global_vault`) carry
    // these forward so the vault branch of `process_repay` can settle
    // correctly.
    let (lender_kind, lender_profile_id, lender_global_vault): (u8, u8, Pubkey) =
        if node.lender_seat_index == NIL {
            // P2Pool loans have no human lender — `lender_seat_index = NIL`.
            // Stamp wallet-defaults; the loan is funded by marginfi directly.
            (0u8, 0u8, Pubkey::default())
        } else {
            let market_data = market.info.try_borrow_data()?;
            let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
            let seat =
                crate::state::market::get_helper_seat(dynamic, node.lender_seat_index).get_value();
            // For wallet lenders, normalize `lender_global_vault` to
            // `Pubkey::default()` so off-chain consumers can use
            // `lender_kind` as the sole branch.
            let kind = seat.owner_kind;
            let pid = seat.risk_profile_id;
            let v = if kind == crate::state::OWNER_KIND_RISK_PROFILE {
                seat.owner
            } else {
                Pubkey::default()
            };
            (kind, pid, v)
        };

    // Snapshot the market's curator_fee_bps. Stamped only on
    // vault-funded loans so wallet loans don't accrue against a config
    // that has no curator counterparty.
    let curator_fee_bps_snapshot: u16 = if lender_kind == crate::state::OWNER_KIND_RISK_PROFILE {
        market.get_fixed()?.fee_config.curator_fee_bps
    } else {
        0
    };

    let loan_fixed = LoanFixed::new_from_matched_loan_with_lender(
        market_key,
        node.sequence,
        loan_bump,
        *payer.info.key,
        node.lender_seat_index,
        node.borrower_seat_index,
        node.principal_atoms,
        net_principal,
        node.collateral_atoms,
        node.borrower_rate_bps,
        node.lender_rate_bps,
        node.term_seconds,
        node.matched_at_unix,
        node.flags,
        loan_type,
        node.borrower_marginfi_borrow_shares,
        lender_kind,
        lender_profile_id,
        lender_global_vault,
        curator_fee_bps_snapshot,
        node.lender_debt_share_price_snapshot_fp48,
        node.borrower_collateral_share_price_snapshot_fp48,
    );
    {
        let loan_data: &mut RefMut<&mut [u8]> = &mut loan.info.try_borrow_mut_data()?;
        let dst: &mut [u8] = &mut loan_data[..LOAN_FIXED_SIZE];
        dst.copy_from_slice(bytemuck::bytes_of(&loan_fixed));
    }

    let credited_shares: u128 = match loan_type {
        LoanType::Fixed => {
            MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], net_principal)?
        }
        LoanType::P2Pool => 0,
    };
    // P2Pool loans pay marginfi's full borrow APR — there's no
    // orderbook spread for the protocol to capture, so no origination
    // fee. Skip the share computation entirely on P2Pool to save CU
    // and avoid bumping `accumulated_protocol_fee_shares`.
    let origination_shares: u128 = if loan_type == LoanType::P2Pool {
        0
    } else if node.origination_atoms > 0 {
        MarginfiV18Adapter
            .amount_to_asset_shares(&[debt_bank.info.clone()], node.origination_atoms)?
    } else {
        0
    };

    // Risk-profile lenders: 3-CPI atom migration moves the loan's full
    // gross principal from `global_vault.integration` to
    // `market.lender_integration_account` (vault.integration →
    // global_vault_staging → market_debt_vault → market.lender_integration).
    // The full gross is migrated so origination-fee shares accumulate
    // uniformly on `market.accumulated_protocol_fee_shares` regardless
    // of lender kind. RiskProfile aggregates also update here so
    // `accrue_risk_profile` credits depositor yield on the now-deployed
    // principal.
    if lender_kind == crate::state::OWNER_KIND_RISK_PROFILE && loan_type == LoanType::Fixed {
        let vault_settle = vault_settle
            .as_ref()
            .ok_or_else(|| {
                solana_program::msg!(
                    "process_matched_loan: risk-profile lender match requires vault settlement accounts"
                );
                YdeltaError::IncorrectAccount
            })?;
        do_vault_settle(
            node.principal_atoms,
            &debt_bank,
            vault_settle,
            node.lender_rate_bps,
            lender_profile_id,
            market_key,
            now_unix_ts,
        )?;
    }

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);

        if loan_type == LoanType::Fixed && credited_shares > 0 {
            da.deposit_to_seat(
                node.borrower_seat_index,
                credited_shares,
                /*is_debt=*/ true,
            )?;
        }

        da.fixed.accumulated_protocol_fee_shares = da
            .fixed
            .accumulated_protocol_fee_shares
            .checked_add(origination_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        let mut tree = crate::state::market::MatchedLoanTree::new(
            da.dynamic,
            da.fixed.matched_loans_root_index,
            NIL,
        );
        tree.remove_by_index(node_index);
        da.fixed.matched_loans_root_index = tree.get_root_index();
        drop(tree);
        release_address_on_market_fixed(da.fixed, da.dynamic, node_index);
    }

    emit_stack(LoanPromotedLog {
        market: market_key,
        loan: *loan.info.key,
        sequence: node.sequence,
        cranker: *payer.info.key,
        principal_atoms: node.principal_atoms,
        net_principal_atoms: net_principal,
        origination_atoms: node.origination_atoms,
        credited_shares,
        origination_shares,
    })?;

    Ok(())
}

// ────────────────── Secondary cross ──────────────────

/// Reasons a secondary queue node is dropped without finalizing the
/// transfer. Encoded into `SecondaryStaleBidDroppedLog.reason`.
const STALE_REASON_LOAN_REPAID: u8 = 1;
const STALE_REASON_LOAN_NOT_FIXED: u8 = 2;
// Reason code 3 is reserved for stable ABI.

fn process_secondary_finalize(
    program_id: &Pubkey,
    ctx: ProcessMatchedLoanContext,
) -> ProgramResult {
    let _ = program_id;
    // Validate optional accounts presence. Loader has already asserted
    // these are present for SecondaryFull/SecondarySplit; unwrap safe.
    let existing_loan = ctx
        .existing_loan
        .as_ref()
        .expect("secondary mode → existing_loan must be passed");
    let collateral_bank = ctx
        .collateral_bank
        .as_ref()
        .expect("secondary mode → collateral_bank must be passed");
    // Oracle accounts are still loaded but unused here — the LTV check
    // fires at `place_secondary_bid`. Borrow rather than move so `ctx`
    // stays usable downstream (drop_stale_secondary takes `ctx` by value).
    let _ = (&ctx.debt_oracle_ais, &ctx.collateral_oracle_ais);
    let _ = collateral_bank;

    // Read the existing loan + run staleness checks. On any
    // staleness/LTV failure → refund-and-drop, return Ok.
    let (loan_pre_state, can_finalize) = {
        let l = existing_loan.get_fixed()?;
        if l.outstanding_debt_atoms == 0 {
            (None, false)
        } else if l.loan_type != LoanType::Fixed as u8 {
            // Refinanced to P2Pool between match and crank.
            (None, false)
        } else {
            (
                Some((
                    l.principal_debt_atoms,
                    l.outstanding_debt_atoms,
                    l.lender_claimable_atoms,
                    l.collateral_atoms,
                    l.borrower_seat_index,
                    l.borrower_rate_bps,
                    l.lender_rate_bps,
                    l.matures_at_unix,
                )),
                true,
            )
        }
    };
    if !can_finalize {
        let reason = {
            let l = existing_loan.get_fixed()?;
            if l.outstanding_debt_atoms == 0 {
                STALE_REASON_LOAN_REPAID
            } else {
                STALE_REASON_LOAN_NOT_FIXED
            }
        };
        return drop_stale_secondary(ctx, reason);
    }
    let (
        pre_principal,
        pre_outstanding,
        pre_lender_claimable,
        pre_collateral,
        borrower_seat_index,
        loan_borrower_rate_bps,
        _loan_lender_rate_bps_old,
        _matures_at_unix,
    ) = loan_pre_state.unwrap();

    let market_key = *ctx.market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;
    let _ = now_unix_ts;

    let node = ctx.queue_node;
    let is_split = (node.flags & MATCHED_LOAN_FLAG_SECONDARY_SPLIT) != 0;

    // Compute split portions (full transfer ⇒ pass-through). Pro-rata
    // math: floor on new sub-loan, residual stays with original.
    let matched_principal = node.principal_atoms;
    let cash_paid = node.cash_paid_atoms;
    let new_collateral: u64 = if is_split {
        let raw = (pre_collateral as u128)
            .checked_mul(matched_principal as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / (pre_principal as u128);
        u64::try_from(raw).map_err(|_| ProgramError::ArithmeticOverflow)?
    } else {
        pre_collateral
    };
    let new_outstanding: u64 = if is_split {
        let raw = (pre_outstanding as u128)
            .checked_mul(matched_principal as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / (pre_principal as u128);
        u64::try_from(raw).map_err(|_| ProgramError::ArithmeticOverflow)?
    } else {
        pre_outstanding
    };
    let new_lender_claimable_seized: u64 = if is_split {
        // Proportional seizure on partial transfer; remainder stays
        // with the seller's portion of the loan (unforfeited).
        let raw = (pre_lender_claimable as u128)
            .checked_mul(matched_principal as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?
            / (pre_principal as u128);
        u64::try_from(raw).map_err(|_| ProgramError::ArithmeticOverflow)?
    } else {
        pre_lender_claimable
    };

    // Cash settlement on the seats. New lender's encumbered →
    // seller's withdrawable. Cash_paid worth of fp48 shares (against
    // the live debt bank).
    let cash_paid_shares: u128 =
        MarginfiV18Adapter.amount_to_asset_shares(&[ctx.debt_bank.info.clone()], cash_paid)?;
    let seized_shares: u128 = if new_lender_claimable_seized > 0 {
        MarginfiV18Adapter
            .amount_to_asset_shares(&[ctx.debt_bank.info.clone()], new_lender_claimable_seized)?
    } else {
        0
    };

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut ctx.market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);

        // Credit the seller's withdrawable. The new lender's
        // encumbered share-debit was ALREADY applied at match time
        // (see `decrement_encumbrance_on_match` in the matching
        // engine's secondary branch); doing it again here would be a
        // double-debit. The cranker only handles the receive-side
        // credit + the loan/queue mutations.
        da.deposit_to_seat(
            node.lender_seat_index,
            cash_paid_shares,
            /*is_debt=*/ true,
        )?;

        // Seize accrued (proportional to matched portion) into protocol
        // fee accumulator.
        da.fixed.accumulated_protocol_fee_shares = da
            .fixed
            .accumulated_protocol_fee_shares
            .checked_add(seized_shares)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // Free the queue slot.
        let mut tree = crate::state::market::MatchedLoanTree::new(
            da.dynamic,
            da.fixed.matched_loans_root_index,
            NIL,
        );
        tree.remove_by_index(ctx.queue_node_index);
        da.fixed.matched_loans_root_index = tree.get_root_index();
        drop(tree);
        release_address_on_market_fixed(da.fixed, da.dynamic, ctx.queue_node_index);
    }

    // Mutate the existing loan + (optionally) allocate the split
    // sub-loan PDA.
    let new_loan_pda_for_log: Pubkey;
    if is_split {
        let split_loan = ctx
            .split_loan_to_create
            .as_ref()
            .expect("SecondarySplit → split_loan_to_create must be passed");
        // Derive the split sub-loan's PDA from the queue node's
        // STABLE `sequence` (assigned at match time, immutable across
        // the node's lifetime), not from the live
        // `market.matched_loan_sequence` counter. The latter bumps on
        // every primary cross, so a concurrent place_order between
        // cranker derivation and tx execution would invalidate the
        // pre-derived PDA. The distinct seed prefix
        // (`SPLIT_LOAN_SEED`) keeps the seed space from colliding
        // with primary loans.
        use crate::state::loan::{split_loan_pda, SPLIT_LOAN_SEED};
        let stable_seq: u64 = node.sequence;
        let (_expected, split_bump) = split_loan_pda(&market_key, stable_seq);
        let seq_le = stable_seq.to_le_bytes();
        let bump_arr = [split_bump];
        let seeds: Vec<Vec<u8>> = vec![
            SPLIT_LOAN_SEED.to_vec(),
            market_key.to_bytes().to_vec(),
            seq_le.to_vec(),
            bump_arr.to_vec(),
        ];
        create_account(
            ctx.payer.info,
            split_loan.info,
            ctx.system_program.info,
            program_id,
            &Rent::get()?,
            LOAN_FIXED_SIZE as u64,
            seeds,
        )?;

        // Build via the public constructor, then patch fields that
        // differ for a split sub-loan: matures_at_unix carries over
        // from the source loan; outstanding_debt_atoms is the
        // pro-rata slice (vs the constructor's `net_principal`);
        // lender_claimable_atoms resets to 0 (Option A); started /
        // last_accrued line up with the matched_at_unix.
        // Borrower's collateral encumbrance was set at the ORIGINAL
        // loan's place time and stays put for the split. Carry the
        // original snapshot so the eventual settle/liquidate decrement
        // is byte-symmetric. Lender side is the secondary taker — its
        // snapshot was sampled at their place-secondary time and lives
        // on the queue node.
        let (orig_borrower_snapshot, orig_lender_snapshot_fallback) = {
            let l = existing_loan.get_fixed()?;
            (
                l.borrower_collateral_share_price_snapshot_fp48,
                l.lender_debt_share_price_snapshot_fp48,
            )
        };
        let new_lender_snapshot = if node.lender_debt_share_price_snapshot_fp48 != 0 {
            node.lender_debt_share_price_snapshot_fp48
        } else {
            orig_lender_snapshot_fallback
        };
        let mut sub = LoanFixed::new_from_matched_loan(
            market_key,
            stable_seq,
            split_bump,
            *ctx.payer.info.key,
            node.new_lender_seat_index,
            borrower_seat_index,
            matched_principal,
            /*net_principal=*/ new_outstanding,
            new_collateral,
            loan_borrower_rate_bps,
            node.lender_rate_bps,
            /*term_seconds=*/ 0, // recomputed below from matures_at_unix
            node.matched_at_unix,
            /*flags=*/ 0,
            LoanType::Fixed,
            /*borrower_marginfi_borrow_shares=*/ 0,
            new_lender_snapshot,
            orig_borrower_snapshot,
        );
        sub.lender_claimable_atoms = 0; // Option A — fresh start.
        sub.matures_at_unix = {
            let l = existing_loan.get_fixed()?;
            l.matures_at_unix
        };
        {
            let data: &mut RefMut<&mut [u8]> = &mut split_loan.info.try_borrow_mut_data()?;
            let dst: &mut [u8] = &mut data[..LOAN_FIXED_SIZE];
            dst.copy_from_slice(bytemuck::bytes_of(&sub));
        }
        // Splits no longer consume `matched_loan_sequence` (they use
        // the queue node's stable `node.sequence` under
        // SPLIT_LOAN_SEED), so no bump here. The counter is reserved
        // for primary loan allocations elsewhere.
        new_loan_pda_for_log = *split_loan.info.key;

        // Mutate the original loan to keep the seller's portion.
        let mut data: RefMut<&mut [u8]> = existing_loan.info.try_borrow_mut_data()?;
        let dst: &mut [u8] = &mut data[..LOAN_FIXED_SIZE];
        let l: &mut LoanFixed = bytemuck::from_bytes_mut(dst);
        l.principal_debt_atoms = pre_principal
            .checked_sub(matched_principal)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        l.outstanding_debt_atoms = pre_outstanding
            .checked_sub(new_outstanding)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        l.collateral_atoms = pre_collateral
            .checked_sub(new_collateral)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        l.lender_claimable_atoms = pre_lender_claimable
            .checked_sub(new_lender_claimable_seized)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // last_accrued_unix stays — seller's portion didn't reset.
        // NOTE: in the SPLIT case the matching engine left the
        // secondary bid resting (with reduced principal_atoms), so
        // `has_resting_secondary_bid` MUST stay 1 — there's still a
        // resting bid for `existing_loan`. Only the full-transfer
        // branch below clears the flag.
    } else {
        new_loan_pda_for_log = Pubkey::default();
        // Full-transfer reset. Read the new lender's seat to stamp the
        // loan's lender_kind / lender_global_vault / lender_profile_id —
        // mirrors the primary promotion path so subsequent settlement
        // ixs can detect risk-profile vs wallet ownership uniformly.
        let (new_lender_kind, new_lender_profile_id, new_lender_global_vault): (u8, u8, Pubkey) = {
            let market_data = ctx.market.info.try_borrow_data()?;
            let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
            let seat = crate::state::market::get_helper_seat(dynamic, node.new_lender_seat_index)
                .get_value();
            let kind = seat.owner_kind;
            let pid = seat.risk_profile_id;
            let v = if kind == crate::state::OWNER_KIND_RISK_PROFILE {
                seat.owner
            } else {
                Pubkey::default()
            };
            (kind, pid, v)
        };

        // Read the new curator_fee_bps snapshot from the market BEFORE
        // the mutable borrow on existing_loan below, since `ctx.market`
        // is a sibling refcell.
        let new_curator_fee_bps_snapshot: u16 =
            if new_lender_kind == crate::state::OWNER_KIND_RISK_PROFILE {
                ctx.market.get_fixed()?.fee_config.curator_fee_bps
            } else {
                0
            };

        let mut data: RefMut<&mut [u8]> = existing_loan.info.try_borrow_mut_data()?;
        let dst: &mut [u8] = &mut data[..LOAN_FIXED_SIZE];
        let l: &mut LoanFixed = bytemuck::from_bytes_mut(dst);
        l.lender_seat_index = node.new_lender_seat_index;
        l.lender_rate_bps = node.lender_rate_bps;
        l.lender_claimable_atoms = 0;
        l.last_accrued_unix = node.matched_at_unix;
        l.lender_kind = new_lender_kind;
        l.lender_profile_id = new_lender_profile_id;
        l.lender_global_vault = new_lender_global_vault;
        l.has_resting_secondary_bid = 0;
        // Curator-fee snapshot follows the lender. Wallet → 0; vault →
        // the market's current `curator_fee_bps`. This locks the new
        // owner's manager-fee rate for accruals going forward.
        l.curator_fee_bps_snapshot = new_curator_fee_bps_snapshot;
        // Reset the old curator's accrued fee. Same rationale as
        // `lender_claimable_atoms = 0` above: secondary cross transfers
        // the loan to a new lender, and the old lender (and their
        // curator) was paid out as cash_paid by the buyer. Residual
        // accruals on the body don't belong to anyone post-transfer.
        l.accumulated_curator_fee_atoms = 0;
        // Lender side changed — adopt the new lender's snapshot so
        // their later `claim_repayment` decrement uses the same fp48
        // share count that was added to their seat at place-time.
        // Borrower-side snapshot stays untouched (collateral
        // encumbrance is unchanged).
        if node.lender_debt_share_price_snapshot_fp48 != 0 {
            l.lender_debt_share_price_snapshot_fp48 = node.lender_debt_share_price_snapshot_fp48;
        }
    }

    // If the new lender is a risk-profile, migrate cash_paid atoms from
    // `vault.integration` into `market.lender_integration_account` so
    // the seller's just-credited withdrawable shares are physically
    // backed. Risk-profile-as-new-lender is full-transfer only for v1
    // (split adds bookkeeping for two vault-funded slices simultaneously
    // and is deferred). Wallet new lender path is a no-op here — its
    // encumbered shares were debited at match time.
    if let Some(vault_settle) = ctx.vault_settle.as_ref() {
        require!(
            !is_split,
            YdeltaError::InvalidArgument,
            "secondary cross to risk-profile new lender does not yet support split — \
             full-transfer only in v1"
        )?;
        let new_lender_profile_id: u8 = {
            let market_data = ctx.market.info.try_borrow_data()?;
            let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];
            crate::state::market::get_helper_seat(dynamic, node.new_lender_seat_index)
                .get_value()
                .risk_profile_id
        };
        do_vault_settle(
            cash_paid,
            &ctx.debt_bank,
            vault_settle,
            node.lender_rate_bps,
            new_lender_profile_id,
            market_key,
            now_unix_ts,
        )?;
    }

    emit_stack(LoanTransferredLog {
        market: market_key,
        loan_pda: *existing_loan.info.key,
        new_loan_pda: new_loan_pda_for_log,
        old_lender_seat_index: node.lender_seat_index,
        new_lender_seat_index: node.new_lender_seat_index,
        matched_principal_atoms: matched_principal,
        cash_paid_atoms: cash_paid,
        seized_accrued_atoms: new_lender_claimable_seized,
        new_lender_rate_bps: node.lender_rate_bps,
        was_split: if is_split { 1 } else { 0 },
        _padding: [0; 5],
    })?;

    Ok(())
}

/// Refund the new lender's encumbered cash → withdrawable shares,
/// drop the queue node, emit a stale-bid log. Used when the loan has
/// been repaid, refinanced to P2Pool, or fails the cranker LTV
/// recheck.
fn drop_stale_secondary(ctx: ProcessMatchedLoanContext, reason: u8) -> ProgramResult {
    let market_key = *ctx.market.info.key;
    let node = ctx.queue_node;

    // Compute share-equivalent of cash_paid against the live debt bank.
    let refund_shares: u128 = if node.cash_paid_atoms > 0 {
        MarginfiV18Adapter
            .amount_to_asset_shares(&[ctx.debt_bank.info.clone()], node.cash_paid_atoms)?
    } else {
        0
    };

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut ctx.market.info.try_borrow_mut_data()?;
        let mut da = get_mut_dynamic_account::<MarketFixed>(market_data);

        if refund_shares > 0 {
            // The new lender's encumbered shares were already debited
            // at match time. Refunding the queue node = credit those
            // shares back to withdrawable (no debit needed, since we
            // never re-encumbered).
            da.deposit_to_seat(
                node.new_lender_seat_index,
                refund_shares,
                /*is_debt=*/ true,
            )?;
        }

        let mut tree = crate::state::market::MatchedLoanTree::new(
            da.dynamic,
            da.fixed.matched_loans_root_index,
            NIL,
        );
        tree.remove_by_index(ctx.queue_node_index);
        da.fixed.matched_loans_root_index = tree.get_root_index();
        drop(tree);
        release_address_on_market_fixed(da.fixed, da.dynamic, ctx.queue_node_index);
    }

    let loan_pda = ctx
        .existing_loan
        .as_ref()
        .map(|l| *l.info.key)
        .unwrap_or_default();

    emit_stack(SecondaryStaleBidDroppedLog {
        market: market_key,
        loan_pda,
        bid_sequence: node.sequence,
        seller_seat_index: node.lender_seat_index,
        swept_by: reason,
        _padding: [0; 3],
    })?;

    Ok(())
}

/// Look up a `MatchedLoan` node by sequence. Try the hint first; if it
/// validates, return immediately. Otherwise fall back to a tree scan.
/// (Kept for callers outside the cranker — the cranker itself uses the
/// loader's pre-resolved `queue_node_index`.)
#[allow(dead_code)]
fn lookup_matched_loan(
    fixed: &MarketFixed,
    dynamic: &[u8],
    sequence: u64,
    hint: Option<DataIndex>,
) -> Result<DataIndex, ProgramError> {
    use hypertree::HyperTreeReadOperations;
    if let Some(hint_idx) = hint {
        if hint_idx != NIL {
            let node = get_helper_matched_loan(dynamic, hint_idx).get_value();
            if node.sequence == sequence {
                return Ok(hint_idx);
            }
        }
    }
    let tree = crate::state::market::MatchedLoanTreeReadOnly::new(
        dynamic,
        fixed.matched_loans_root_index,
        NIL,
    );
    let mut probe = MatchedLoan::default();
    probe.sequence = sequence;
    Ok(tree.lookup_index(&probe))
}

#[allow(dead_code)]
fn assert_unused_helpers(idx: DataIndex) {
    let _ = is_not_nil!(idx);
}

// ─────────────────── Vault match settlement ───────────────────

/// Settle a vault-funded primary match.
///
/// Two effects:
/// 1. **Atom migration** (3 CPIs): atoms move from the vault's
///    marginfi account to the market's lender-side marginfi account
///    so the borrower's `debt_withdrawable_shares` corresponds to
///    real shares that `process_withdraw` can convert to atoms.
/// 2. **Vault state aggregate updates**:
///    `RiskProfile.deployed_principal_atoms` grows by the matched
///    principal, `total_weighted_rate_bps` grows by `(principal ×
///    lender_rate_bps)`. The market-side `ClaimedSeat`'s
///    `deployed_atoms()` bumps too (the vault-funded seat lives on
///    the market alongside user seats). Lender-rate yield will accrue
///    correctly via `accrue_risk_profile` from this point on.
#[allow(clippy::too_many_arguments)]
fn do_vault_settle<'a, 'info>(
    principal_atoms: u64,
    debt_bank: &'a crate::validation::MarginfiBankInfo<'a, 'info>,
    settle: &'a crate::validation::loaders::VaultSettleAccounts<'a, 'info>,
    lender_rate_bps: u16,
    profile_id: u8,
    market_key: Pubkey,
    now_unix_ts: i64,
) -> ProgramResult {
    if principal_atoms == 0 {
        return Ok(());
    }

    let vault_key = *settle.vault.info.key;
    let vault_bytes = vault_key.to_bytes();
    let vault_signer_bump_arr = [settle.global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] = &[
        crate::state::vault::GLOBAL_VAULT_SIGNER_SEED,
        &vault_bytes,
        &vault_signer_bump_arr,
    ];

    let market_bytes = market_key.to_bytes();
    let market_signer_bump_arr = [settle.market_signer_bump];
    let market_signer_seeds: &[&[u8]] = &[
        crate::validation::MARKET_SIGNER_SEED,
        &market_bytes,
        &market_signer_bump_arr,
    ];

    // ─── CPI 1: marginfi.withdraw — vault.integration_account → global_vault_staging ───
    let expected_shares: u128 =
        MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], principal_atoms)?;
    let withdraw_accounts: Vec<AccountInfo> = vec![
        settle.marginfi_group.info.clone(),
        settle.global_vault_integration_account.info.clone(),
        settle.global_vault_signer.clone(),
        debt_bank.info.clone(),
        settle.global_vault_staging.info.clone(),
        settle.bank_liquidity_vault_authority.clone(),
        settle.liquidity_vault.info.clone(),
        settle.token_program.info.clone(),
        settle.marginfi_program.info.clone(),
        // Active-balance health-check pair (vault has only one).
        debt_bank.info.clone(),
        settle.bank_oracle.clone(),
    ];
    // marginfi.withdraw can return `principal_atoms` ± 1 due to
    // share-value rounding. Downstream CPIs must use the actual value
    // landed in global_vault_staging, otherwise the SPL transfer hits
    // InsufficientFunds when withdraw underpaid by 1 atom.
    let actual_atoms = MarginfiV18Adapter.withdraw(
        &withdraw_accounts,
        expected_shares,
        &[global_vault_signer_seeds],
    )?;

    // ─── CPI 2: SPL transfer — global_vault_staging → market_debt_vault ───
    // Signed by global_vault_signer (global_vault_staging owner). Token-2022 path
    // uses transfer_checked; legacy spl-token uses transfer.
    if settle.token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            settle.token_program.info.key,
            settle.global_vault_staging.info.key,
            settle.mint.info.key,
            settle.market_debt_vault.info.key,
            settle.global_vault_signer.key,
            &[],
            actual_atoms,
            settle.mint.mint.decimals,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                settle.global_vault_staging.info.clone(),
                settle.mint.info.clone(),
                settle.market_debt_vault.info.clone(),
                settle.global_vault_signer.clone(),
                settle.token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    } else {
        let ix = spl_token::instruction::transfer(
            settle.token_program.info.key,
            settle.global_vault_staging.info.key,
            settle.market_debt_vault.info.key,
            settle.global_vault_signer.key,
            &[],
            actual_atoms,
        )?;
        solana_program::program::invoke_signed(
            &ix,
            &[
                settle.global_vault_staging.info.clone(),
                settle.market_debt_vault.info.clone(),
                settle.global_vault_signer.clone(),
                settle.token_program.info.clone(),
            ],
            &[global_vault_signer_seeds],
        )?;
    }

    // ─── CPI 3: marginfi.deposit — market_debt_vault → market.lender_integration_account ───
    // Adapter expected account ordering (per `protocol/marginfi.rs` deposit):
    //   [0] marginfi_group, [1] marginfi_account (destination),
    //   [2] authority (signer), [3] bank, [4] signer_token_account,
    //   [5] liquidity_vault, [6] token_program, [7] marginfi_program.
    let deposit_accounts: Vec<AccountInfo> = vec![
        settle.marginfi_group.info.clone(),
        settle.market_lender_integration_account.info.clone(),
        settle.market_signer.clone(),
        debt_bank.info.clone(),
        settle.market_debt_vault.info.clone(),
        settle.liquidity_vault.info.clone(),
        settle.token_program.info.clone(),
        settle.marginfi_program.info.clone(),
    ];
    let _credited: u128 =
        MarginfiV18Adapter.deposit(&deposit_accounts, actual_atoms, &[market_signer_seeds])?;

    // ─── State aggregate updates on the vault profile + seat ───
    {
        use crate::state::vault::{
            get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
        };
        use crate::state::GLOBAL_VAULT_FIXED_SIZE;
        let data: &mut RefMut<&mut [u8]> = &mut settle.vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        // Update profile aggregates.
        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1, 0);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "profile_id {} not found during vault settlement",
            profile_id
        )?;
        let profile_node = get_mut_helper_risk_profile(dynamic, profile_idx);
        let profile = profile_node.get_mut_value();
        // Crystallise yield up to `now` at the OLD weighted_rate
        // before the new loan's contribution is folded in.
        let share_value_fp48 =
            crate::state::vault::read_bank_asset_share_value_fp48(debt_bank.info);
        crate::state::vault::accrue_risk_profile(profile, now_unix_ts, share_value_fp48)?;
        // Atoms transition from "committed-and-waiting"
        // (encumbered_in_orders_atoms) to "deployed-in-loan"
        // (deployed_principal_atoms). Gross convention throughout
        // matches the inline write at the matching-loop accept gate,
        // the close path's decrement of `principal_debt_atoms`, and
        // the wallet path's protocol-fee accumulation on origination
        // shares.
        profile.deployed_principal_atoms = profile
            .deployed_principal_atoms
            .checked_add(principal_atoms)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let weighted_delta: u128 = (principal_atoms as u128)
            .checked_mul(lender_rate_bps as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_weighted_rate_bps = profile
            .total_weighted_rate_bps
            .checked_add(weighted_delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.encumbered_in_orders_atoms = profile
            .encumbered_in_orders_atoms
            .saturating_sub(principal_atoms);
    }

    Ok(())
}
