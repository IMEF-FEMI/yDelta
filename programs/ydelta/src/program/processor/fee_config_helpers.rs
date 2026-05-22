//! Validation + apply helpers for partial `FeeConfig` updates.
//!
//! Two ixs send the same `Option`-everywhere overrides payload:
//! `create_market` (one-shot configuration during account init) and
//! `set_fee_config` (ongoing retunes). The bps bounds, the 50%
//! liquidation-keeper sanity cap, and the 90-day grace cap MUST stay
//! identical between the two paths — otherwise an admin could route
//! through whichever ix happens to be looser. Centralising both the
//! checks and the field-by-field apply guarantees parity by
//! construction.

use solana_program::entrypoint::ProgramResult;

use crate::program::YdeltaError;
use crate::require;
use crate::state::market::FeeConfig;

use super::set_fee_config::SetFeeConfigParams;

/// Shared shape for partial `FeeConfig` overrides. Both
/// `CreateMarketParams` and `SetFeeConfigParams` deserialize into this
/// exact byte layout, so validation and application can live in one
/// place without duplicating eight `if let Some(v) = …` blocks per ix.
pub type FeeConfigOverrides = SetFeeConfigParams;

/// Maximum grace window. An admin typo of `u32::MAX` (~136 years)
/// would permanently disable `settle_matured_loan` on the market, so
/// we cap at 90 days.
pub const MAX_GRACE_PERIOD_SECONDS: u32 = 90 * 86_400;

/// Maximum `liquidation_keeper_bps`. The generic 10_000 (100%) bound
/// is mathematically valid but practically catastrophic — at 100% a
/// single liquidation drains the borrower for zero protocol benefit.
/// 5_000 (50%) is already more aggressive than any real keeper market
/// needs.
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
