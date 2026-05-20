//! Update a market's `FeeConfig`.

use std::cell::RefMut;

use borsh::{BorshDeserialize, BorshSerialize};
use hypertree::get_mut_helper;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};
use std::slice::Iter;

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::MarketFixed;
use crate::validation::{Program, Signer, YdeltaAccountInfo};

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
    // No pause gate. Markets ship paused-by-default and the setup order
    // is "configure fee config, THEN unpause" — a pause check here would
    // deadlock that. This is an admin-only header mutation with no atom
    // flow, so it is safe while paused.
    let _system_program = Program::new(next_account_info(account_iter)?, &system_program::id())?;

    // Admin gate.
    {
        let m = market.get_fixed()?;
        require!(
            *payer.info.key == m.admin,
            YdeltaError::MarketAdminRequired,
            "set_fee_config: signer != MarketFixed.admin"
        )?;
    }

    // Bps bounds.
    fn check_bps(value: Option<u16>) -> Result<(), ProgramError> {
        if let Some(v) = value {
            if v > 10_000 {
                return Err(YdeltaError::InvalidFeeConfig.into());
            }
        }
        Ok(())
    }
    check_bps(params.protocol_fee_bps_floor)?;
    check_bps(params.origination_bps)?;
    check_bps(params.curator_split_bps)?;
    check_bps(params.curator_fee_bps)?;
    check_bps(params.liquidation_keeper_bps)?;
    check_bps(params.liquidation_protocol_bps)?;
    check_bps(params.ltv_buffer_bps)?;

    // Tighter sanity cap on `liquidation_keeper_bps`. The generic
    // 10_000 (100%) bound is technically valid math but practically
    // catastrophic — at 100% the keeper bonus equals seized
    // collateral, so a single liquidation drains the borrower for
    // zero protocol benefit. 5_000 (50%) is already more aggressive
    // than any real keeper market needs.
    if let Some(v) = params.liquidation_keeper_bps {
        require!(
            v <= 5_000,
            YdeltaError::InvalidFeeConfig,
            "liquidation_keeper_bps {} exceeds 5000 (50%) sanity cap",
            v
        )?;
    }

    // Cap grace_period_seconds at 90 days. Without a cap, an admin
    // typo of u32::MAX (~136 years) would permanently disable
    // settle_matured_loan on this market.
    const MAX_GRACE_PERIOD_SECONDS: u32 = 90 * 86_400;
    if let Some(v) = params.grace_period_seconds {
        require!(
            v <= MAX_GRACE_PERIOD_SECONDS,
            YdeltaError::InvalidFeeConfig,
            "grace_period_seconds {} exceeds cap {}",
            v,
            MAX_GRACE_PERIOD_SECONDS
        )?;
    }

    // Mutate.
    {
        let market_data: &mut RefMut<&mut [u8]> = &mut market.info.try_borrow_mut_data()?;
        let header: &mut MarketFixed = get_mut_helper::<MarketFixed>(market_data, 0_u32);
        if let Some(v) = params.protocol_fee_bps_floor {
            header.fee_config.protocol_fee_bps_floor = v;
        }
        if let Some(v) = params.origination_bps {
            header.fee_config.origination_bps = v;
        }
        if let Some(v) = params.curator_split_bps {
            header.fee_config.curator_split_bps = v;
        }
        if let Some(v) = params.curator_fee_bps {
            header.fee_config.curator_fee_bps = v;
        }
        if let Some(v) = params.liquidation_keeper_bps {
            header.fee_config.liquidation_keeper_bps = v;
        }
        if let Some(v) = params.liquidation_protocol_bps {
            header.fee_config.liquidation_protocol_bps = v;
        }
        if let Some(v) = params.ltv_buffer_bps {
            header.fee_config.ltv_buffer_bps = v;
        }
        if let Some(v) = params.grace_period_seconds {
            header.fee_config.grace_period_seconds = v;
        }

        // Mark the market's fee config as explicitly configured.
        // `set_market_pause` refuses to unpause a market until this flag
        // is set, so a fresh market — whose `FeeConfig` defaults every
        // bps field (incl. `ltv_buffer_bps`) to 0 — can never go live
        // without the admin consciously running `set_fee_config`. The
        // admin sets `ltv_buffer_bps` to the desired safety margin in
        // that same call.
        header.fee_config_set = 1;
    }

    Ok(())
}
