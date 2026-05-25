//! Off-chain replica of marginfi v0.1.8's accrue formula. Decodes the
//! bank's interest-rate config + the group's program-fee state directly
//! from raw account data (no `marginfi_mocks` types to keep the read
//! path zero-copy), projects the borrowing / lending APRs at the bank's
//! current utilization, and applies continuous-time accrual to lsv/asv
//! through `dt = now - bank.last_update`. Used by repay / convert /
//! close-out flows to fund the EXACT post-CPI liability without dust.

use solana_program::{account_info::AccountInfo, program_error::ProgramError};

use crate::math::{div_scale, mul_scale, SCALE};
use crate::protocol::AdapterError;
use crate::require;

fn assert_marginfi_owned(account: &AccountInfo) -> Result<(), ProgramError> {
    if account.owner != &marginfi_mocks::ID {
        solana_program::msg!(
            "marginfi_rate_calc: account {} owned by {} (expected marginfi program {})",
            account.key,
            account.owner,
            marginfi_mocks::ID,
        );
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(())
}

const ANCHOR_HEADER_LEN: usize = 8;

/// Year length marginfi uses for APR → per-second-rate conversion
/// (365.25 days). Must match marginfi exactly or accrue predictions
/// drift from the post-CPI state.
pub const MARGINFI_SECONDS_PER_YEAR: u128 = 31_557_600;

const INTEREST_CURVE_LEGACY: u8 = 0;

const INTEREST_CURVE_SEVEN_POINT: u8 = 1;

const PROGRAM_FEES_ENABLED_FLAG: u64 = 1;

const BANK_BODY_TOTAL_LIABILITY_SHARES: usize = 248;
const BANK_BODY_TOTAL_ASSET_SHARES: usize = 264;
const BANK_BODY_LAST_UPDATE: usize = 280;
const BANK_BODY_CONFIG_START: usize = 288;

const BC_INTEREST_RATE_CONFIG: usize = 72;

const IRC_OPTIMAL_UTILIZATION_RATE: usize = 0;
const IRC_PLATEAU_INTEREST_RATE: usize = 16;
const IRC_MAX_INTEREST_RATE: usize = 32;
const IRC_INSURANCE_FEE_FIXED_APR: usize = 48;
const IRC_INSURANCE_IR_FEE: usize = 64;
const IRC_PROTOCOL_FIXED_FEE_APR: usize = 80;
const IRC_PROTOCOL_IR_FEE: usize = 96;

const IRC_ZERO_UTIL_RATE: usize = 128;
const IRC_HUNDRED_UTIL_RATE: usize = 132;
const IRC_POINTS: usize = 136;
const IRC_POINTS_COUNT: usize = 5;
const IRC_CURVE_TYPE: usize = 176;

const GROUP_BODY_FLAGS: usize = 32;
const GROUP_BODY_PROGRAM_FEE_FIXED: usize = 72;
const GROUP_BODY_PROGRAM_FEE_RATE: usize = 88;

fn read_wrapped_i80f48_fp48(data: &[u8], body_offset: usize) -> Result<u128, ProgramError> {
    let offset = ANCHOR_HEADER_LEN + body_offset;
    if data.len() < offset + 16 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&data[offset..offset + 16]);
    let bits = i128::from_le_bytes(buf);
    if bits < 0 {
        solana_program::msg!(
            "read_wrapped_i80f48_fp48: negative I80F48 bit pattern ({}) at offset {} — refusing silent 0",
            bits,
            body_offset,
        );
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(bits as u128)
}

fn read_u8(data: &[u8], body_offset: usize) -> Result<u8, ProgramError> {
    let offset = ANCHOR_HEADER_LEN + body_offset;
    if data.len() <= offset {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    Ok(data[offset])
}

fn read_u32(data: &[u8], body_offset: usize) -> Result<u32, ProgramError> {
    let offset = ANCHOR_HEADER_LEN + body_offset;
    if data.len() < offset + 4 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&data[offset..offset + 4]);
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(data: &[u8], body_offset: usize) -> Result<u64, ProgramError> {
    let offset = ANCHOR_HEADER_LEN + body_offset;
    if data.len() < offset + 8 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[offset..offset + 8]);
    Ok(u64::from_le_bytes(buf))
}

fn read_i64(data: &[u8], body_offset: usize) -> Result<i64, ProgramError> {
    Ok(read_u64(data, body_offset)? as i64)
}

/// Read the bank's `last_update` unix timestamp from raw account data.
/// `dt = now - last_update` drives the accrue projection in
/// [`calc_projected_liability_share_value_fp48`] /
/// [`calc_projected_asset_share_value_fp48`].
pub fn read_bank_last_update(bank_info: &AccountInfo) -> Result<i64, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    read_i64(&data, BANK_BODY_LAST_UPDATE)
}

