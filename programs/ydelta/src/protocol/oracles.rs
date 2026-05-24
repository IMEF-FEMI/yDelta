use solana_program::{
    account_info::AccountInfo, clock::Clock, pubkey, pubkey::Pubkey, stake::state::StakeStateV2,
    sysvar::Sysvar,
};

pub const PYTH_PUSH_PROGRAM_ID: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

pub const SWITCHBOARD_ON_DEMAND_PROGRAM_ID: Pubkey =
    pubkey!("SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv");

use marginfi_mocks::state::{BankConfigView, OracleSetup};

use crate::program::YdeltaError;
use crate::require;

use super::AdapterError;

type AdapterResult<T> = Result<T, solana_program::program_error::ProgramError>;

/// Default oracle staleness window. Matches marginfi's reference
/// `MAX_PYTH_ORACLE_AGE = 300` seconds — keeping the two systems in
/// lockstep so an oracle that's fresh enough for yDelta is also fresh
/// enough for marginfi (relevant for cross-CPI flows where both engines
/// evaluate the same price).
const DEFAULT_ORACLE_MAX_AGE_SECS: i64 = 300;

const _MARGINFI_ORACLE_MIN_AGE_SECS: i64 = 10;

const MAX_FUTURE_SKEW_SECS: i64 = 900;

pub fn expected_oracle_account_count(setup: OracleSetup) -> Option<usize> {
    match setup {
        OracleSetup::None => None,

        OracleSetup::PythLegacy | OracleSetup::SwitchboardV2 => None,
        OracleSetup::PythPushOracle | OracleSetup::SwitchboardPull => Some(1),
        OracleSetup::StakedWithPythPush => Some(3),

        OracleSetup::KaminoPythPush
        | OracleSetup::KaminoSwitchboardPull
        | OracleSetup::DriftPythPull
        | OracleSetup::DriftSwitchboardPull
        | OracleSetup::SolendPythPull
        | OracleSetup::SolendSwitchboardPull
        | OracleSetup::JuplendPythPull
        | OracleSetup::JuplendSwitchboardPull => None,

        OracleSetup::Fixed
        | OracleSetup::FixedKamino
        | OracleSetup::FixedDrift
        | OracleSetup::FixedJuplend => None,
    }
}

