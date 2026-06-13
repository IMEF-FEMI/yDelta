//! `SetFeeConfig` — market-admin update of selected `MarketFixed::fee_config`
//! fields. Signer must equal `MarketFixed.admin`. Each `Option<>` field is an
//! override: `Some(v)` is validated and written, `None` leaves the existing
//! value untouched.

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

/// Per-field overrides for `MarketFixed::fee_config`. `None` means "leave
/// unchanged"; `Some(v)` is validated and written.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy, Debug, Default)]
pub struct SetFeeConfigParams {
    /// Floor on the protocol fee in bps (per-loan; see `fee_config_helpers`).
    pub protocol_fee_bps_floor: Option<u16>,
    /// Origination fee in bps, charged on net principal at promotion.
    pub origination_bps: Option<u16>,
    /// Split (bps) of interest paid to the curator.
    pub curator_split_bps: Option<u16>,
    /// Curator fee in bps applied to a profile's deployed principal at match.
    /// Liquidation keeper bonus in bps of repaid principal.
    pub liquidation_keeper_bps: Option<u16>,
    /// Liquidation share in bps of repaid principal that accrues to protocol.
    pub liquidation_protocol_bps: Option<u16>,
    /// Grace period in seconds applied after maturity before settle is allowed.
    pub grace_period_seconds: Option<u32>,
}

/// Apply selected fee_config overrides on a market. Signer must be the market admin.
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