fn read_bank_asset_share_value_fp48(bank_info: &AccountInfo) -> Result<u128, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    read_wrapped_i80f48_fp48(&data, 72)
}

fn read_bank_liability_share_value_fp48(bank_info: &AccountInfo) -> Result<u128, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    read_wrapped_i80f48_fp48(&data, 88)
}

fn read_bank_total_asset_shares_fp48(bank_info: &AccountInfo) -> Result<u128, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    read_wrapped_i80f48_fp48(&data, BANK_BODY_TOTAL_ASSET_SHARES)
}

fn read_bank_total_liability_shares_fp48(bank_info: &AccountInfo) -> Result<u128, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    read_wrapped_i80f48_fp48(&data, BANK_BODY_TOTAL_LIABILITY_SHARES)
}

#[derive(Debug, Clone, Copy)]
struct InterestRateConfigDecoded {
    optimal_utilization_rate_fp48: u128,
    plateau_interest_rate_fp48: u128,
    max_interest_rate_fp48: u128,

    insurance_fee_fixed_apr_fp48: u128,
    insurance_ir_fee_fp48: u128,
    protocol_fixed_fee_apr_fp48: u128,
    protocol_ir_fee_fp48: u128,

    zero_util_rate_milli_u32: u32,
    hundred_util_rate_milli_u32: u32,
    points: [(u32, u32); IRC_POINTS_COUNT],
    curve_type: u8,
}

fn read_bank_interest_rate_config(
    bank_info: &AccountInfo,
) -> Result<InterestRateConfigDecoded, ProgramError> {
    assert_marginfi_owned(bank_info)?;
    let data = bank_info.try_borrow_data()?;
    let irc_base = BANK_BODY_CONFIG_START + BC_INTEREST_RATE_CONFIG;

    let optimal_utilization_rate_fp48 =
        read_wrapped_i80f48_fp48(&data, irc_base + IRC_OPTIMAL_UTILIZATION_RATE)?;
    let plateau_interest_rate_fp48 =
        read_wrapped_i80f48_fp48(&data, irc_base + IRC_PLATEAU_INTEREST_RATE)?;
    let max_interest_rate_fp48 = read_wrapped_i80f48_fp48(&data, irc_base + IRC_MAX_INTEREST_RATE)?;
    let insurance_fee_fixed_apr_fp48 =
        read_wrapped_i80f48_fp48(&data, irc_base + IRC_INSURANCE_FEE_FIXED_APR)?;
    let insurance_ir_fee_fp48 = read_wrapped_i80f48_fp48(&data, irc_base + IRC_INSURANCE_IR_FEE)?;
    let protocol_fixed_fee_apr_fp48 =
        read_wrapped_i80f48_fp48(&data, irc_base + IRC_PROTOCOL_FIXED_FEE_APR)?;
    let protocol_ir_fee_fp48 = read_wrapped_i80f48_fp48(&data, irc_base + IRC_PROTOCOL_IR_FEE)?;
    let zero_util_rate_milli_u32 = read_u32(&data, irc_base + IRC_ZERO_UTIL_RATE)?;
    let hundred_util_rate_milli_u32 = read_u32(&data, irc_base + IRC_HUNDRED_UTIL_RATE)?;

    let mut points = [(0u32, 0u32); IRC_POINTS_COUNT];
    for i in 0..IRC_POINTS_COUNT {
        let off = irc_base + IRC_POINTS + i * 8;
        let util = read_u32(&data, off)?;
        let rate = read_u32(&data, off + 4)?;
        points[i] = (util, rate);
    }
    let curve_type = read_u8(&data, irc_base + IRC_CURVE_TYPE)?;
    Ok(InterestRateConfigDecoded {
        optimal_utilization_rate_fp48,
        plateau_interest_rate_fp48,
        max_interest_rate_fp48,
        insurance_fee_fixed_apr_fp48,
        insurance_ir_fee_fp48,
        protocol_fixed_fee_apr_fp48,
        protocol_ir_fee_fp48,
        zero_util_rate_milli_u32,
        hundred_util_rate_milli_u32,
        points,
        curve_type,
    })
}

