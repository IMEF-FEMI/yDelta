//! `UpdateSubVault` — curator-gated update of a sub-vault's mutable
//! parameters (the curator — the owner, for Private — manages
//! their own spread / LTV pair / term; `curator_fee_bps` is immutable).
//! Each `Option<>` field is an override; `None` leaves the field
//! unchanged. Rejected when the sub-vault is sunset.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_mut_helper_sub_vault, GlobalVaultFixed, SubVault, SubVaultTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::SubVaultMutationContext;

/// Per-field overrides for a `SubVault`. `None` leaves the field
/// unchanged; `Some(v)` is validated and written.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct UpdateSubVaultParams {
    /// Sub-vault ID to update (1-based; 0 is the sentinel).
    pub sub_vault_id: u16,
    /// New max LTV in bps; must be in `(0, 10_000)`.
    pub new_max_ltv_bps: Option<u16>,
    /// New liquidation trigger in bps; the resulting pair must satisfy
    /// `liquidation >= max_ltv + MIN_LIQ_GAP_BPS`.
    pub new_liquidation_ltv_bps: Option<u16>,
    /// New max term in seconds; must be `> 0`.
    pub new_max_term_seconds: Option<u32>,
    /// New spread over the live bank rate, in bps.
    pub new_spread_bps: Option<u16>,
}

/// Update mutable parameters on a sub-vault. Rejected when the sub_vault is
/// sunset.
pub fn process_update_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = UpdateSubVaultParams::try_from_slice(data)?;

    let SubVaultMutationContext {
        payer,
        vault,
        _system_program: _,
    } = SubVaultMutationContext::load(accounts)?;

    let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

    let probe = SubVault::new_empty(params.sub_vault_id, Pubkey::default(), 1, 1);
    let sub_vault_idx = {
        let tree = SubVaultTreeReadOnly::new(dynamic, header.sub_vaults_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        sub_vault_idx != NIL,
        YdeltaError::SubVaultNotFound,
        "sub_vault_id {} not found",
        params.sub_vault_id
    )?;

    let sub_vault = get_mut_helper_sub_vault(dynamic, sub_vault_idx).get_mut_value();
    // curator-gated (the owner, for Private sub-vaults).
    require!(
        sub_vault.curator == *payer.info.key,
        YdeltaError::VaultCuratorRequired,
        "update_sub_vault: signer ({}) is not the sub-vault curator ({})",
        payer.info.key,
        sub_vault.curator
    )?;
    require!(
        sub_vault.is_sunset == 0,
        YdeltaError::SubVaultSunset,
        "update_sub_vault: sub_vault_id {} is sunset; parameter updates are rejected \
         during wind-down",
        params.sub_vault_id
    )?;

    // Validate the RESULTING pair/term so partial overrides cannot break
    // the liq-gap invariant.
    let next_max_ltv = params.new_max_ltv_bps.unwrap_or(sub_vault.max_ltv_bps);
    let next_liq_ltv = params
        .new_liquidation_ltv_bps
        .unwrap_or(sub_vault.liquidation_ltv_bps);
    let next_term = params
        .new_max_term_seconds
        .unwrap_or(sub_vault.max_term_seconds);
    crate::program::processor::create_sub_vault::validate_sub_vault_risk_params(
        next_max_ltv,
        next_liq_ltv,
        next_term,
    )?;

    sub_vault.max_ltv_bps = next_max_ltv;
    sub_vault.liquidation_ltv_bps = next_liq_ltv;
    sub_vault.max_term_seconds = next_term;
    if let Some(v) = params.new_spread_bps {
        sub_vault.spread_bps = v;
    }

    Ok(())
}