pub fn read_oracle_price<'info>(accounts: &[AccountInfo<'info>]) -> AdapterResult<u128> {
    if accounts.is_empty() {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let bank_ai = &accounts[0];

    if bank_ai.owner != &marginfi_mocks::ID {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let oracle_accounts: &[AccountInfo<'info>] = &accounts[1..];
    let bank_data = bank_ai.try_borrow_data()?;
    let cfg = BankConfigView::try_from_account_data(&bank_data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount)?;

    let setup = cfg
        .oracle_setup()
        .ok_or(AdapterError::OracleSetupUnsupported)?;

    let expected =
        expected_oracle_account_count(setup).ok_or(AdapterError::OracleSetupUnsupported)?;
    if oracle_accounts.len() != expected {
        return Err(AdapterError::OracleSetupUnsupported.into());
    }

    let primary_ai = &oracle_accounts[0];
    if *primary_ai.key != cfg.primary_oracle() {
        return Err(AdapterError::OracleAccountMismatch.into());
    }

    let expected_owner: Pubkey = match setup {
        OracleSetup::PythPushOracle | OracleSetup::StakedWithPythPush => PYTH_PUSH_PROGRAM_ID,
        OracleSetup::SwitchboardPull => SWITCHBOARD_ON_DEMAND_PROGRAM_ID,
        _ => return Err(AdapterError::OracleSetupUnsupported.into()),
    };
    if primary_ai.owner != &expected_owner {
        return Err(AdapterError::OracleAccountMismatch.into());
    }

    let max_age = cfg.oracle_max_age() as i64;
    let oracle_max_confidence = cfg.oracle_max_confidence();
    let now = Clock::get()?.unix_timestamp;

    let oracle_data = primary_ai.try_borrow_data()?;
    let (price_fp48, publish_time) = match setup {
        OracleSetup::PythPushOracle => decode_pyth_push(&oracle_data, oracle_max_confidence)?,
        OracleSetup::SwitchboardPull => {
            decode_switchboard_pull(&oracle_data, oracle_max_confidence)?
        }
        OracleSetup::StakedWithPythPush => {
            let lst_mint_ai = &oracle_accounts[1];
            let stake_state_ai = &oracle_accounts[2];
            require!(
                *lst_mint_ai.key == cfg.oracle_key(1),
                YdeltaError::IncorrectAccount,
                "lst_mint account does not match bank.config.oracle_keys[1]"
            )?;
            require!(
                *stake_state_ai.key == cfg.oracle_key(2),
                YdeltaError::IncorrectAccount,
                "stake_state account does not match bank.config.oracle_keys[2]"
            )?;

            require!(
                *lst_mint_ai.owner == spl_token::id() || *lst_mint_ai.owner == spl_token_2022::id(),
                YdeltaError::IncorrectAccount,
                "lst_mint not owned by an SPL token program (owner={:?})",
                lst_mint_ai.owner
            )?;
            require!(
                *stake_state_ai.owner == solana_program::stake::program::id(),
                YdeltaError::IncorrectAccount,
                "stake_state not owned by the stake program (owner={:?})",
                stake_state_ai.owner
            )?;

            let (lst_supply, lst_decimals) = read_spl_mint_supply_and_decimals(lst_mint_ai)?;
            require!(
                lst_supply > 0,
                AdapterError::OracleSetupUnsupported,
                "LST mint supply is zero — staked oracle path requires non-empty pool"
            )?;

            require!(
                lst_decimals == SOL_DECIMALS,
                AdapterError::OracleSetupUnsupported,
                "LST mint decimals {} != SOL decimals {} — unsupported \
                 staked-LST oracle configuration",
                lst_decimals,
                SOL_DECIMALS
            )?;
            let sol_pool_balance = read_stake_state_v2_delegated_lamports(stake_state_ai)?;

            const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
            let sol_pool_adjusted = sol_pool_balance
                .checked_sub(LAMPORTS_PER_SOL)
                .ok_or(AdapterError::OracleSetupUnsupported)?;

            require!(
                sol_pool_adjusted > 0,
                AdapterError::OracleSetupUnsupported,
                "staked-LST pool has zero backing SOL after the 1-SOL \
                 minimum — refusing to derive a zero price"
            )?;

            let (raw_pyth_fp48, ts) = decode_pyth_push(&oracle_data, oracle_max_confidence)?;

            let adjusted_fp48 = crate::math::mul_div(
                raw_pyth_fp48,
                sol_pool_adjusted as u128,
                lst_supply as u128,
                false,
            )?;
            require!(
                adjusted_fp48 > 0,
                AdapterError::OracleNonPositive,
                "derived staked-LST price is zero"
            )?;
            (adjusted_fp48, ts)
        }
        _ => return Err(AdapterError::OracleSetupUnsupported.into()),
    };

    let effective_max_age = if max_age == 0 {
        DEFAULT_ORACLE_MAX_AGE_SECS
    } else {
        max_age
    };
    let age = now - publish_time;

    if !oracle_timestamp_acceptable(age, effective_max_age) {
        return Err(AdapterError::OracleStale.into());
    }
    Ok(price_fp48)
}

fn oracle_timestamp_acceptable(age: i64, effective_max_age: i64) -> bool {
    // M-23: exclusive `<` matches marginfi's reference check. Pre-fix
    // `<=` gave us a 1-second window of legitimacy that marginfi itself
    // would reject; the inconsistency could surface as an oracle being
    // "fresh enough for yDelta but stale for marginfi".
    age >= -MAX_FUTURE_SKEW_SECS && age < effective_max_age
}

const PYTH_DISC_LEN: usize = 8;
const PYTH_VERIFICATION_LEVEL_OFFSET: usize = PYTH_DISC_LEN + 32;

const PYTH_MIN_EXPONENT: i32 = -12;
const PYTH_MAX_EXPONENT: i32 = 2;

const PYTH_VERIFY_TAG_FULL: u8 = 1;

// M-22 (deferred): pinning the exact on-disk size of Pyth-Full
// requires cross-validation against the Pyth Solana SDK's
// `PriceUpdateV2` layout (which the audit didn't quote a fixed
// number for). The current `data.len() < publish_time_offset + 8`
// check inside `decode_pyth_push` catches truncation but not a
// future longer payload that shifts trailing offsets. Tracked
// here so a future PR can pin the exact `data.len() == EXPECTED`
// once SDK-aligned.

const CONF_INTERVAL_MULTIPLE_BPS: u128 = 21_200;
const STD_DEV_MULTIPLE_BPS: u128 = 19_600;
const BPS_DIVISOR: u128 = 10_000;

const DEFAULT_MAX_CONF_PCT_BPS: u128 = 1_000;
const U32_MAX_AS_U128: u128 = u32::MAX as u128;

/// H-16: PRIVATE byte-offset decoder. The owner check lives in
/// `read_oracle_price` — DO NOT call this from outside the module
/// without first checking the source account is owned by
/// `PYTH_PUSH_PROGRAM_ID`.
fn decode_pyth_push(data: &[u8], oracle_max_confidence_u32: u32) -> AdapterResult<(u128, i64)> {
    if data.len() < PYTH_VERIFICATION_LEVEL_OFFSET + 1 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let verify_tag = data[PYTH_VERIFICATION_LEVEL_OFFSET];

    if verify_tag != PYTH_VERIFY_TAG_FULL {
        return Err(AdapterError::OracleSetupUnsupported.into());
    }

    let price_message_body_offset: usize = 33;
    let price_offset = PYTH_DISC_LEN + price_message_body_offset + 32;
    let conf_offset = price_offset + 8;
    let exponent_offset = conf_offset + 8;
    let publish_time_offset = exponent_offset + 4;

    if data.len() < publish_time_offset + 8 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let price = i64::from_le_bytes(data[price_offset..price_offset + 8].try_into().unwrap());
    let conf = u64::from_le_bytes(data[conf_offset..conf_offset + 8].try_into().unwrap());
    let exponent = i32::from_le_bytes(
        data[exponent_offset..exponent_offset + 4]
            .try_into()
            .unwrap(),
    );
    let publish_time = i64::from_le_bytes(
        data[publish_time_offset..publish_time_offset + 8]
            .try_into()
            .unwrap(),
    );

    if price <= 0 {
        return Err(AdapterError::OracleNonPositive.into());
    }

    if conf == 0 {
        solana_program::msg!("pyth confidence rejection: reported conf is exactly 0");
        return Err(AdapterError::OracleMaxConfidenceExceeded.into());
    }

    if !(PYTH_MIN_EXPONENT..=PYTH_MAX_EXPONENT).contains(&exponent) {
        return Err(AdapterError::OracleSetupUnsupported.into());
    }
    let price_fp48 = scale_to_fp48(price as u128, exponent)?;
    let conf_fp48 = scale_to_fp48(conf as u128, exponent)?;
    check_confidence_interval(
        price_fp48,
        conf_fp48,
        CONF_INTERVAL_MULTIPLE_BPS,
        oracle_max_confidence_u32,
    )?;
    Ok((price_fp48, publish_time))
}

const SWB_DISC_LEN: usize = 8;
const SWB_LAST_UPDATE_TS_OFFSET: usize = SWB_DISC_LEN + 2208;
const SWB_RESULT_VALUE_OFFSET: usize = SWB_DISC_LEN + 2256;
const SWB_RESULT_STD_DEV_OFFSET: usize = SWB_DISC_LEN + 2272;
const SWB_PRECISION: u32 = 18;

const SWB_MIN_SAMPLE_SIZE_OFFSET: usize = SWB_DISC_LEN + 2207;
const SWB_RESULT_NUM_SAMPLES_OFFSET: usize = SWB_DISC_LEN + 2352;

/// H-16: PRIVATE byte-offset decoder. The owner check lives in
/// `read_oracle_price` (the only public entry point) — DO NOT expose
/// this function publicly or call it from outside this module without
/// performing the SWITCHBOARD_ON_DEMAND_PROGRAM_ID owner check on the
/// source account first. Attacker-chosen `result_value` / `std_dev` /
/// `last_update_ts` would flow straight into LTV / liquidation math.
fn decode_switchboard_pull(
    data: &[u8],
    oracle_max_confidence_u32: u32,
) -> AdapterResult<(u128, i64)> {
    if data.len() < SWB_RESULT_NUM_SAMPLES_OFFSET + 1 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }

    let min_sample_size = data[SWB_MIN_SAMPLE_SIZE_OFFSET];
    let num_samples = data[SWB_RESULT_NUM_SAMPLES_OFFSET];
    require!(
        num_samples >= min_sample_size && num_samples > 0,
        AdapterError::OracleSetupUnsupported,
        "switchboard result has {} samples; feed min_sample_size is {}",
        num_samples,
        min_sample_size
    )?;
    let last_update_ts = i64::from_le_bytes(
        data[SWB_LAST_UPDATE_TS_OFFSET..SWB_LAST_UPDATE_TS_OFFSET + 8]
            .try_into()
            .unwrap(),
    );
    let result_value = i128::from_le_bytes(
        data[SWB_RESULT_VALUE_OFFSET..SWB_RESULT_VALUE_OFFSET + 16]
            .try_into()
            .unwrap(),
    );
    let std_dev = i128::from_le_bytes(
        data[SWB_RESULT_STD_DEV_OFFSET..SWB_RESULT_STD_DEV_OFFSET + 16]
            .try_into()
            .unwrap(),
    );
    if result_value <= 0 {
        return Err(AdapterError::OracleNonPositive.into());
    }

    let denom = pow10_u128(SWB_PRECISION)?;

    let price_fp48 = crate::math::mul_div(result_value as u128, 1u128 << 48, denom, false)?;

    let std_dev_fp48 = if std_dev > 0 {
        crate::math::mul_div(std_dev as u128, 1u128 << 48, denom, false)?
    } else {
        0
    };
    check_confidence_interval(
        price_fp48,
        std_dev_fp48,
        STD_DEV_MULTIPLE_BPS,
        oracle_max_confidence_u32,
    )?;
    Ok((price_fp48, last_update_ts))
}

fn check_confidence_interval(
    price_fp48: u128,
    conf_fp48: u128,
    multiplier_bps: u128,
    oracle_max_confidence_u32: u32,
) -> AdapterResult<()> {
    if conf_fp48 == 0 {
        return Ok(());
    }
    let inflated_conf = crate::math::mul_div(conf_fp48, multiplier_bps, BPS_DIVISOR, false)?;
    let max_conf_fp48: u128 = if oracle_max_confidence_u32 > 0 {
        crate::math::mul_div(
            price_fp48,
            oracle_max_confidence_u32 as u128,
            U32_MAX_AS_U128,
            false,
        )?
    } else {
        crate::math::mul_div(price_fp48, DEFAULT_MAX_CONF_PCT_BPS, BPS_DIVISOR, false)?
    };
    if inflated_conf > max_conf_fp48 {
        solana_program::msg!("oracle confidence rejection: conf_inflated > max_allowed");
        return Err(AdapterError::OracleMaxConfidenceExceeded.into());
    }
    Ok(())
}

const SPL_MINT_SUPPLY_OFFSET: usize = 36;
const SPL_MINT_DECIMALS_OFFSET: usize = 44;

const SOL_DECIMALS: u8 = 9;

fn read_spl_mint_supply_and_decimals(mint_ai: &AccountInfo) -> AdapterResult<(u64, u8)> {
    let data = mint_ai.try_borrow_data()?;
    if data.len() < SPL_MINT_DECIMALS_OFFSET + 1 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let supply = u64::from_le_bytes(
        data[SPL_MINT_SUPPLY_OFFSET..SPL_MINT_SUPPLY_OFFSET + 8]
            .try_into()
            .unwrap(),
    );
    let decimals = data[SPL_MINT_DECIMALS_OFFSET];
    Ok((supply, decimals))
}

fn read_stake_state_v2_delegated_lamports(stake_ai: &AccountInfo) -> AdapterResult<u64> {
    let data = stake_ai.try_borrow_data()?;
    let stake_state: StakeStateV2 = solana_program::borsh1::try_from_slice_unchecked(&data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
    match stake_state {
        StakeStateV2::Stake(_meta, stake, _flags) => Ok(stake.delegation.stake),
        _ => Err(AdapterError::OracleSetupUnsupported.into()),
    }
}

fn scale_to_fp48(value: u128, exponent: i32) -> AdapterResult<u128> {
    if exponent >= 0 {
        let factor = pow10_u128(exponent as u32)?;
        let scaled_value = crate::math::mul_div(value, factor, 1, false)?;
        crate::math::to_scaled(scaled_value)
    } else {
        let abs_exp = (-exponent) as u32;
        let denom = pow10_u128(abs_exp)?;
        let scaled = crate::math::mul_div(value, 1u128 << 48, denom, false)?;
        if scaled == 0 {
            return Err(AdapterError::OracleNonPositive.into());
        }
        Ok(scaled)
    }
}

fn pow10_u128(exp: u32) -> AdapterResult<u128> {
    let mut acc: u128 = 1;
    for _ in 0..exp {
        acc = acc
            .checked_mul(10)
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow10_handful_of_values() {
        assert_eq!(pow10_u128(0).unwrap(), 1);
        assert_eq!(pow10_u128(1).unwrap(), 10);
        assert_eq!(pow10_u128(6).unwrap(), 1_000_000);
        assert_eq!(pow10_u128(18).unwrap(), 1_000_000_000_000_000_000);
    }

    #[test]
    fn pow10_overflow_faults_instead_of_saturating() {
        assert!(pow10_u128(38).is_ok());
        assert!(pow10_u128(39).is_err());
        assert!(pow10_u128(u32::MAX).is_err());
    }

    #[test]
    fn scale_to_fp48_unit_price_negative_exponent() {
        let v = scale_to_fp48(1_000_000, -6).unwrap();
        let one = 1u128 << 48;
        let drift = (v as i128 - one as i128).abs();
        assert!(drift < 1024, "got {} expected ~{}", v, one);
    }

    #[test]
    fn scale_to_fp48_positive_exponent_shifts_up() {
        let v = scale_to_fp48(5, 2).unwrap();
        assert_eq!(v, 500u128 << 48);
    }

    fn build_pyth_push_buf(verify_tag: u8) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(134);
        buf.extend_from_slice(&[0u8; 8]);
        buf.extend_from_slice(&[0xAAu8; 32]);
        buf.push(verify_tag);
        if verify_tag == 0 {
            buf.push(5);
        }
        buf.extend_from_slice(&[0xCCu8; 32]);
        buf.extend_from_slice(&100_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&50_u64.to_le_bytes());
        buf.extend_from_slice(&(-8_i32).to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&100_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&50_u64.to_le_bytes());
        buf.extend_from_slice(&12345_u64.to_le_bytes());

        while buf.len() < 134 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn decode_pyth_push_full_variant_passes() {
        let buf = build_pyth_push_buf(1);
        let (price_fp48, ts) = decode_pyth_push(&buf, 0).unwrap();

        let one_fp48 = 1u128 << 48;
        let drift = (price_fp48 as i128 - one_fp48 as i128).abs();
        assert!(drift < 1024, "got {} expected ~{}", price_fp48, one_fp48);
        assert_eq!(ts, 1_700_000_000);
    }

    #[test]
    fn decode_pyth_push_partial_variant_rejected() {
        let buf = build_pyth_push_buf(0);
        assert!(decode_pyth_push(&buf, 0).is_err());
    }

    #[test]
    fn decode_pyth_push_unknown_variant_rejected() {
        let mut buf = build_pyth_push_buf(1);
        buf[40] = 99;
        assert!(decode_pyth_push(&buf, 0).is_err());
    }

    fn build_pyth_full_with(price: i64, conf: u64) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(134);
        buf.extend_from_slice(&[0u8; 8]);
        buf.extend_from_slice(&[0xAAu8; 32]);
        buf.push(1);
        buf.extend_from_slice(&[0xCCu8; 32]);
        buf.extend_from_slice(&price.to_le_bytes());
        buf.extend_from_slice(&conf.to_le_bytes());
        buf.extend_from_slice(&(-8_i32).to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&price.to_le_bytes());
        buf.extend_from_slice(&conf.to_le_bytes());
        buf.extend_from_slice(&12345_u64.to_le_bytes());
        while buf.len() < 134 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn confidence_within_default_10pct_passes() {
        let buf = build_pyth_full_with(100_000_000, 4_000_000);
        assert!(decode_pyth_push(&buf, 0).is_ok());
    }

    #[test]
    fn confidence_above_default_10pct_rejects() {
        let buf = build_pyth_full_with(100_000_000, 10_000_000);
        let err = decode_pyth_push(&buf, 0).unwrap_err();

        assert_eq!(
            err,
            solana_program::program_error::ProgramError::Custom(104)
        );
    }

    #[test]
    fn confidence_exactly_zero_rejected() {
        let buf = build_pyth_full_with(100_000_000, 0);
        let err = decode_pyth_push(&buf, 0).unwrap_err();

        assert_eq!(
            err,
            solana_program::program_error::ProgramError::Custom(104)
        );
    }

    #[test]
    fn check_confidence_interval_pyth_default_path() {
        let one_fp48 = 1u128 << 48;
        let conf_fp48 = one_fp48 / 20;
        assert!(
            check_confidence_interval(one_fp48, conf_fp48, CONF_INTERVAL_MULTIPLE_BPS, 0,).is_err()
        );
    }

    #[test]
    fn check_confidence_interval_swb_default_path() {
        let one_fp48 = 1u128 << 48;
        let conf_fp48 = one_fp48 / 20;
        assert!(check_confidence_interval(one_fp48, conf_fp48, STD_DEV_MULTIPLE_BPS, 0,).is_ok());
    }

    fn build_pyth_full_exponent(price: i64, conf: u64, exponent: i32) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(134);
        buf.extend_from_slice(&[0u8; 8]);
        buf.extend_from_slice(&[0xAAu8; 32]);
        buf.push(1);
        buf.extend_from_slice(&[0xCCu8; 32]);
        buf.extend_from_slice(&price.to_le_bytes());
        buf.extend_from_slice(&conf.to_le_bytes());
        buf.extend_from_slice(&exponent.to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes());
        buf.extend_from_slice(&price.to_le_bytes());
        buf.extend_from_slice(&conf.to_le_bytes());
        buf.extend_from_slice(&12345_u64.to_le_bytes());
        while buf.len() < 134 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn pyth_exponent_within_bounds_accepted() {
        let buf = build_pyth_full_exponent(100_000_000, 50, -8);
        assert!(decode_pyth_push(&buf, 0).is_ok());
    }

    #[test]
    fn pyth_exponent_out_of_range_rejected() {
        for bad_exp in [-30_i32, -13, 3, 50] {
            let buf = build_pyth_full_exponent(100_000_000, 50, bad_exp);
            assert!(
                decode_pyth_push(&buf, 0).is_err(),
                "exponent {} should be rejected",
                bad_exp
            );
        }
    }

    #[test]
    fn future_skew_gate_rejects_future_dated_oracle() {
        let max_age = DEFAULT_ORACLE_MAX_AGE_SECS;

        assert!(oracle_timestamp_acceptable(0, max_age));

        assert!(oracle_timestamp_acceptable(max_age - 1, max_age));

        assert!(!oracle_timestamp_acceptable(max_age + 1, max_age));

        assert!(oracle_timestamp_acceptable(-MAX_FUTURE_SKEW_SECS, max_age));

        assert!(!oracle_timestamp_acceptable(
            -MAX_FUTURE_SKEW_SECS - 1,
            max_age
        ));
        assert!(!oracle_timestamp_acceptable(-86_400, max_age));
    }
}