#[derive(Debug, Clone, Copy)]
struct GroupProgramFees {
    enabled: bool,
    program_fee_fixed_fp48: u128,
    program_fee_rate_fp48: u128,
}

fn read_group_program_fees(group_info: &AccountInfo) -> Result<GroupProgramFees, ProgramError> {
    assert_marginfi_owned(group_info)?;
    let data = group_info.try_borrow_data()?;
    let flags = read_u64(&data, GROUP_BODY_FLAGS)?;
    let program_fee_fixed_fp48 = read_wrapped_i80f48_fp48(&data, GROUP_BODY_PROGRAM_FEE_FIXED)?;
    let program_fee_rate_fp48 = read_wrapped_i80f48_fp48(&data, GROUP_BODY_PROGRAM_FEE_RATE)?;
    Ok(GroupProgramFees {
        enabled: (flags & PROGRAM_FEES_ENABLED_FLAG) != 0,
        program_fee_fixed_fp48,
        program_fee_rate_fp48,
    })
}

fn rate_milli_u32_to_fp48(rate: u32) -> Result<u128, ProgramError> {
    crate::math::mul_div(rate as u128, 10u128 * SCALE, u32::MAX as u128, false)
}

fn util_centi_u32_to_fp48(util: u32) -> Result<u128, ProgramError> {
    crate::math::mul_div(util as u128, SCALE, u32::MAX as u128, false)
}

fn lerp_fp48(
    start_x: u128,
    start_y: u128,
    end_x: u128,
    end_y: u128,
    target_x: u128,
) -> Result<u128, ProgramError> {
    if end_x <= start_x {
        solana_program::msg!(
            "lerp_fp48: degenerate segment end_x={} <= start_x={} (marginfi rejects)",
            end_x,
            start_x,
        );
        return Err(crate::program::YdeltaError::InvalidArgument.into());
    }
    if target_x < start_x || target_x > end_x {
        solana_program::msg!(
            "lerp_fp48: target_x={} outside [start_x={}, end_x={}] (marginfi rejects)",
            target_x,
            start_x,
            end_x,
        );
        return Err(crate::program::YdeltaError::InvalidArgument.into());
    }
    if end_y < start_y {
        solana_program::msg!(
            "lerp_fp48: negative slope end_y={} < start_y={} (marginfi rejects)",
            end_y,
            start_y,
        );
        return Err(crate::program::YdeltaError::InvalidArgument.into());
    }
    let delta_x = end_x - start_x;
    let offset = target_x - start_x;
    let proportion_fp48 = div_scale(offset, delta_x)?;
    let delta_y = end_y - start_y;
    let scaled_delta = mul_scale(delta_y, proportion_fp48)?;
    Ok(start_y.saturating_add(scaled_delta))
}

fn interest_rate_curve_legacy(
    ur_fp48: u128,
    cfg: &InterestRateConfigDecoded,
) -> Result<u128, ProgramError> {
    let optimal_ur = cfg.optimal_utilization_rate_fp48;
    let plateau_ir = cfg.plateau_interest_rate_fp48;
    let max_ir = cfg.max_interest_rate_fp48;
    if optimal_ur == 0 {
        solana_program::msg!(
            "interest_rate_curve_legacy: degenerate config — optimal_utilization_rate = 0"
        );
        return Err(crate::program::YdeltaError::InvalidArgument.into());
    }
    if ur_fp48 <= optimal_ur {
        let prop_fp48 = div_scale(ur_fp48, optimal_ur)?;
        mul_scale(prop_fp48, plateau_ir)
    } else {
        let numer = ur_fp48 - optimal_ur;
        let denom = SCALE.saturating_sub(optimal_ur);
        if denom == 0 {
            solana_program::msg!(
                "interest_rate_curve_legacy: degenerate config — optimal_utilization_rate ≥ SCALE"
            );
            return Err(crate::program::YdeltaError::InvalidArgument.into());
        }
        let prop_fp48 = div_scale(numer, denom)?;
        let spread = max_ir.saturating_sub(plateau_ir);
        let scaled = mul_scale(prop_fp48, spread)?;
        Ok(plateau_ir.saturating_add(scaled))
    }
}

