use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed, pubkey::Pubkey,
};

use crate::logs::{emit_stack, RepaymentClaimedForRiskProfileLog};
use crate::program::YdeltaError;
use crate::protocol::{marginfi::MarginfiV18Adapter, LendingProtocol};
use crate::require;
use crate::state::claimed_seat::{ClaimedSeat, OWNER_KIND_RISK_PROFILE};
use crate::state::market::{get_mut_helper_seat, ClaimedSeatTreeReadOnly, MarketFixed};
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
    GLOBAL_VAULT_SIGNER_SEED,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::ClaimRepaymentForRiskProfileContext;
use crate::validation::MARKET_SIGNER_SEED;

use super::shared::get_mut_dynamic_account;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy, Default)]
pub struct ClaimRepaymentForRiskProfileParams {
    /// Identifies the risk-profile seat to sweep. Combined with the
    /// global_vault account, locates the seat at
    /// `(market, OWNER_KIND_RISK_PROFILE, global_vault, risk_profile_id)`.
    pub risk_profile_id: u8,
}

/// Stateless seat→vault sweeper. Per the repay/claim split, this ix
/// NEVER reads the loan PDA and NEVER re-accrues loan interest. It looks
/// at the vault's risk-profile market seat, sees how many `debt_withdrawable_shares`
/// have accumulated (from any borrower's repays — or from liquidate/settle
/// once those ixs land Phase 2B), withdraws the underlying atoms from the
/// per-market `lender_marginfi_account`, and deposits them into this
/// vault's per-vault `global_vault_integration_account`. Then decrements
/// the seat shares and `profile.pending_claim_atoms` by the amount swept.
///
/// Per-loan economic facts (principal, rate, curator_fee_bps, started_at)
/// were already applied to the risk profile by `repay`/`liquidate_loan`/
/// `settle_matured_loan` at their respective close events. The loan PDA
/// is already closed by the time claim runs; the curator just sweeps.
pub fn process_claim_repayment_for_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params: ClaimRepaymentForRiskProfileParams =
        ClaimRepaymentForRiskProfileParams::try_from_slice(data)?;
    let risk_profile_id: u8 = params.risk_profile_id;

    let ClaimRepaymentForRiskProfileContext {
        payer: _,
        market,
        global_vault,
        global_vault_signer,
        global_vault_signer_bump,
        global_vault_staging,
        global_vault_integration_account,
        market_debt_vault,
        market_signer,
        market_signer_bump,
        lender_marginfi_account,
        debt_bank,
        debt_liquidity_vault,
        debt_bank_lva,
        debt_oracle_ais,
        debt_mint,
        token_program,
        marginfi_group,
        marginfi_program,
    } = ClaimRepaymentForRiskProfileContext::load(accounts, risk_profile_id)?;

    let market_key = *market.info.key;
    let global_vault_key = *global_vault.info.key;

    // (1) Look up the lender risk-profile seat by composite key. Pull
    // out the pending claim shares; bail no-op if nothing to sweep.
    // Validation that the seat exists, has owner_kind=RISK_PROFILE, owner=vault,
    // matching profile_id is implicit in `lookup_index` against the probe.
    let (lender_seat_index, pending_shares) = {
        let market_data: &std::cell::Ref<&mut [u8]> = &market.info.try_borrow_data()?;
        let market_dyn_offset = std::mem::size_of::<MarketFixed>();
        let header: &MarketFixed = bytemuck::from_bytes(&market_data[..market_dyn_offset]);
        let dynamic = &market_data[market_dyn_offset..];
        let probe = ClaimedSeat::new_empty(
            global_vault_key,
            OWNER_KIND_RISK_PROFILE,
            risk_profile_id,
        );
        let tree = ClaimedSeatTreeReadOnly::new(dynamic, header.claimed_seats_root_index, NIL);
        let idx = tree.lookup_index(&probe);
        require!(
            idx != NIL,
            YdeltaError::IncorrectAccount,
            "claim: no risk-profile seat found for vault {} profile_id {}",
            global_vault_key,
            risk_profile_id,
        )?;
        let seat = crate::state::market::get_helper_seat(dynamic, idx).get_value();
        (idx, seat.debt_withdrawable_shares)
    };

    if pending_shares == 0 {
        // No-op: nothing has accumulated on the seat. Emit log and return
        // success. This is the common case once the curator catches up.
        emit_stack(RepaymentClaimedForRiskProfileLog {
            market: market_key,
            loan: Pubkey::default(),
            global_vault: global_vault_key,
            risk_profile_id,
            _pad0: [0; 7],
            claimed_atoms: 0,
            principal_atoms: 0,
            _pad1: [0; 8],
            protocol_fee_shares_swept: 0,
        })?;
        return Ok(());
    }

    // (2) Withdraw the seat's pending shares from the per-market
    // lender_marginfi_account into market_debt_vault, signed by market_signer.
    let market_signer_seeds: &[&[u8]] = &[
        MARKET_SIGNER_SEED,
        market_key.as_ref(),
        &[market_signer_bump],
    ];

    let mut withdraw_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        lender_marginfi_account.info.clone(),
        market_signer.clone(),
        debt_bank.info.clone(),
        market_debt_vault.info.clone(),
        debt_bank_lva.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
        debt_bank.info.clone(),
    ];
    for oracle_ai in &debt_oracle_ais.ais {
        withdraw_accounts.push((*oracle_ai).clone());
    }
    let (actual_atoms, actual_shares_burned) =
        MarginfiV18Adapter.withdraw(&withdraw_accounts, pending_shares, &[market_signer_seeds])?;

    if actual_atoms == 0 {
        // Marginfi withdraw rounded to 0 atoms. This can only happen for
        // dust-sized share balances. Decrement the seat by what was
        // actually burned (≤ pending_shares) so we don't loop forever
        // and don't drift the seat tracking.
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
        seat.debt_withdrawable_shares = seat
            .debt_withdrawable_shares
            .saturating_sub(actual_shares_burned);
        return Ok(());
    }

    // (3) Token transfer market_debt_vault → global_vault_staging,
    // signed by market_signer.
    if token_program.info.key == &spl_token_2022::id() {
        let ix = spl_token_2022::instruction::transfer_checked(
            token_program.info.key,
            market_debt_vault.info.key,
            debt_mint.info.key,
            global_vault_staging.info.key,
            market_signer.key,
            &[],
            actual_atoms,
            debt_mint.mint.decimals,
        )?;
        invoke_signed(
            &ix,
            &[
                market_debt_vault.info.clone(),
                debt_mint.info.clone(),
                global_vault_staging.info.clone(),
                market_signer.clone(),
                token_program.info.clone(),
            ],
            &[market_signer_seeds],
        )?;
    } else {
        let ix = spl_token::instruction::transfer(
            token_program.info.key,
            market_debt_vault.info.key,
            global_vault_staging.info.key,
            market_signer.key,
            &[],
            actual_atoms,
        )?;
        invoke_signed(
            &ix,
            &[
                market_debt_vault.info.clone(),
                global_vault_staging.info.clone(),
                market_signer.clone(),
                token_program.info.clone(),
            ],
            &[market_signer_seeds],
        )?;
    }

    // (4) Marginfi deposit global_vault_staging → global_vault_integration_account,
    // signed by global_vault_signer.
    let global_vault_bytes = global_vault_key.to_bytes();
    let global_vault_signer_bump_arr = [global_vault_signer_bump];
    let global_vault_signer_seeds: &[&[u8]] = &[
        GLOBAL_VAULT_SIGNER_SEED,
        &global_vault_bytes,
        &global_vault_signer_bump_arr,
    ];
    let deposit_accounts: Vec<AccountInfo> = vec![
        marginfi_group.info.clone(),
        global_vault_integration_account.info.clone(),
        global_vault_signer.clone(),
        debt_bank.info.clone(),
        global_vault_staging.info.clone(),
        debt_liquidity_vault.info.clone(),
        token_program.info.clone(),
        marginfi_program.info.clone(),
    ];
    let _credited_shares: u128 = MarginfiV18Adapter.deposit(
        &deposit_accounts,
        actual_atoms,
        &[global_vault_signer_seeds],
    )?;

    // (5) H-14: decrement seat.debt_withdrawable_shares by what marginfi
    // ACTUALLY burned (not the input `pending_shares`) — see the
    // explanation above the withdraw call.
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let da = get_mut_dynamic_account::<MarketFixed>(market_data);
        let seat = get_mut_helper_seat(da.dynamic, lender_seat_index).get_mut_value();
        seat.debt_withdrawable_shares = seat
            .debt_withdrawable_shares
            .saturating_sub(actual_shares_burned);
    }

    // (6) Decrement profile.pending_claim_atoms — the atoms have physically
    // left lender_marginfi_account and arrived in this vault's integration
    // account, so the in-transit counter shrinks. saturating_sub: partial
    // repays don't bump pending_claim, so the swept atoms may exceed it.
    {
        let vault_data: &mut RefMut<&mut [u8]> = &mut global_vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = vault_data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &GlobalVaultFixed = bytemuck::from_bytes(fixed_bytes);
        let probe = RiskProfile::new_empty(risk_profile_id, Pubkey::default(), 1, 1);
        let profile_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            tree.lookup_index(&probe)
        };
        require!(
            profile_idx != NIL,
            YdeltaError::VaultProfileNotFound,
            "claim: profile_id {} not found on global_vault",
            risk_profile_id,
        )?;
        let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
        profile.pending_claim_atoms = profile
            .pending_claim_atoms
            .saturating_sub(actual_atoms);
    }

    emit_stack(RepaymentClaimedForRiskProfileLog {
        market: market_key,
        loan: Pubkey::default(),
        global_vault: global_vault_key,
        risk_profile_id,
        _pad0: [0; 7],
        claimed_atoms: actual_atoms,
        principal_atoms: 0,
        _pad1: [0; 8],
        protocol_fee_shares_swept: 0,
    })?;

    Ok(())
}
