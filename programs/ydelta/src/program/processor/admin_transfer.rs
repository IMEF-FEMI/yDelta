//! Two-step admin transfers for the three admin slots:
//! `MarketFixed.admin`, `GlobalVaultFixed.global_vault_admin`, and per-profile
//! `RiskProfile.curator`.
//!
//! Pattern: `transfer_*` (signer = current admin) sets `pending_*`;
//! `accept_*` (signer = pending_*) promotes pending into the live slot
//! and zeroes pending. Two steps prevent accidental transfer to a
//! non-controlled key (whoever sets pending could fat-finger; the
//! intended new admin must actively accept).
//!
//! All six ixs are admin-gated and pure header mutations — no CPIs, no
//! atom flow.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::{HyperTreeReadOperations, NIL};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::MarketFixed;
use crate::state::vault::{
    get_mut_helper_risk_profile, GlobalVaultFixed, RiskProfile, RiskProfileTreeReadOnly,
};
use crate::state::GLOBAL_VAULT_FIXED_SIZE;
use crate::validation::loaders::{
    AcceptCuratorContext, AcceptGlobalVaultAdminContext, AcceptMarketAdminContext,
    TransferCuratorContext, TransferGlobalVaultAdminContext, TransferMarketAdminContext,
};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferMarketAdminParams {
    pub new_admin: Pubkey,
}

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
    Ok(())
}

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
    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferGlobalVaultAdminParams {
    pub new_admin: Pubkey,
}

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
    Ok(())
}

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
    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferCuratorParams {
    pub profile_id: u8,
    pub new_curator: Pubkey,
}

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
    let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
    let profile_idx = {
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        profile_idx != NIL,
        YdeltaError::VaultProfileNotFound,
        "profile_id {} not found",
        params.profile_id
    )?;
    let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
    require!(
        profile.curator == *payer.info.key,
        YdeltaError::VaultCuratorRequired,
        "transfer_curator: signer != RiskProfile.curator"
    )?;
    profile.pending_curator = params.new_curator;
    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct AcceptCuratorParams {
    pub profile_id: u8,
}

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
    let probe = RiskProfile::new_empty(params.profile_id, Pubkey::default(), 1, 1, 0);
    let profile_idx = {
        let tree = RiskProfileTreeReadOnly::new(dynamic, header.risk_profiles_root_index, NIL);
        tree.lookup_index(&probe)
    };
    require!(
        profile_idx != NIL,
        YdeltaError::VaultProfileNotFound,
        "profile_id {} not found",
        params.profile_id
    )?;
    let profile = get_mut_helper_risk_profile(dynamic, profile_idx).get_mut_value();
    require!(
        profile.pending_curator == *payer.info.key,
        YdeltaError::PendingAdminMismatch,
        "accept_curator: signer != RiskProfile.pending_curator"
    )?;
    require!(
        profile.pending_curator != Pubkey::default(),
        YdeltaError::PendingAdminMismatch,
        "no pending curator set"
    )?;
    profile.curator = profile.pending_curator;
    profile.pending_curator = Pubkey::default();
    Ok(())
}