fn interest_rate_curve_seven_point(
    ur_fp48: u128,
    cfg: &InterestRateConfigDecoded,
) -> Result<u128, ProgramError> {
    let zero_rate = rate_milli_u32_to_fp48(cfg.zero_util_rate_milli_u32)?;
    let hundred_rate = rate_milli_u32_to_fp48(cfg.hundred_util_rate_milli_u32)?;

    let ur_fp48 = ur_fp48.min(SCALE);

    let mut prev_util_fp48: u128 = 0;
    let mut prev_rate_fp48: u128 = zero_rate;
    for &(point_util_u32, point_rate_u32) in cfg.points.iter() {
        if point_util_u32 == 0 {
            continue;
        }
        let point_util_fp48 = util_centi_u32_to_fp48(point_util_u32)?;
        let point_rate_fp48 = rate_milli_u32_to_fp48(point_rate_u32)?;
        require!(
            point_util_fp48 > prev_util_fp48,
            crate::program::YdeltaError::InvalidArgument,
            "seven_point curve: out-of-order point — util {} <= prev {}",
            point_util_fp48,
            prev_util_fp48,
        )?;
        if ur_fp48 <= point_util_fp48 {
            return lerp_fp48(
                prev_util_fp48,
                prev_rate_fp48,
                point_util_fp48,
                point_rate_fp48,
                ur_fp48,
            );
        }
        prev_util_fp48 = point_util_fp48;
        prev_rate_fp48 = point_rate_fp48;
    }

    lerp_fp48(prev_util_fp48, prev_rate_fp48, SCALE, hundred_rate, ur_fp48)
}

fn calc_interest_rate_fp48(
    utilization_fp48: u128,
    cfg: &InterestRateConfigDecoded,
    program_fees: &GroupProgramFees,
) -> Result<(u128, u128), ProgramError> {
    let (program_rate_fee, program_fixed_fee) = if program_fees.enabled {
        (
            program_fees.program_fee_rate_fp48,
            program_fees.program_fee_fixed_fp48,
        )
    } else {
        (0u128, 0u128)
    };
    let fee_ir_fp48 = cfg
        .insurance_ir_fee_fp48
        .saturating_add(cfg.protocol_ir_fee_fp48)
        .saturating_add(program_rate_fee);
    let fee_fixed_fp48 = cfg
        .insurance_fee_fixed_apr_fp48
        .saturating_add(cfg.protocol_fixed_fee_apr_fp48)
        .saturating_add(program_fixed_fee);

    let base_rate_apr_fp48 = match cfg.curve_type {
        INTEREST_CURVE_LEGACY => interest_rate_curve_legacy(utilization_fp48, cfg)?,
        INTEREST_CURVE_SEVEN_POINT => interest_rate_curve_seven_point(utilization_fp48, cfg)?,
        _ => return Err(AdapterError::InvalidIntegrationAccount.into()),
    };

    let lending_rate_apr_fp48 = mul_scale(base_rate_apr_fp48, utilization_fp48)?;

    let one_plus_fee_ir = SCALE.saturating_add(fee_ir_fp48);
    let borrowing_rate_apr_fp48 =
        mul_scale(base_rate_apr_fp48, one_plus_fee_ir)?.saturating_add(fee_fixed_fp48);
    Ok((lending_rate_apr_fp48, borrowing_rate_apr_fp48))
}

fn calc_accrued_share_value_fp48(
    apr_fp48: u128,
    dt: u128,
    share_value_fp48: u128,
) -> Result<u128, ProgramError> {
    if dt == 0 || apr_fp48 == 0 || share_value_fp48 == 0 {
        return Ok(share_value_fp48);
    }

    let period_rate_fp48 = crate::math::mul_div(apr_fp48, dt, MARGINFI_SECONDS_PER_YEAR, false)?;
    let accrued_fp48 = mul_scale(share_value_fp48, period_rate_fp48)?;
    Ok(share_value_fp48.saturating_add(accrued_fp48))
}

