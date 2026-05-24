use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::get_mut_helper;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    pubkey::Pubkey,
    system_program,
};
use std::slice::Iter;

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::MarketFixed;
use crate::validation::{Program, Signer, YdeltaAccountInfo};

use super::fee_config_helpers::{apply_fee_config_overrides, validate_fee_config_overrides};

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy, Debug, Default)]
pub struct SetFeeConfigParams {
    pub protocol_fee_bps_floor: Option<u16>,
    pub origination_bps: Option<u16>,
    pub curator_split_bps: Option<u16>,
    pub curator_fee_bps: Option<u16>,
    pub liquidation_keeper_bps: Option<u16>,
    pub liquidation_protocol_bps: Option<u16>,
    pub ltv_buffer_bps: Option<u16>,
    pub grace_period_seconds: Option<u32>,
}

pub fn process_set_fee_config(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SetFeeConfigParams::try_from_slice(data)?;
    let account_iter: &mut Iter<AccountInfo> = &mut accounts.iter();

    let payer = Signer::new_payer(next_account_info(account_iter)?)?;
    let _ = crate::validation::loaders::load_global_config(account_iter)?;
    let market = YdeltaAccountInfo::<MarketFixed>::new(next_account_info(account_iter)?)?;

    let _system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

    {
        let m = market.get_fixed()?;
        require!(
            *payer.info.key == m.admin,
            YdeltaError::MarketAdminRequired,
            "set_fee_config: signer != MarketFixed.admin"
        )?;
    }

    validate_fee_config_overrides(&params)?;

    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let header: &mut MarketFixed = get_mut_helper::<MarketFixed>(market_data, 0_u32);
        apply_fee_config_overrides(&mut header.fee_config, &params);
    }

    Ok(())
}
