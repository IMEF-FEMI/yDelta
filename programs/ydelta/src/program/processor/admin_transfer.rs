//! Two-step admin / curator role transfers for markets, global vaults,
//! and sub-vaults. Each role exposes a `Transfer*` initiator (signer
//! must be the current role-holder; sets `pending_*`) and an `Accept*`
//! confirm (signer must match `pending_*`; promotes pending to live).

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, msg, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::MarketFixed;
use crate::state::vault::{
    get_mut_helper_sub_vault, GlobalVaultFixed, SubVault, SubVaultTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::{
    AcceptCuratorContext, AcceptGlobalVaultAdminContext, AcceptMarketAdminContext,
    TransferCuratorContext, TransferGlobalVaultAdminContext, TransferMarketAdminContext,
};

/// Parameters for [`process_transfer_market_admin`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferMarketAdminParams {
    /// Pubkey to set as `MarketFixed.pending_admin`.
    pub new_admin: Pubkey,
}

/// Initiates a market admin transfer. Signer must equal the current
/// `MarketFixed.admin`; writes `params.new_admin` into `pending_admin`.
pub fn process_transfer_market_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = TransferMarketAdminParams::try_from_slice(data)?;
    let TransferMarketAdminContext { payer, market } = TransferMarketAdminContext::load(accounts)?;
    let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
    let header: &mut MarketFixed =
        bytemuck::from_bytes_mut(&mut market_data[..std::mem::size_of::<MarketFixed>()]);
    require!(
        header.admin == *payer.info.key,
        YdeltaError::MarketAdminRequired,
        "transfer_market_admin: signer != MarketFixed.admin"
    )?;
    header.pending_admin = params.new_admin;
    msg!(
        "ydelta: market admin transfer initiated, pending -> {}",
        params.new_admin
    );
    Ok(())
}

/// Completes a market admin transfer. Signer must equal
/// `pending_admin` (non-default); promotes it to `admin` and clears
/// `pending_admin`.
pub fn process_accept_market_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let AcceptMarketAdminContext { payer, market } = AcceptMarketAdminContext::load(accounts)?;
    let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
    let header: &mut MarketFixed =
        bytemuck::from_bytes_mut(&mut market_data[..std::mem::size_of::<MarketFixed>()]);
    require!(
        header.pending_admin == *payer.info.key,
        YdeltaError::PendingAdminMismatch,
        "accept_market_admin: signer != MarketFixed.pending_admin"
    )?;
    require!(
        header.pending_admin != Pubkey::default(),
        YdeltaError::PendingAdminMismatch,
        "no pending admin set"
    )?;
    header.admin = header.pending_admin;
    header.pending_admin = Pubkey::default();
    msg!("ydelta: market admin accepted by {}", payer.info.key);
    Ok(())
}

/// Parameters for [`process_transfer_global_vault_admin`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferGlobalVaultAdminParams {
    /// Pubkey to set as `GlobalVaultFixed.pending_global_vault_admin`.
    pub new_admin: Pubkey,
}

/// Initiates a global-vault admin transfer. Signer must equal the
/// current `global_vault_admin`; writes `params.new_admin` into
/// `pending_global_vault_admin`.
pub fn process_transfer_global_vault_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = TransferGlobalVaultAdminParams::try_from_slice(data)?;
    let TransferGlobalVaultAdminContext { payer, vault } =
        TransferGlobalVaultAdminContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let header: &mut GlobalVaultFixed =
        bytemuck::from_bytes_mut(&mut data_ref[..GLOBAL_VAULT_FIXED_SIZE]);
    require!(
        header.global_vault_admin == *payer.info.key,
        YdeltaError::VaultAdminRequired,
        "transfer_global_vault_admin: signer != GlobalVaultFixed.global_vault_admin"
    )?;
    header.pending_global_vault_admin = params.new_admin;
    msg!(
        "ydelta: global_vault admin transfer initiated, pending -> {}",
        params.new_admin
    );
    Ok(())
}

/// Completes a global-vault admin transfer. Signer must equal
/// `pending_global_vault_admin` (non-default); promotes it to
/// `global_vault_admin` and clears the pending slot.
pub fn process_accept_global_vault_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let AcceptGlobalVaultAdminContext { payer, vault } =
        AcceptGlobalVaultAdminContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let header: &mut GlobalVaultFixed =
        bytemuck::from_bytes_mut(&mut data_ref[..GLOBAL_VAULT_FIXED_SIZE]);
    require!(
        header.pending_global_vault_admin == *payer.info.key,
        YdeltaError::PendingAdminMismatch,
        "accept_global_vault_admin: signer != GlobalVaultFixed.pending_global_vault_admin"
    )?;
    require!(
        header.pending_global_vault_admin != Pubkey::default(),
        YdeltaError::PendingAdminMismatch,
        "no pending global_vault_admin set"
    )?;
    header.global_vault_admin = header.pending_global_vault_admin;
    header.pending_global_vault_admin = Pubkey::default();
    msg!("ydelta: global_vault admin accepted by {}", payer.info.key);
    Ok(())
}

/// Parameters for [`process_transfer_curator`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferCuratorParams {
    /// Identifies the sub-vault whose curator is being transferred.
    pub sub_vault_id: u16,
    /// Pubkey to set as `SubVault.pending_curator`.
    pub new_curator: Pubkey,
}

/// Initiates a sub-vault curator transfer. Signer must equal the
/// sub-vault's current `curator`; writes `params.new_curator` into
/// `pending_curator`.
pub fn process_transfer_curator(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = TransferCuratorParams::try_from_slice(data)?;
    let TransferCuratorContext { payer, vault } = TransferCuratorContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data_ref.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
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
    let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
    require!(
        profile.curator == *payer.info.key,
        YdeltaError::VaultCuratorRequired,
        "transfer_curator: signer != SubVault.curator"
    )?;
    profile.pending_curator = params.new_curator;
    msg!(
        "ydelta: curator transfer initiated for sub_vault {}, pending -> {}",
        params.sub_vault_id,
        params.new_curator
    );
    Ok(())
}

/// Parameters for [`process_accept_curator`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct AcceptCuratorParams {
    /// Identifies the sub-vault to take curator control of.
    pub sub_vault_id: u16,
}

/// Completes a curator transfer. Signer must equal the profile's
/// `pending_curator` (non-default); promotes it to `curator` and
/// clears the pending slot.
pub fn process_accept_curator(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = AcceptCuratorParams::try_from_slice(data)?;
    let AcceptCuratorContext { payer, vault } = AcceptCuratorContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut vault.info.try_borrow_mut_data()?;
    let (fixed_bytes, dynamic) = data_ref.split_at_mut(GLOBAL_VAULT_FIXED_SIZE);
    let header: &mut GlobalVaultFixed = bytemuck::from_bytes_mut(fixed_bytes);
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
    let profile = get_mut_helper_sub_vault(dynamic, profile_idx).get_mut_value();
    require!(
        profile.pending_curator == *payer.info.key,
        YdeltaError::PendingAdminMismatch,
        "accept_curator: signer != SubVault.pending_curator"
    )?;
    require!(
        profile.pending_curator != Pubkey::default(),
        YdeltaError::PendingAdminMismatch,
        "no pending curator set"
    )?;
    profile.curator = profile.pending_curator;
    profile.pending_curator = Pubkey::default();
    msg!(
        "ydelta: curator accepted for profile {} by {}",
        params.sub_vault_id,
        payer.info.key
    );
    Ok(())
}