/// Project the bank's fp48 `liability_share_value` to `now_unix` by
/// applying the borrowing APR (base + IR fees + program-rate fee +
/// fixed fees) over `now_unix - bank.last_update`. Returns the current
/// lsv unchanged when `dt == 0` or utilization is zero. Pair with
/// [`crate::protocol::marginfi::MarginfiV18Adapter::liability_shares_to_atoms_ceil_at_lsv`]
/// to predict the exact post-CPI atom liability.
pub fn calc_projected_liability_share_value_fp48(
    bank_info: &AccountInfo,
    group_info: &AccountInfo,
    now_unix: i64,
) -> Result<u128, ProgramError> {
    let last_update = read_bank_last_update(bank_info)?;
    let dt = (now_unix - last_update).max(0) as u128;
    let lsv_fp48 = read_bank_liability_share_value_fp48(bank_info)?;
    if dt == 0 {
        return Ok(lsv_fp48);
    }

    let total_asset_shares_fp48 = read_bank_total_asset_shares_fp48(bank_info)?;
    let total_liability_shares_fp48 = read_bank_total_liability_shares_fp48(bank_info)?;
    let asv_fp48 = read_bank_asset_share_value_fp48(bank_info)?;
    let total_assets_fp48 = mul_scale(total_asset_shares_fp48, asv_fp48)?;
    let total_liabilities_fp48 = mul_scale(total_liability_shares_fp48, lsv_fp48)?;
    if total_assets_fp48 == 0 || total_liabilities_fp48 == 0 {
        return Ok(lsv_fp48);
    }
    let utilization_fp48 = div_scale(total_liabilities_fp48, total_assets_fp48)?;

    let cfg = read_bank_interest_rate_config(bank_info)?;
    let program_fees = read_group_program_fees(group_info)?;
    let (_lending_apr_fp48, borrowing_apr_fp48) =
        calc_interest_rate_fp48(utilization_fp48, &cfg, &program_fees)?;
    calc_accrued_share_value_fp48(borrowing_apr_fp48, dt, lsv_fp48)
}

