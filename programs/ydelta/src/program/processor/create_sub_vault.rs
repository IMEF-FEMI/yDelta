//! `CreateSubVault` instruction. Vault-admin-gated append of a new
//! `SubVault` to a `GlobalVaultFixed`. Auto-assigns `sub_vault_id`
//! from `header.next_sub_vault_id` (monotonic, starts at 1) and grows
//! the vault by one profile block if no free slot remains.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, HyperTreeWriteOperations, NIL};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, sysvar::Sysvar,
};

use crate::logs::{emit_stack, SubVaultCreatedLog};
use crate::program::YdeltaError;
use crate::require;
use crate::state::vault::{
    get_free_profile_address_on_vault_fixed, vault_expand_profile_block, GlobalVaultFixed,
    SubVault, SubVaultTree,
};
use crate::state::{GLOBAL_VAULT_FIXED_SIZE, SUB_VAULT_BLOCK_SIZE};
use crate::validation::loaders::CreateSubVaultContext;

/// Parameters for [`process_create_sub_vault`].
#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct CreateSubVaultParams {
    /// Initial curator. Must be non-default; can be rotated later via
    /// `TransferCurator` / `AcceptCurator`.
    pub curator: Pubkey,
    /// Per-profile LTV cap in bps; required, `0 < v < 10_000`. The
    /// marginfi-auto sentinel was removed (v1 D17) — `None` rejects.
    pub max_ltv_bps: Option<u16>,
    /// Maximum term any of this profile's asks will accept, in seconds.
    /// Must be `> 0`.
    pub max_term_seconds: u32,
}

/// Append a new `SubVault` to the vault. Auto-assigns `sub_vault_id`,
/// expands the vault by one profile block when needed, increments
/// `sub_vault_count`, and emits `SubVaultCreatedLog`.
pub fn process_create_sub_vault(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = CreateSubVaultParams::try_from_slice(data)?;
    let CreateSubVaultContext {
        payer,
        vault,
        _system_program: _,
    } = CreateSubVaultContext::load(accounts)?;

    let stored_max_ltv_bps: u16 = match params.max_ltv_bps {
        Some(v) => {
            require!(
                v > 0 && v < 10_000,
                YdeltaError::SubVaultLtvOutOfRange,
                "max_ltv_bps {} must be in (0, 10_000)",
                v
            )?;
            v
        }
        None => {
            return Err(YdeltaError::SubVaultLtvOutOfRange.into());
        }
    };
    require!(
        params.max_term_seconds > 0,
        YdeltaError::SubVaultTermInvalid,
        "max_term_seconds must be > 0"
    )?;

    require!(
        params.curator != Pubkey::default(),
        YdeltaError::InvalidArgument,
        "curator cannot be Pubkey::default() — profile would be \
         un-operable and un-transferable"
    )?;

    let vault_key = *vault.info.key;

    let needs_grow: bool = {
        let vault_data: &Ref<&mut [u8]> = &vault.info.try_borrow_data()?;
        let header: &GlobalVaultFixed =
            bytemuck::from_bytes(&vault_data[..GLOBAL_VAULT_FIXED_SIZE]);
        !header.has_free_profile_block()
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
        vault_expand_profile_block(header, dynamic)?;
    }

    let assigned_profile_id: u8;
    {
        let data: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
        let (fixed_bytes, dynamic) = data.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
        let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);

        assigned_profile_id = header.next_sub_vault_id;

        header.next_sub_vault_id = header
            .next_sub_vault_id
            .checked_add(1)
            .ok_or(YdeltaError::SubVaultIdExists)?;

        let order_index = get_free_profile_address_on_vault_fixed(header, dynamic);
        require!(
            order_index != NIL,
            ProgramError::AccountDataTooSmall,
            "no free profile block (vault_expand_profile_block should have run)"
        )?;

        let profile = SubVault::new_empty(
            assigned_profile_id,
            params.curator,
            stored_max_ltv_bps,
            params.max_term_seconds,
        );

        let mut tree = SubVaultTree::new(dynamic, header.sub_vaults_root_index, NIL);
        tree.insert(order_index, profile);
        header.sub_vaults_root_index = tree.get_root_index();
        drop(tree);

        header.sub_vault_count = header
            .sub_vault_count
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    emit_stack(SubVaultCreatedLog {
        global_vault: vault_key,
        curator: params.curator,
        sub_vault_id: assigned_profile_id,
        _reserved0: 0,
        _pad0: [0; 2],
        max_ltv_bps: stored_max_ltv_bps,
        _pad1: [0; 2],
        max_term_seconds: params.max_term_seconds,
        _pad2: [0; 4],
    })?;

    Ok(())
}

use std::cell::Ref;
