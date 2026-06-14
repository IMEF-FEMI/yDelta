//! Sub-vault creation: `CreatePoolSubVault` is
//! protocol-admin-gated and assigns a curator + `curator_fee_bps`;
//! `CreatePrivateSubVault` is permissionless — the signer becomes the
//! curator/sole depositor and the fee is forced to 0. Both auto-assign
//! `sub_vault_id` from `header.next_sub_vault_id` (monotonic, starts at
//! 1) and grow the vault by one sub_vault block if no free slot remains.

use std::cell::{Ref, RefMut};

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, SubVaultCreatedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::constants::{MAX_CURATOR_FEE_BPS, MIN_LIQ_GAP_BPS};
use crate::state::vault::{
    get_free_sub_vault_address_on_vault_fixed, vault_expand_sub_vault_block, GlobalVaultFixed,
    SubVault, SubVaultTree, SUB_VAULT_KIND_POOL, SUB_VAULT_KIND_PRIVATE,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, SUB_VAULT_BLOCK_SIZE};
use crate::validation::loaders::{CreatePoolSubVaultContext, SubVaultMutationContext};
use crate::validation::Signer as YdeltaSigner;
use crate::validation::YdeltaAccountInfo;

/// Parameters for [`process_create_pool_sub_vault`].
#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct CreatePoolSubVaultParams {
    /// Admin-assigned curator. Must be non-default; rotated later via
    /// `TransferCurator` / `AcceptCurator`.
    pub curator: Pubkey,
    /// Quoted spread over the live marginfi lending APR, in bps.
    pub spread_bps: u16,
    /// Origination LTV cap in bps; `0 < v < 10_000`.
    pub max_ltv_bps: u16,
    /// Liquidation trigger in bps; `max_ltv + MIN_LIQ_GAP_BPS ≤ v ≤
    /// 10_000`.
    pub liquidation_ltv_bps: u16,
    /// Maximum term any of this sub-vault's asks will accept; `> 0`.
    pub max_term_seconds: u32,
    /// Curator management fee in bps; `≤ MAX_CURATOR_FEE_BPS`.
    /// Immutable after creation.
    pub curator_fee_bps: u16,
}

/// Parameters for [`process_create_private_sub_vault`]. The signer is
/// stamped as curator; the fee is forced to 0.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct CreatePrivateSubVaultParams {
    /// Quoted spread over the live marginfi lending APR, in bps.
    pub spread_bps: u16,
    /// Origination LTV cap in bps; `0 < v < 10_000`.
    pub max_ltv_bps: u16,
    /// Liquidation trigger in bps; `max_ltv + MIN_LIQ_GAP_BPS ≤ v ≤
    /// 10_000`.
    pub liquidation_ltv_bps: u16,
    /// Maximum term any of this sub-vault's asks will accept; `> 0`.
    pub max_term_seconds: u32,
}

/// Shared risk-parameter validation for create and update paths
/// (the liq gap guarantees no loan is born liquidatable).
pub fn validate_sub_vault_risk_params(
    max_ltv_bps: u16,
    liquidation_ltv_bps: u16,
    max_term_seconds: u32,
) -> ProgramResult {
    require!(
        max_ltv_bps > 0 && max_ltv_bps < 10_000,
        YdeltaError::SubVaultLtvOutOfRange,
        "max_ltv_bps {} must be in (0, 10_000)",
        max_ltv_bps
    )?;
    require!(
        liquidation_ltv_bps <= 10_000,
        YdeltaError::SubVaultLtvOutOfRange,
        "liquidation_ltv_bps {} must be <= 10_000",
        liquidation_ltv_bps
    )?;
    require!(
        liquidation_ltv_bps >= max_ltv_bps.saturating_add(MIN_LIQ_GAP_BPS),
        YdeltaError::SubVaultLtvOutOfRange,
        "liquidation_ltv_bps {} must be >= max_ltv_bps {} + MIN_LIQ_GAP_BPS {}",
        liquidation_ltv_bps,
        max_ltv_bps,
        MIN_LIQ_GAP_BPS
    )?;
    require!(
        max_term_seconds > 0,
        YdeltaError::SubVaultTermInvalid,
        "max_term_seconds must be > 0"
    )?;
    Ok(())
}

