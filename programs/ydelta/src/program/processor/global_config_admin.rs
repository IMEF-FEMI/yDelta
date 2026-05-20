//! `GlobalConfig` lifecycle instructions.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

use crate::program::YdeltaError;
use crate::require;
use crate::state::global_config::{
    global_config_pda, GlobalConfig, GLOBAL_CONFIG_DISCRIMINANT, GLOBAL_CONFIG_SEED,
    GLOBAL_CONFIG_SIZE,
};
use crate::validation::loaders::{
    AcceptProtocolAdminContext, CreateGlobalConfigContext, SetGlobalPauseContext,
    TransferProtocolAdminContext,
};

pub fn process_create_global_config(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let CreateGlobalConfigContext {
        payer,
        global_config,
        system_program,
    } = CreateGlobalConfigContext::load(accounts)?;

    let (expected, bump) = global_config_pda();
    require!(
        *global_config.key == expected,
        YdeltaError::IncorrectAccount,
        "global_config PDA mismatch"
    )?;
    require!(
        global_config.data_is_empty(),
        YdeltaError::InvalidArgument,
        "global_config already initialized"
    )?;

    // `allocate` + `assign` (rent shortfall topped up) rather than
    // `create_account`: `global_config` is the protocol singleton at a
    // fixed PDA. `create_account` aborts if the account already holds
    // lamports, so anyone could front-run deployment with a 1-lamport
    // transfer and permanently brick the entire protocol bootstrap.
    let required = Rent::get()?.minimum_balance(GLOBAL_CONFIG_SIZE);
    let bump_arr = [bump];
    let signer_seeds: &[&[u8]] = &[GLOBAL_CONFIG_SEED, &bump_arr];
    let current = global_config.lamports();
    if current < required {
        invoke(
            &system_instruction::transfer(payer.info.key, global_config.key, required - current),
            &[
                payer.info.clone(),
                global_config.clone(),
                system_program.info.clone(),
            ],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(global_config.key, GLOBAL_CONFIG_SIZE as u64),
        &[global_config.clone(), system_program.info.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(global_config.key, program_id),
        &[global_config.clone(), system_program.info.clone()],
        &[signer_seeds],
    )?;

    {
        let data: &mut RefMut<&mut [u8]> = &mut global_config.try_borrow_mut_data()?;
        let header: &mut GlobalConfig = bytemuck::from_bytes_mut(&mut data[..GLOBAL_CONFIG_SIZE]);
        *header = GlobalConfig::new_empty(*payer.info.key);
        header.discriminator = GLOBAL_CONFIG_DISCRIMINANT;
    }
    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct TransferProtocolAdminParams {
    pub new_admin: Pubkey,
}

pub fn process_transfer_protocol_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = TransferProtocolAdminParams::try_from_slice(data)?;
    let TransferProtocolAdminContext {
        payer,
        global_config,
    } = TransferProtocolAdminContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut global_config.info.try_borrow_mut_data()?;
    let header: &mut GlobalConfig = bytemuck::from_bytes_mut(&mut data_ref[..GLOBAL_CONFIG_SIZE]);
    require!(
        header.protocol_admin == *payer.info.key,
        YdeltaError::ProtocolAdminRequired,
        "transfer_protocol_admin: signer != GlobalConfig.protocol_admin"
    )?;
    header.pending_protocol_admin = params.new_admin;
    msg!(
        "ydelta: protocol admin transfer initiated, pending -> {}",
        params.new_admin
    );
    Ok(())
}

pub fn process_accept_protocol_admin(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let AcceptProtocolAdminContext {
        payer,
        global_config,
    } = AcceptProtocolAdminContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut global_config.info.try_borrow_mut_data()?;
    let header: &mut GlobalConfig = bytemuck::from_bytes_mut(&mut data_ref[..GLOBAL_CONFIG_SIZE]);
    require!(
        header.pending_protocol_admin == *payer.info.key,
        YdeltaError::PendingAdminMismatch,
        "accept_protocol_admin: signer != GlobalConfig.pending_protocol_admin"
    )?;
    require!(
        header.pending_protocol_admin != Pubkey::default(),
        YdeltaError::PendingAdminMismatch,
        "no pending protocol_admin set"
    )?;
    header.protocol_admin = header.pending_protocol_admin;
    header.pending_protocol_admin = Pubkey::default();
    msg!("ydelta: protocol admin accepted by {}", payer.info.key);
    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SetGlobalPauseParams {
    pub paused: u8,
}

pub fn process_set_global_pause(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SetGlobalPauseParams::try_from_slice(data)?;
    let SetGlobalPauseContext {
        payer,
        global_config,
    } = SetGlobalPauseContext::load(accounts)?;
    let data_ref: &mut RefMut<&mut [u8]> = &mut global_config.info.try_borrow_mut_data()?;
    let header: &mut GlobalConfig = bytemuck::from_bytes_mut(&mut data_ref[..GLOBAL_CONFIG_SIZE]);
    require!(
        header.protocol_admin == *payer.info.key,
        YdeltaError::ProtocolAdminRequired,
        "set_global_pause: signer != GlobalConfig.protocol_admin"
    )?;
    header.is_paused = if params.paused != 0 { 1 } else { 0 };
    Ok(())
}
