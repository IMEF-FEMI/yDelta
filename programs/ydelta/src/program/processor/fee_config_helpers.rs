use solana_program::entrypoint::ProgramResult;

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::FeeConfig;

use super::set_fee_config::SetFeeConfigParams;

pub type FeeConfigOverrides = SetFeeConfigParams;

pub const MAX_GRACE_PERIOD_SECONDS: u32 = 90 * 86_400;

pub const MAX_LIQUIDATION_KEEPER_BPS: u16 = 5_000;

pub fn validate_fee_config_overrides(params: &FeeConfigOverrides) -> ProgramResult {
    fn check_bps(value: Option<u16>) -> ProgramResult {
        if let Some(v) = value {
            require!(
                v <= 10_000,
                YdeltaError::InvalidFeeConfig,
                "bps {} > 10000",
                v
            )?;
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

    if let Some(v) = params.liquidation_keeper_bps {
        require!(
            v <= MAX_LIQUIDATION_KEEPER_BPS,
            YdeltaError::InvalidFeeConfig,
            "liquidation_keeper_bps {} exceeds {} (50%) sanity cap",
            v,
            MAX_LIQUIDATION_KEEPER_BPS
        )?;
    }

    if let Some(v) = params.grace_period_seconds {
        require!(
            v <= MAX_GRACE_PERIOD_SECONDS,
            YdeltaError::InvalidFeeConfig,
            "grace_period_seconds {} exceeds cap {}",
            v,
            MAX_GRACE_PERIOD_SECONDS
        )?;
    }

    Ok(())
}

pub fn apply_fee_config_overrides(target: &mut FeeConfig, params: &FeeConfigOverrides) {
    if let Some(v) = params.protocol_fee_bps_floor {
        target.protocol_fee_bps_floor = v;
    }
    if let Some(v) = params.origination_bps {
        target.origination_bps = v;
    }
    if let Some(v) = params.curator_split_bps {
        target.curator_split_bps = v;
    }
    if let Some(v) = params.curator_fee_bps {
        target.curator_fee_bps = v;
    }
    if let Some(v) = params.liquidation_keeper_bps {
        target.liquidation_keeper_bps = v;
    }
    if let Some(v) = params.liquidation_protocol_bps {
        target.liquidation_protocol_bps = v;
    }
    if let Some(v) = params.ltv_buffer_bps {
        target.ltv_buffer_bps = v;
    }
    if let Some(v) = params.grace_period_seconds {
        target.grace_period_seconds = v;
    }
}