/// Protocol-admin creates a curator-run Pool sub-vault.
pub fn process_create_pool_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CreatePoolSubVaultParams::try_from_slice(data)?;
    let CreatePoolSubVaultContext {
        payer,
        vault,
        _system_program: _,
    } = CreatePoolSubVaultContext::load(accounts)?;

    validate_sub_vault_risk_params(
        params.max_ltv_bps,
        params.liquidation_ltv_bps,
        params.max_term_seconds,
    )?;
    require!(
        params.curator_fee_bps <= MAX_CURATOR_FEE_BPS,
        YdeltaError::InvalidArgument,
        "curator_fee_bps {} exceeds MAX_CURATOR_FEE_BPS ({})",
        params.curator_fee_bps,
        MAX_CURATOR_FEE_BPS
    )?;
    require!(
        params.curator != Pubkey::default(),
        YdeltaError::InvalidArgument,
        "curator cannot be Pubkey::default() — sub-vault would be \
         un-operable and un-transferable"
    )?;

    append_sub_vault(
        &payer,
        &vault,
        SUB_VAULT_KIND_POOL,
        params.curator,
        params.spread_bps,
        params.max_ltv_bps,
        params.liquidation_ltv_bps,
        params.max_term_seconds,
        params.curator_fee_bps,
    )
}

/// Anyone creates a single-owner Private sub-vault: the signer
/// is the curator and sole depositor; the fee is 0.
pub fn process_create_private_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CreatePrivateSubVaultParams::try_from_slice(data)?;
    let SubVaultMutationContext {
        payer,
        vault,
        _system_program: _,
    } = SubVaultMutationContext::load(accounts)?;

    validate_sub_vault_risk_params(
        params.max_ltv_bps,
        params.liquidation_ltv_bps,
        params.max_term_seconds,
    )?;

    let curator = *payer.info.key;
    append_sub_vault(
        &payer,
        &vault,
        SUB_VAULT_KIND_PRIVATE,
        curator,
        params.spread_bps,
        params.max_ltv_bps,
        params.liquidation_ltv_bps,
        params.max_term_seconds,
        /*curator_fee_bps=*/ 0,
    )
}

/// Shared append: expands the vault by one sub_vault block when needed,
/// assigns the monotonic id, inserts the `SubVault`, bumps the count,
/// and emits `SubVaultCreatedLog`.
#[allow(clippy::too_many_arguments)]
fn append_sub_vault<'a, 'info>(
    payer: &YdeltaSigner<'a, 'info>,
    vault: &YdeltaAccountInfo<'a, 'info, GlobalVaultFixed>,
    kind: u8,
    curator: Pubkey,
    spread_bps: u16,
    max_ltv_bps: u16,
    liquidation_ltv_bps: u16,
    max_term_seconds: u32,
    curator_fee_bps: u16,
) -> ProgramResult {
    let vault_key = *vault.info.key;

    let needs_grow: bool = {
        let vault_data: &Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let header: &GlobalVaultFixed =
            bytemuck::from_bytes(&vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
        !header.has_free_sub_vault_block()
    };
    if needs_grow {
        let new_size = vault.info.data_len() + SUB_VAULT_BLOCK_SIZE;
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
        vault_expand_sub_vault_block(header, dynamic)?;
    }

    let assigned_sub_vault_id: u16;
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        assigned_sub_vault_id = header.next_sub_vault_id;

        header.next_sub_vault_id = header
            .next_sub_vault_id
            .checked_add(1)
            .ok_or(YdeltaError::SubVaultIdExists)?;

        let order_index = get_free_sub_vault_address_on_vault_fixed(header, dynamic);
        require!(
            order_index != NIL,
            ProgramError::AccountDataTooSmall,
            "no free sub_vault block (vault_expand_sub_vault_block should have run)"
        )?;

        let mut sub_vault = SubVault::new_empty(
            assigned_sub_vault_id,
            curator,
            max_ltv_bps,
            max_term_seconds,
        );
        sub_vault.kind = kind;
        sub_vault.spread_bps = spread_bps;
        sub_vault.liquidation_ltv_bps = liquidation_ltv_bps;
        sub_vault.curator_fee_bps = curator_fee_bps;

        let mut tree = SubVaultTree::new(dynamic, header.sub_vaults_root_index, NIL);
        tree.insert(order_index, sub_vault);
        header.sub_vaults_root_index = tree.get_root_index();
        drop(tree);

        header.sub_vault_count = header
            .sub_vault_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    emit_stack(SubVaultCreatedLog {
        global_vault: vault_key,
        curator,
        sub_vault_id: assigned_sub_vault_id,
        kind,
        _pad0: [0; 1],
        max_ltv_bps,
        liquidation_ltv_bps,
        max_term_seconds,
        spread_bps,
        curator_fee_bps,
    })?;

    Ok(())
}
