//! Insert a new `RiskProfile` into a vault.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, RiskProfileCreatedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_free_profile_address_on_vault_fixed, vault_expand_profile_block, GlobalVaultFixed,
    RiskProfile, RiskProfileTree, RiskProfileTreeReadOnly,
};
use crate::state::{
    GLOBAL_VAULT_FIXED_SIZE, RISK_PROFILE_ALLOWED_MARKETS_CAP, RISK_PROFILE_BLOCK_SIZE,
};
use crate::validation::loaders::CreateRiskProfileContext;

#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct CreateRiskProfileParams {
    pub profile_id: u8,
    pub curator: Pubkey,
    pub max_ltv_bps: u16,
    pub max_term_seconds: u32,
    /// Maximum number of markets this profile can simultaneously hold
    /// vault-owned `ClaimedSeat`s in. Must be in `1..=RISK_PROFILE_ALLOWED_MARKETS_CAP`.
    pub allowed_market_max: u8,
}

pub fn process_create_risk_profile(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CreateRiskProfileParams::try_from_slice(data)?;
    let CreateRiskProfileContext {
        payer,
        vault,
        _system_program: _,
    } = CreateRiskProfileContext::load(accounts)?;

    // ─── Param validation ───
    require!(
        params.allowed_market_max >= 1
            && params.allowed_market_max <= RISK_PROFILE_ALLOWED_MARKETS_CAP,
        YdeltaError::VaultProfileAllowedMarketsExceeded,
        "allowed_market_max {} must be in 1..={}",
        params.allowed_market_max,
        RISK_PROFILE_ALLOWED_MARKETS_CAP
    )?;
    require!(
        params.max_ltv_bps > 0 && params.max_ltv_bps < 10_000,
        YdeltaError::VaultProfileLtvOutOfRange,
        "max_ltv_bps {} must be in (0, 10_000)",
        params.max_ltv_bps
    )?;
    require!(
        params.max_term_seconds > 0,
        YdeltaError::VaultProfileTermInvalid,
        "max_term_seconds must be > 0"
    )?;
    // Pubkey::default() is unsignable, so a profile stamped with that
    // curator can't be operated on or transferred — it would be a
    // permanent dead slot eating against allowed_market_count. Reject
    // at create time.
    require!(
        params.curator != Pubkey::default(),
        YdeltaError::InvalidArgument,
        "curator cannot be Pubkey::default() — profile would be \
         un-operable and un-transferable"
    )?;

    let vault_key = *vault.info.key;

    // Realloc the vault to fit one more profile block, then expand the
    // free list. Caller (global_vault_admin) pays the rent diff.
    let needs_grow: bool = {
        let vault_data: &Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let header: &GlobalVaultFixed =
            bytemuck::from_bytes(&vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
        !header.has_free_profile_block()
    };
    if needs_grow {
        let new_size = vault.info.data_len() + RISK_PROFILE_BLOCK_SIZE;
        let rent: Rent = Rent::get()?;
        let new_min = rent.minimum_balance(new_size);
        let old_min = rent.minimum_balance(vault.info.data_len());
        let lamports_diff = new_min.saturating_sub(old_min);
        if lamports_diff > 0 {
            invoke(
                &system_instruction::transfer(payer.info.key, vault.info.key, lamports_diff),
                &[payer.info.clone(), vault.info.clone()],
            )?;
        }

        #[allow(deprecated)]
        vault.info.realloc(new_size, false)?;

        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
        vault_expand_profile_block(header, dynamic)?;
    }

    // ─── Insert the profile ───
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        // Reject duplicate profile_id.
        let existing_idx = {
            let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
            let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
            tree.lookup_index(&probe)
        };
        require!(
            existing_idx == NIL,
            YdeltaError::VaultProfileIdExists,
            "profile_id {} already exists in vault",
            params.profile_id
        )?;

        // Pop a 512-byte block off the profile free list.
        let order_index = get_free_profile_address_on_vault_fixed(header, dynamic);
        require!(
            order_index != NIL,
            ProgramError::AccountDataTooSmall,
            "no free profile block (vault_expand_profile_block should have run)"
        )?;

        let profile = RiskProfile::new_empty(
            params.profile_id,
            params.curator,
            params.max_ltv_bps,
            params.max_term_seconds,
            params.allowed_market_max,
        );

        let mut tree = RiskProfileTree::new(dynamic, header.risk_profiles_root_index, NIL);
        tree.insert(order_index, profile);
        header.risk_profiles_root_index = tree.get_root_index();
        drop(tree);

        header.risk_profile_count = header.risk_profile_count.saturating_add(1);
    }

    emit_stack(RiskProfileCreatedLog {
        global_vault: vault_key,
        curator: params.curator,
        profile_id: params.profile_id,
        allowed_market_count: params.allowed_market_max,
        _pad0: [0; 2],
        max_ltv_bps: params.max_ltv_bps,
        _pad1: [0; 2],
        max_term_seconds: params.max_term_seconds,
        _pad2: [0; 4],
    })?;

    Ok(())
}

use std::cell::Ref;
