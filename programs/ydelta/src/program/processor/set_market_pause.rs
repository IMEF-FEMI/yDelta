//! Set or clear the market pause flag.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::MarketFixed;
use crate::validation::loaders::SetMarketPauseContext;

#[derive(BorshDeserialize, BorshSerialize, Clone, Copy)]
pub struct SetMarketPauseParams {
    pub paused: u8,
}

pub fn process_set_market_pause(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let params = SetMarketPauseParams::try_from_slice(data)?;
    let SetMarketPauseContext { payer, market } = SetMarketPauseContext::load(accounts)?;
    let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
    let header: &mut MarketFixed =
        bytemuck::from_bytes_mut(&mut market_data[..std::mem::size_of::<MarketFixed>()]);
    require!(
        header.admin == *payer.info.key,
        YdeltaError::MarketAdminRequired,
        "set_market_pause: signer != MarketFixed.admin"
    )?;
    // A market may not be UNpaused until `set_fee_config` has been
    // explicitly called. A fresh `FeeConfig` defaults every bps
    // field — including `ltv_buffer_bps` — to 0, so an un-configured
    // live market would run zero-margin LTV checks. Pausing (or a
    // no-op re-pause) is always allowed.
    if params.paused == 0 {
        require!(
            header.fee_config_set != 0,
            YdeltaError::InvalidArgument,
            "set_market_pause: cannot unpause before set_fee_config has been called"
        )?;
    }
    header.is_paused = if params.paused != 0 { 1 } else { 0 };
    Ok(())
}