/// Project the bank's fp48 `asset_share_value` to `now_unix` using the
/// lending APR (base × utilization). Returns the current asv unchanged
/// when `dt == 0` or utilization is zero. Used by the lender-side
/// accrue predictor for vault deposits / claims.
pub fn calc_projected_asset_share_value_fp48(
    bank_info: &AccountInfo,
    group_info: &AccountInfo,
    now_unix: i64,
) -> Result<u128, ProgramError> {
    let last_update = read_bank_last_update(bank_info)?;
    let dt = (now_unix - last_update).max(0) as u128;
    let asv_fp48 = read_bank_asset_share_value_fp48(bank_info)?;
    if dt == 0 {
        return Ok(asv_fp48);
    }
    let total_asset_shares_fp48 = read_bank_total_asset_shares_fp48(bank_info)?;
    let total_liability_shares_fp48 = read_bank_total_liability_shares_fp48(bank_info)?;
    let lsv_fp48 = read_bank_liability_share_value_fp48(bank_info)?;
    let total_assets_fp48 = mul_scale(total_asset_shares_fp48, asv_fp48)?;
    let total_liabilities_fp48 = mul_scale(total_liability_shares_fp48, lsv_fp48)?;
    if total_assets_fp48 == 0 || total_liabilities_fp48 == 0 {
        return Ok(asv_fp48);
    }
    let utilization_fp48 = div_scale(total_liabilities_fp48, total_assets_fp48)?;

    let cfg = read_bank_interest_rate_config(bank_info)?;
    let program_fees = read_group_program_fees(group_info)?;
    let (lending_apr_fp48, _borrowing_apr_fp48) =
        calc_interest_rate_fp48(utilization_fp48, &cfg, &program_fees)?;
    calc_accrued_share_value_fp48(lending_apr_fp48, dt, asv_fp48)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_curve_at_optimal_yields_plateau() {
        let cfg = InterestRateConfigDecoded {
            optimal_utilization_rate_fp48: SCALE / 2,
            plateau_interest_rate_fp48: SCALE / 10,
            max_interest_rate_fp48: SCALE,
            insurance_fee_fixed_apr_fp48: 0,
            insurance_ir_fee_fp48: 0,
            protocol_fixed_fee_apr_fp48: 0,
            protocol_ir_fee_fp48: 0,
            zero_util_rate_milli_u32: 0,
            hundred_util_rate_milli_u32: 0,
            points: [(0, 0); IRC_POINTS_COUNT],
            curve_type: INTEREST_CURVE_LEGACY,
        };
        let base = interest_rate_curve_legacy(SCALE / 2, &cfg).unwrap();

        assert_eq!(base, SCALE / 10);
    }

    #[test]
    fn legacy_curve_at_zero_yields_zero() {
        let cfg = InterestRateConfigDecoded {
            optimal_utilization_rate_fp48: SCALE / 2,
            plateau_interest_rate_fp48: SCALE / 10,
            max_interest_rate_fp48: SCALE,
            insurance_fee_fixed_apr_fp48: 0,
            insurance_ir_fee_fp48: 0,
            protocol_fixed_fee_apr_fp48: 0,
            protocol_ir_fee_fp48: 0,
            zero_util_rate_milli_u32: 0,
            hundred_util_rate_milli_u32: 0,
            points: [(0, 0); IRC_POINTS_COUNT],
            curve_type: INTEREST_CURVE_LEGACY,
        };
        let base = interest_rate_curve_legacy(0, &cfg).unwrap();
        assert_eq!(base, 0);
    }

    #[test]
    fn legacy_curve_at_full_util_yields_max() {
        let cfg = InterestRateConfigDecoded {
            optimal_utilization_rate_fp48: SCALE / 2,
            plateau_interest_rate_fp48: SCALE / 10,
            max_interest_rate_fp48: SCALE,
            insurance_fee_fixed_apr_fp48: 0,
            insurance_ir_fee_fp48: 0,
            protocol_fixed_fee_apr_fp48: 0,
            protocol_ir_fee_fp48: 0,
            zero_util_rate_milli_u32: 0,
            hundred_util_rate_milli_u32: 0,
            points: [(0, 0); IRC_POINTS_COUNT],
            curve_type: INTEREST_CURVE_LEGACY,
        };
        let base = interest_rate_curve_legacy(SCALE, &cfg).unwrap();

        assert_eq!(base, SCALE);
    }

    #[test]
    fn calc_interest_rate_adds_borrowing_fees() {
        let cfg = InterestRateConfigDecoded {
            optimal_utilization_rate_fp48: SCALE / 2,
            plateau_interest_rate_fp48: SCALE / 10,
            max_interest_rate_fp48: SCALE,
            insurance_fee_fixed_apr_fp48: SCALE / 1000,
            insurance_ir_fee_fp48: SCALE / 100,
            protocol_fixed_fee_apr_fp48: 0,
            protocol_ir_fee_fp48: 0,
            zero_util_rate_milli_u32: 0,
            hundred_util_rate_milli_u32: 0,
            points: [(0, 0); IRC_POINTS_COUNT],
            curve_type: INTEREST_CURVE_LEGACY,
        };
        let group_fees = GroupProgramFees {
            enabled: false,
            program_fee_fixed_fp48: 0,
            program_fee_rate_fp48: 0,
        };
        let (lending, borrowing) = calc_interest_rate_fp48(SCALE / 2, &cfg, &group_fees).unwrap();

        let expect_lending = SCALE / 20;
        let expect_borrowing = SCALE / 10 + SCALE / 1000 + SCALE / 1000;

        assert!(
            (lending as i128 - expect_lending as i128).unsigned_abs() < 10,
            "lending {} ≉ {}",
            lending,
            expect_lending
        );
        assert!(
            (borrowing as i128 - expect_borrowing as i128).unsigned_abs() < 100,
            "borrowing {} ≉ {}",
            borrowing,
            expect_borrowing
        );
    }

    #[test]
    fn accrued_share_value_zero_dt_unchanged() {
        let lsv = SCALE;
        let apr = SCALE / 10;
        let new_lsv = calc_accrued_share_value_fp48(apr, 0, lsv).unwrap();
        assert_eq!(new_lsv, lsv);
    }

    #[test]
    fn accrued_share_value_one_year_doubles_at_100pct_apr() {
        let lsv = SCALE;
        let apr = SCALE;
        let new_lsv = calc_accrued_share_value_fp48(apr, MARGINFI_SECONDS_PER_YEAR, lsv).unwrap();

        let drift = (new_lsv as i128 - (2 * SCALE) as i128).unsigned_abs();
        assert!(drift < 10, "new_lsv {} ≉ 2.0 fp48", new_lsv);
    }

    #[test]
    fn lerp_fp48_rejects_marginfi_invariant_violations() {
        // (1) end_x <= start_x: degenerate segment.
        assert!(lerp_fp48(100, 0, 100, 50, 100).is_err(), "end_x == start_x");
        assert!(lerp_fp48(100, 0, 50, 50, 75).is_err(), "end_x < start_x");

        // (2) target_x outside [start_x, end_x]: pre-fix clamped silently.
        assert!(lerp_fp48(100, 0, 200, 50, 50).is_err(), "target_x < start_x");
        assert!(lerp_fp48(100, 0, 200, 50, 300).is_err(), "target_x > end_x");

        // (3) end_y < start_y: negative slope.
        assert!(lerp_fp48(0, 100, 200, 50, 100).is_err(), "negative slope");

        // Sanity: a well-formed call still succeeds (midpoint interpolation).
        let mid = lerp_fp48(0, 0, 200, 100, 100).unwrap();
        // 100/200 of (100-0) = 50.
        assert_eq!(mid, 50);
    }
}
