//! Fee-config validation and override application shared by
//! `create_market` and `set_fee_config`. Bounds-check every bps field
//! at 10_000 and enforces stricter sanity caps on liquidation-keeper
//! and curator fee splits plus the grace-period seconds ceiling.

use solana_program::entrypoint::ProgramResult;

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::FeeConfig;

use super::set_fee_config::SetFeeConfigParams;

/// Re-exported [`SetFeeConfigParams`] used by both `create_market`
/// (initial config) and `set_fee_config` (later overrides).
pub type FeeConfigOverrides = SetFeeConfigParams;

/// Hard cap on `FeeConfig.grace_period_seconds` (90 days).
pub const MAX_GRACE_PERIOD_SECONDS: u32 = 90 * 86_400;

/// Sanity cap on the liquidation keeper's bonus share (50%).
pub const MAX_LIQUIDATION_KEEPER_BPS: u16 = 5_000;


/// Bounds-check every `Some` field in `params`. Each bps value must
/// be `<= 10_000`; `liquidation_keeper_bps`
/// additionally cap at 5_000; `grace_period_seconds` caps at 90 days.
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
    check_bps(params.liquidation_keeper_bps)?;
    check_bps(params.liquidation_protocol_bps)?;

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

/// Apply each `Some` field from `params` onto `target` in place.
/// `None` fields leave the existing value untouched.
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
    if let Some(v) = params.liquidation_keeper_bps {
        target.liquidation_keeper_bps = v;
    }
    if let Some(v) = params.liquidation_protocol_bps {
        target.liquidation_protocol_bps = v;
    }
    if let Some(v) = params.grace_period_seconds {
        target.grace_period_seconds = v;
    }
}
