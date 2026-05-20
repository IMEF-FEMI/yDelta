//! Finalize a `MatchedLoan` queue node into live loan state.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{DataIndex, HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};

use crate::logs::{emit_stack, LoanPromotedLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::loan::{LoanFixed, LoanType, LOAN_FIXED_SIZE, LOAN_SEED};
use crate::state::market::{get_helper_matched_loan, MarketFixed, MatchedLoan};
use crate::state::market_helpers::release_address_on_market_fixed;
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::utils::create_account;
use crate::validation::loaders::ProcessMatchedLoanContext;

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

    process_primary_promotion(program_id, ctx)
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
    } = ctx;
    let market_key = *market.info.key;
    let now_unix_ts: i64 = Clock::get()?.unix_timestamp;

    let loan_type = LoanType::from_u8(node.loan_type)?;
    // `VAULT_PRESETTLED` — set on `convert_p2pool_to_fixed` nodes. The
    // convert processor already migrated the vault principal (to retire
    // the borrower's P2Pool liability) and ran the profile bookkeeping.
    let presettled: bool =
        node.flags & crate::state::market::MATCHED_LOAN_FLAG_VAULT_PRESETTLED != 0;
    let net_principal: u64 = node
        .principal_atoms
        .checked_sub(node.origination_atoms)
        .ok_or(YdeltaError::InvalidArgument)?;
    // Reject a zero-net-principal promotion. If the origination
    // fee consumed the entire matched principal the loan would owe
    // nothing yet still lock the borrower's collateral — a zombie loan.
    require!(
        net_principal > 0,
        YdeltaError::InvalidArgument,
        "process_matched_loan: net_principal is 0 (origination_atoms {} == principal_atoms {})",
        node.origination_atoms,
        node.principal_atoms
    )?;

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

    // Routing wallet-vs-vault settlement is keyed on the MATCH-TIME
    // record (`MATCHED_LOAN_FLAG_VAULT_LENDER` stamped on the node by the
    // matching engine), not a live re-read of the lender seat's
    // `owner_kind`. Every orderbook-funded Fixed loan has a vault
    // risk-profile lender; P2Pool loans carry `lender_seat_index == NIL`
    // and the flag clear.
    let node_says_vault_lender: bool =
        node.flags & crate::state::market::MATCHED_LOAN_FLAG_VAULT_LENDER != 0;

    // Before stamping a `LoanFixed`, re-validate that the queue
    // node's seat indices still resolve to LIVE seats of the expected
    // owner kind. The borrower seat must be a user seat; the lender seat
    // (when present) must be a risk-profile seat. A P2Pool loan has
    // `lender_seat_index == NIL` and no lender seat to check.
    //
    // Read the lender's `ClaimedSeat` and capture the vault PDA +
    // profile_id. The `LoanFixed` fields (`lender_kind`,
    // `lender_profile_id`, `lender_global_vault`) carry these forward so
    // `process_repay` can settle against the vault.
    let (lender_kind, lender_profile_id, lender_global_vault): (u8, u8, Pubkey) = {
        let market_data = market.info.try_borrow_data()?;
        let claimed_seats_root = {
            let fixed: &MarketFixed =
                bytemuck::from_bytes(&market_data[..std::mem::size_of::<MarketFixed>()]);
            fixed.claimed_seats_root_index
        };
        let dynamic = &market_data[std::mem::size_of::<MarketFixed>()..];

        // Borrower seat is always a live user seat.
        crate::state::market::verify_live_seat(
            dynamic,
            claimed_seats_root,
            node.borrower_seat_index,
            crate::state::OWNER_KIND_USER,
        )?;

        if node.lender_seat_index == NIL {
            // P2Pool loan — no orderbook lender. The node flag MUST
            // agree (P2Pool nodes never carry VAULT_LENDER).
            require!(
                !node_says_vault_lender,
                YdeltaError::IncorrectAccount,
                "MatchedLoan has NIL lender seat but VAULT_LENDER flag set"
            )?;
            (0u8, 0u8, Pubkey::default())
        } else {
            // Orderbook-funded Fixed loan — the lender seat must be a
            // live risk-profile seat, and the match-time flag must say
            // so. Reject loudly on any mismatch.
            require!(
                node_says_vault_lender,
                YdeltaError::IncorrectAccount,
                "MatchedLoan has a lender seat but VAULT_LENDER flag clear"
            )?;
            let seat = crate::state::market::verify_live_seat(
                dynamic,
                claimed_seats_root,
                node.lender_seat_index,
                crate::state::OWNER_KIND_RISK_PROFILE,
            )?;
            (
                crate::state::OWNER_KIND_RISK_PROFILE,
                seat.risk_profile_id,
                seat.owner,
            )
        }
    };

    // Snapshot the market's curator_fee_bps. Stamped only on
    // vault-funded loans (skipped for P2Pool loans, which have no
    // curator counterparty).
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

    // `credited_shares` is the borrower's `debt_withdrawable_shares`
    // credit for a freshly-funded Fixed loan — the principal landed in
    // `market.lender_integration` and the borrower can withdraw it.
    // A `VAULT_PRESETTLED` (converted) loan credits NOTHING: the
    // borrower received debt relief on their marginfi liability, not
    // cash, so the vault principal never becomes withdrawable for them.
    let credited_shares: u128 = match loan_type {
        LoanType::Fixed if !presettled => {
            MarginfiV18Adapter.amount_to_asset_shares(&[debt_bank.info.clone()], net_principal)?
        }
        LoanType::Fixed => 0,
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
    //
    // `VAULT_PRESETTLED` nodes (emitted by `convert_p2pool_to_fixed`)
    // skip this entirely: the convert processor already migrated the
    // vault principal — to retire the borrower's P2Pool marginfi
    // liability — and already ran the profile `encumbered → deployed`
    // bookkeeping. Re-running `do_vault_settle` would double-spend the
    // vault.
    if lender_kind == crate::state::OWNER_KIND_RISK_PROFILE
        && loan_type == LoanType::Fixed
        && !presettled
    {
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
            curator_fee_bps_snapshot,
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
///    lender_rate_bps)`, and `total_weighted_net_rate_bps` grows by the
///    same product scaled by `(10_000 − curator_fee_bps) / 10_000` —
///    the latter is what `accrue_risk_profile` credits to depositors so
///    the curator's slice is not double-counted. The
///    market-side `ClaimedSeat`'s `deployed_atoms()` bumps too (the
///    vault-funded seat lives on the market alongside user seats).
///    Lender-rate yield will accrue correctly via `accrue_risk_profile`
///    from this point on.
#[allow(clippy::too_many_arguments)]
fn do_vault_settle<'a, 'info>(
    principal_atoms: u64,
    debt_bank: &'a crate::validation::MarginfiBankInfo<'a, 'info>,
    settle: &'a crate::validation::loaders::VaultSettleAccounts<'a, 'info>,
    lender_rate_bps: u16,
    curator_fee_bps: u16,
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
    let withdraw_cover_atoms = principal_atoms
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
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
    // The cushion strategy: ask marginfi for `principal_atoms + 1` so
    // the ±1 drift band still leaves us with `actual_atoms >=
    // principal_atoms`. But when the vault's marginfi balance is small
    // (a vault funded to back exactly an `ask_principal`-sized cross),
    // ceil-rounding 41 atoms to shares overshoots the live share count
    // and marginfi v0.1.8 hard-errors with `OperationWithdrawOnly`
    // because the implicit drain didn't carry `withdraw_all=Some(true)`.
    // Read the live mfi balance and switch to the `withdraw_atoms_full`
    // path when the cushion would zero the position.
    let mfi_asset_shares_pre: u128 = crate::protocol::marginfi::read_asset_shares_u128(
        settle.global_vault_integration_account.info,
        debt_bank.info.key,
    )?;
    let mfi_atoms_pre: u64 =
        MarginfiV18Adapter.shares_to_amount(&[debt_bank.info.clone()], mfi_asset_shares_pre)?;
    let actual_atoms = if withdraw_cover_atoms >= mfi_atoms_pre {
        // Drain path. Pass the live atom balance as the cap so marginfi
        // closes the position cleanly. Any extra atom is pushed back
        // into the vault below; the market only receives the exact
        // matched principal.
        MarginfiV18Adapter.withdraw_atoms_full(
            &withdraw_accounts,
            mfi_atoms_pre,
            &[global_vault_signer_seeds],
        )?
    } else {
        // Cushion path. Ceil-rounded shares + ±1 drift gate guarantees
        // `actual_atoms >= principal_atoms`; the spare atom is
        // redeposited below.
        let expected_shares: u128 = MarginfiV18Adapter
            .amount_to_asset_shares_ceil(&[debt_bank.info.clone()], withdraw_cover_atoms)?;
        MarginfiV18Adapter.withdraw(
            &withdraw_accounts,
            expected_shares,
            &[global_vault_signer_seeds],
        )?
    };
    require!(
        actual_atoms >= principal_atoms,
        ProgramError::ArithmeticOverflow,
        "vault settle underfunded: actual_atoms {} < principal_atoms {}",
        actual_atoms,
        principal_atoms
    )?;
    let surplus_atoms: u64 = actual_atoms.saturating_sub(principal_atoms);

    // ─── CPI 2: SPL transfer — global_vault_staging → market_debt_vault ───
    // Signed by global_vault_signer (global_vault_staging owner). Move
    // only the nominal matched principal into the market side. Any
    // cushion atom stays in staging and is redeposited to the vault.
    if settle.token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            settle.token_program.info.key,
            settle.global_vault_staging.info.key,
            settle.mint.info.key,
            settle.market_debt_vault.info.key,
            settle.global_vault_signer.key,
            &[],
            principal_atoms,
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
            principal_atoms,
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
        MarginfiV18Adapter.deposit(&deposit_accounts, principal_atoms, &[market_signer_seeds])?;

    if surplus_atoms > 0 {
        let redeposit_accounts: Vec<AccountInfo> = vec![
            settle.marginfi_group.info.clone(),
            settle.global_vault_integration_account.info.clone(),
            settle.global_vault_signer.clone(),
            debt_bank.info.clone(),
            settle.global_vault_staging.info.clone(),
            settle.liquidity_vault.info.clone(),
            settle.token_program.info.clone(),
            settle.marginfi_program.info.clone(),
        ];
        let _returned_shares: u128 = MarginfiV18Adapter.deposit(
            &redeposit_accounts,
            surplus_atoms,
            &[global_vault_signer_seeds],
        )?;
    }

    // ─── State aggregate updates on the vault profile + seat ───
    {
        let data: &mut RefMut<&mut [u8]> = &mut settle.vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        // Update profile aggregates.
        let probe = RiskProfile::new_empty(profile_id, Pubkey::default(), 1, 1);
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
        // NET aggregate: the depositors' share of the
        // lender-rate yield after the curator's manager fee. Scaled
        // DOWN by `(10_000 − curator_fee_bps) / 10_000`; `accrue_risk_profile`
        // credits depositor share price from this aggregate so the
        // curator slice (also accumulated into
        // `accumulated_curator_fee_atoms`) is not double-counted.
        let net_weighted_delta: u128 = weighted_delta
            .checked_mul((crate::state::loan::BPS_PER_UNIT as u128) - curator_fee_bps as u128)
            .and_then(|x| x.checked_div(crate::state::loan::BPS_PER_UNIT as u128))
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.total_weighted_net_rate_bps = profile
            .total_weighted_net_rate_bps
            .checked_add(net_weighted_delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        profile.encumbered_in_orders_atoms = profile
            .encumbered_in_orders_atoms
            .saturating_sub(principal_atoms);
    }

    Ok(())
}
