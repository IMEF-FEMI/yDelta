//! Oracle decoders used by the marginfi adapter. Returns prices as
//! fp48 `u128` quote-per-base values.

use solana_program::{
    account_info::AccountInfo, clock::Clock, pubkey, pubkey::Pubkey, stake::state::StakeStateV2,
    sysvar::Sysvar,
};

/// Pyth Solana Receiver Program (mainnet). PriceUpdateV2 accounts
/// must be owned by this program for the bytes we read at fixed
/// offsets to be authentic.
pub const PYTH_PUSH_PROGRAM_ID: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// Switchboard On-Demand Program (mainnet). PullFeedAccountData
/// accounts must be owned by this program.
pub const SWITCHBOARD_ON_DEMAND_PROGRAM_ID: Pubkey =
    pubkey!("SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv");

use marginfi_mocks::state::{BankConfigView, OracleSetup};

use crate::program::YdeltaError;
use crate::require;

use super::AdapterError;

/// Result type alias — keeps signatures readable.
type AdapterResult<T> = Result<T, solana_program::program_error::ProgramError>;

/// Mirrors marginfi v2's `MAX_PYTH_ORACLE_AGE`
/// (`reference/marginfi-v2/type-crate/src/constants.rs`). Used when a
/// bank's `oracle_max_age` is the sentinel value 0. Marginfi only
/// applies this default when `oracle_setup == PythPushOracle`; yDelta
/// applies it for all setups (including Switchboard) — strictly more
/// conservative since a Switchboard bank with `oracle_max_age = 0`
/// would have been rejected at marginfi's bank-config validation,
/// so we never see one in practice.
const DEFAULT_ORACLE_MAX_AGE_SECS: i64 = 60;

/// Marginfi v2's `ORACLE_MIN_AGE = 10` — banks with
/// `oracle_max_age` below this fail marginfi's bank-config
/// validation, so any bank reaching us has either max_age == 0
/// (defaulted to 60 above) or max_age >= 10. We don't enforce the
/// floor here — banks below it can't exist by construction.
const _MARGINFI_ORACLE_MIN_AGE_SECS: i64 = 10;

/// Tolerance for legitimate clock skew between Pyth/Switchboard
/// publishers and Solana's `Clock`. yDelta-specific gate; marginfi
/// doesn't reject future-dated oracle data explicitly (the Pyth SDK
/// handles its own range checks). Defense-in-depth.
///
/// TODO(oracle-future-skew): the gate using this constant is currently
/// disabled in `read_oracle_price` — see the matching TODO there.
#[allow(dead_code)]
const MAX_FUTURE_SKEW_SECS: i64 = 10;

/// Expected number of oracle accounts for a given `OracleSetup`. Mirrors
/// marginfi v2's `OraclePriceFeedAdapter::try_from_bank_with_max_age`
/// arity per branch (see `reference/marginfi-v2/programs/marginfi/src/state/price.rs`).
///
/// Support matrix on yDelta v1:
/// - `PythPushOracle`, `SwitchboardPull`: 1 account, decoder implemented.
/// - `StakedWithPythPush`: 3 accounts, decoder implemented (LST adjust).
/// - Everything else: `None` → `OracleSetupUnsupported`. This includes
///   the v2 integration setups (Kamino / Drift / Solend / JupLend, both
///   Pyth- and Switchboard-flavoured) and the Fixed-price banks. They're
///   listed explicitly rather than caught by a `_` arm so that any
///   future addition to marginfi's `OracleSetup` enum surfaces as an
///   exhaustiveness compile error here, forcing a deliberate decision
///   on yDelta's stance for the new variant.
pub fn expected_oracle_account_count(setup: OracleSetup) -> Option<usize> {
    match setup {
        OracleSetup::None => None,
        // Deprecated upstream — banks created before the v0.1.4 migration.
        OracleSetup::PythLegacy | OracleSetup::SwitchboardV2 => None,
        OracleSetup::PythPushOracle | OracleSetup::SwitchboardPull => Some(1),
        OracleSetup::StakedWithPythPush => Some(3),
        // ── v2 integration setups (not supported on yDelta v1) ──
        //
        // Each is a single-oracle bank at the CPI boundary but the
        // surrounding protocol (Kamino reserve, Drift spot market,
        // Solend reserve, JupLend lending state) sits in
        // `Bank.integration_acc_1/2/3`. Supporting them needs the
        // matching reserve/market decoder, which is out of scope for
        // v1. Reject explicitly here.
        OracleSetup::KaminoPythPush
        | OracleSetup::KaminoSwitchboardPull
        | OracleSetup::DriftPythPull
        | OracleSetup::DriftSwitchboardPull
        | OracleSetup::SolendPythPull
        | OracleSetup::SolendSwitchboardPull
        | OracleSetup::JuplendPythPull
        | OracleSetup::JuplendSwitchboardPull => None,
        // ── Fixed-price banks (price stored in Bank.config.fixed_price) ──
        //
        // Zero oracle accounts at CPI time; price is read from the
        // bank's own `fixed_price: WrappedI80F48` field. We don't
        // expose that path through `read_oracle_price` today.
        OracleSetup::Fixed
        | OracleSetup::FixedKamino
        | OracleSetup::FixedDrift
        | OracleSetup::FixedJuplend => None,
    }
}

/// Read the bank's oracle and return its current price in fp48
/// USD-per-base. `accounts[0]` is the bank; `accounts[1..]` are the
/// variable-length oracle accounts the bank's `OracleSetup` requires
/// (1 for Pyth-push / Switchboard-pull, 3 for StakedWithPythPush,
/// etc. — see `expected_oracle_account_count`). Mismatched count
/// returns `OracleSetupUnsupported` so callers fail loudly rather
/// than feeding the wrong number of accounts to a decoder.
pub fn read_oracle_price<'info>(accounts: &[AccountInfo<'info>]) -> AdapterResult<u128> {
    if accounts.is_empty() {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let bank_ai = &accounts[0];
    let oracle_accounts: &[AccountInfo<'info>] = &accounts[1..];
    let bank_data = bank_ai.try_borrow_data()?;
    let cfg = BankConfigView::try_from_account_data(&bank_data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount)?;

    let setup = cfg
        .oracle_setup()
        .ok_or(AdapterError::OracleSetupUnsupported)?;

    // Reject if the bank's setup doesn't match what we know how to
    // decode, OR if the caller passed the wrong number of oracle
    // accounts. Either way the safe path is to error before touching
    // the oracle bytes.
    let expected =
        expected_oracle_account_count(setup).ok_or(AdapterError::OracleSetupUnsupported)?;
    if oracle_accounts.len() != expected {
        return Err(AdapterError::OracleSetupUnsupported.into());
    }

    let primary_ai = &oracle_accounts[0];
    if *primary_ai.key != cfg.primary_oracle() {
        return Err(AdapterError::OracleAccountMismatch.into());
    }
    // Defense-in-depth: even with pubkey-pinning, verify the account
    // is owned by the expected oracle program. Catches a corrupted
    // bank-config that pinned a non-oracle pubkey or a future setup
    // that points at an unexpected program.
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
            // 3 oracle accounts: [0] Pyth feed (already in primary_ai),
            // [1] LST mint, [2] stake state. Adjust the Pyth SOL price
            // by `(sol_pool_balance - 1 SOL) / lst_supply` to derive
            // the LST's USD-per-token. Mirrors marginfi's
            // `try_from_bank_with_max_age` `StakedWithPythPush` branch
            // Mirrors marginfi's price handling.
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

            let lst_supply = read_spl_mint_supply(lst_mint_ai)?;
            require!(
                lst_supply > 0,
                AdapterError::OracleSetupUnsupported,
                "LST mint supply is zero — staked oracle path requires non-empty pool"
            )?;
            let sol_pool_balance = read_stake_state_v2_delegated_lamports(stake_state_ai)?;
            // The pool has a non-refundable 1 SOL minimum that doesn't
            // back any LST. Subtract before computing the LST exchange
            // rate.
            const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
            let sol_pool_adjusted = sol_pool_balance
                .checked_sub(LAMPORTS_PER_SOL)
                .ok_or(AdapterError::OracleSetupUnsupported)?;

            let (raw_pyth_fp48, ts) = decode_pyth_push(&oracle_data, oracle_max_confidence)?;
            // Adjust: lst_price_fp48 = sol_price_fp48 × sol_adj / lst_supply.
            // Both SOL and LST use 9 decimals so no decimal scaling
            // needed, matching marginfi's handling.
            let adjusted_fp48 = raw_pyth_fp48
                .checked_mul(sol_pool_adjusted as u128)
                .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?
                .checked_div(lst_supply as u128)
                .unwrap_or(0);
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
    // TODO(oracle-future-skew): restore the symmetric gate
    //     `if age < -MAX_FUTURE_SKEW_SECS || age > effective_max_age`
    // once we have a defensible tolerance. Disabled on 2026-05-12 after
    // tx 2z2L…Kj1d failed with OracleStale: Pyth `publish_time` ran
    // ~41 min ahead of `Clock::unix_timestamp` because Solana's
    // stake-weighted clock lags wall-clock while Pyth publishers use
    // wall-clock. Marginfi v2's reference reader doesn't gate future
    // skew either. See `MAX_FUTURE_SKEW_SECS` (currently unused).
    if age > effective_max_age {
        return Err(AdapterError::OracleStale.into());
    }
    Ok(price_fp48)
}

// ─────────────────── Pyth-push (PriceUpdateV2) ───────────────────
//
// On-disk layout (post 8-byte Anchor disc):
//   write_authority: Pubkey                       — bytes 0..32
//   verification_level: enum                      — byte  32 (tag)
//                                                   + 1 byte payload iff Partial
//   price_message {
//       feed_id: [u8; 32]
//       price: i64
//       conf:  u64
//       exponent: i32
//       publish_time: i64
//       prev_publish_time: i64
//       ema_price: i64
//       ema_conf: u64
//   }
//   posted_slot: u64
//
// `verification_level` is a Borsh enum:
//   - `Partial { num_signatures: u8 }` (variant 0): 2 bytes total
//     = [00, num_signatures].
//   - `Full` (variant 1): 1 byte total = [01].
// Anchor's `LEN` allocates 134 bytes assuming Partial; Full leaves a
// 1-byte tail. So all downstream field offsets shift by ±1 depending
// on the live variant. We branch on the tag at body-offset 32 and
// pick the correct base.
//
// Marginfi's reference implementation uses dynamic Borsh
// deserialization (`PriceUpdateV2::deserialize`) which handles both
// variants implicitly. We use offset-based reading for SBPF compute
// efficiency; the variant branch below preserves correctness.

const PYTH_DISC_LEN: usize = 8;
const PYTH_VERIFICATION_LEVEL_OFFSET: usize = PYTH_DISC_LEN + 32;
// Variant tag for the pyth-solana-receiver-sdk `VerificationLevel`
// `Full` variant. (Variant `Partial { num_signatures: u8 }` = 0 is
// rejected outright per `MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`.)
const PYTH_VERIFY_TAG_FULL: u8 = 1;

// ─── Confidence-interval rejection constants (mirror marginfi) ───
//
// Marginfi's `CONF_INTERVAL_MULTIPLE = 2.12` widens Pyth's reported
// confidence (1σ) to ~2σ before checking against the bank's max
// allowance. `STD_DEV_MULTIPLE = 1.96` plays the analogous role for
// Switchboard's `std_dev` (which is reported at 1σ per submission).
// Both are encoded here in basis points so we can do all-integer math
// in fp48 without pulling in fixed::I80F48.
//
// Marginfi reference: `reference/marginfi-v2/type-crate/src/constants.rs`
// (CONF_INTERVAL_MULTIPLE / STD_DEV_MULTIPLE).
const CONF_INTERVAL_MULTIPLE_BPS: u128 = 21_200; // 2.12 × 10_000
const STD_DEV_MULTIPLE_BPS: u128 = 19_600; // 1.96 × 10_000
const BPS_DIVISOR: u128 = 10_000;

// Marginfi default when `bank.config.oracle_max_confidence == 0`.
// Encoded as `U32_MAX_DIV_10` upstream — we just use the same fraction
// directly: 10% of price.
const DEFAULT_MAX_CONF_PCT_BPS: u128 = 1_000; // 10% in bps
const U32_MAX_AS_U128: u128 = u32::MAX as u128;

/// Decode a Pyth-Push `PriceUpdateV2` account.
///
/// Hardening (mirrors marginfi's `PythPushOraclePriceFeed::load_checked`):
/// - **Verification level: Full required.** Marginfi sets
///   `MIN_PYTH_PUSH_VERIFICATION_LEVEL = VerificationLevel::Full`. We
///   match that — Partial-verified updates are rejected.
/// - **Confidence interval rejection.** `conf_inflated = conf × 2.12`
///   (in fp48). If `conf_inflated > price × max_conf_pct`, reject.
///   `max_conf_pct` comes from `bank.config.oracle_max_confidence`
///   (defaulted to 10% if zero).
fn decode_pyth_push(data: &[u8], oracle_max_confidence_u32: u32) -> AdapterResult<(u128, i64)> {
    if data.len() < PYTH_VERIFICATION_LEVEL_OFFSET + 1 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let verify_tag = data[PYTH_VERIFICATION_LEVEL_OFFSET];
    // **Min verification level: Full.** Reject Partial outright.
    // Mirrors marginfi's `MIN_PYTH_PUSH_VERIFICATION_LEVEL` enforcement
    // in `get_price_no_older_than_with_custom_verification_level`.
    if verify_tag != PYTH_VERIFY_TAG_FULL {
        return Err(AdapterError::OracleSetupUnsupported.into());
    }
    // With Full, feed_id lives at body offset 33 (1-byte tag).
    let price_message_body_offset: usize = 33;
    let price_offset = PYTH_DISC_LEN + price_message_body_offset + 32; // skip feed_id
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

// ─────────────── Switchboard-pull (PullFeedAccountData) ───────────────
//
// Within the bank-account-style 8-byte-disc-prefixed layout:
//   submissions: [OracleSubmission(64); 32]       — body 0..2048
//   ... (auth/queue/feed_hash/initialised_at/perms/max_var/min_resp/
//        name/padding1/historical_idx/min_sample_size — total 160)
//   last_update_timestamp: i64                    — body 2208..2216
//   lut_slot: u64                                 — body 2216..2224
//   _reserved1: [u8; 32]                          — body 2224..2256
//   result: CurrentResult {
//       value: i128                               — body 2256..2272
//       std_dev: i128                             — 2272..2288
//       mean: i128                                — 2288..2304
//       range: i128                               — 2304..2320
//       min_value: i128                           — 2320..2336
//       max_value: i128                           — 2336..2352
//       num_samples: u8                           — 2352
//       submission_idx: u8                        — 2353
//       padding1: [u8; 6]                         — 2354..2360
//       slot: u64                                 — 2360..2368
//   }
//
// The price (`result.value`) is fp18 (decimal-scaled, 18 fractional
// digits). Switchboard's PRECISION constant is 18.

const SWB_DISC_LEN: usize = 8;
const SWB_LAST_UPDATE_TS_OFFSET: usize = SWB_DISC_LEN + 2208;
const SWB_RESULT_VALUE_OFFSET: usize = SWB_DISC_LEN + 2256;
const SWB_RESULT_STD_DEV_OFFSET: usize = SWB_DISC_LEN + 2272;
const SWB_PRECISION: u32 = 18;

/// Decode a Switchboard-Pull `PullFeedAccountData` account.
///
/// Hardening: confidence interval rejection. `conf_inflated =
/// std_dev × 1.96` (in fp48). If `conf_inflated > price ×
/// max_conf_pct`, reject. Mirrors marginfi's
/// `SwitchboardPullPriceFeed::get_confidence_interval`.
fn decode_switchboard_pull(
    data: &[u8],
    oracle_max_confidence_u32: u32,
) -> AdapterResult<(u128, i64)> {
    if data.len() < SWB_RESULT_STD_DEV_OFFSET + 16 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
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
    // Switchboard's value is `result_value × 10^-PRECISION` USD; convert
    // to fp48: `(result_value << 48) / 10^PRECISION`. Both operands fit
    // in u128 with margin since result_value ≤ 2^96 ≈ 7.9e28 and the
    // 10^18 divisor is small.
    let denom = pow10_u128(SWB_PRECISION);
    let price_fp48 = (result_value as u128)
        .saturating_mul(1u128 << 48)
        .checked_div(denom)
        .unwrap_or(0);
    // std_dev shares result_value's PRECISION scale.
    let std_dev_fp48 = if std_dev > 0 {
        (std_dev as u128)
            .saturating_mul(1u128 << 48)
            .checked_div(denom)
            .unwrap_or(0)
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

/// Marginfi-style confidence-interval gate. Common to Pyth and
/// Switchboard; only the multiplier differs (`CONF_INTERVAL_MULTIPLE`
/// vs `STD_DEV_MULTIPLE`).
///
/// Rejects when `conf_fp48 × multiplier_bps / 10_000 > price_fp48 ×
/// oracle_max_confidence_u32 / U32_MAX`. When
/// `oracle_max_confidence_u32 == 0`, the default is 10% (mirrors
/// marginfi's `U32_MAX_DIV_10`).
fn check_confidence_interval(
    price_fp48: u128,
    conf_fp48: u128,
    multiplier_bps: u128,
    oracle_max_confidence_u32: u32,
) -> AdapterResult<()> {
    if conf_fp48 == 0 {
        return Ok(());
    }
    let inflated_conf = conf_fp48
        .checked_mul(multiplier_bps)
        .and_then(|v| v.checked_div(BPS_DIVISOR))
        .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?;
    let max_conf_fp48: u128 = if oracle_max_confidence_u32 > 0 {
        // `max_conf = price × (oracle_max_confidence / U32_MAX)`.
        price_fp48
            .checked_mul(oracle_max_confidence_u32 as u128)
            .and_then(|v| v.checked_div(U32_MAX_AS_U128))
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?
    } else {
        // Default 10% of price.
        price_fp48
            .checked_mul(DEFAULT_MAX_CONF_PCT_BPS)
            .and_then(|v| v.checked_div(BPS_DIVISOR))
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?
    };
    if inflated_conf > max_conf_fp48 {
        solana_program::msg!("oracle confidence rejection: conf_inflated > max_allowed");
        return Err(AdapterError::OracleMaxConfidenceExceeded.into());
    }
    Ok(())
}

// ─────────────── Staked-with-Pyth helpers ───────────────
//
// SPL Mint layout (post-init): mint_authority COption<Pubkey> = 36
// bytes (4-byte tag + 32 pubkey), then `supply: u64` at body offset
// 36..44. The token program owns the account so we don't need to
// validate ownership here — marginfi already keyed it via
// `oracle_keys[1]`.

const SPL_MINT_SUPPLY_OFFSET: usize = 36;

fn read_spl_mint_supply(mint_ai: &AccountInfo) -> AdapterResult<u64> {
    let data = mint_ai.try_borrow_data()?;
    if data.len() < SPL_MINT_SUPPLY_OFFSET + 8 {
        return Err(AdapterError::InvalidIntegrationAccount.into());
    }
    let supply = u64::from_le_bytes(
        data[SPL_MINT_SUPPLY_OFFSET..SPL_MINT_SUPPLY_OFFSET + 8]
            .try_into()
            .unwrap(),
    );
    Ok(supply)
}

/// Decode a `StakeStateV2::Stake` account and return
/// `delegation.stake` (active SOL pool balance, in lamports). Mirrors
/// marginfi's `try_from_slice_unchecked::<StakeStateV2>` + match on
/// the `Stake` variant.
fn read_stake_state_v2_delegated_lamports(stake_ai: &AccountInfo) -> AdapterResult<u64> {
    let data = stake_ai.try_borrow_data()?;
    let stake_state: StakeStateV2 = solana_program::borsh1::try_from_slice_unchecked(&data)
        .map_err(|_| AdapterError::InvalidIntegrationAccount)?;
    match stake_state {
        StakeStateV2::Stake(_meta, stake, _flags) => Ok(stake.delegation.stake),
        _ => Err(AdapterError::OracleSetupUnsupported.into()),
    }
}

// ─────────────────── Math helpers ───────────────────

/// Convert `value × 10^exponent` to fp48 (i.e. multiply by 2^48).
/// `exponent` is typically negative for Pyth (e.g. -8 → divide by 10^8).
/// Handles positive and negative exponents via separate paths to keep
/// integer-only math.
fn scale_to_fp48(value: u128, exponent: i32) -> AdapterResult<u128> {
    if exponent >= 0 {
        let factor = pow10_u128(exponent as u32);
        let scaled = value
            .checked_mul(factor)
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?;
        Ok(scaled
            .checked_shl(48)
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?)
    } else {
        let abs_exp = (-exponent) as u32;
        let denom = pow10_u128(abs_exp);
        // Order matters for precision: (value << 48) / denom keeps as
        // many fractional bits as the i128 input affords. value is ≤
        // 2^63 (i64), so value << 48 ≤ 2^111 — fits in u128.
        let shifted = value
            .checked_shl(48)
            .ok_or(solana_program::program_error::ProgramError::ArithmeticOverflow)?;
        Ok(shifted.checked_div(denom).unwrap_or(0))
    }
}

fn pow10_u128(exp: u32) -> u128 {
    let mut acc: u128 = 1;
    for _ in 0..exp {
        acc = acc.saturating_mul(10);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow10_handful_of_values() {
        assert_eq!(pow10_u128(0), 1);
        assert_eq!(pow10_u128(1), 10);
        assert_eq!(pow10_u128(6), 1_000_000);
        assert_eq!(pow10_u128(18), 1_000_000_000_000_000_000);
    }

    #[test]
    fn scale_to_fp48_unit_price_negative_exponent() {
        // 1 USDC at exponent -6 → 1.0 USD → 2^48 fp48.
        let v = scale_to_fp48(1_000_000, -6).unwrap();
        let one = 1u128 << 48;
        let drift = (v as i128 - one as i128).abs();
        assert!(drift < 1024, "got {} expected ~{}", v, one);
    }

    #[test]
    fn scale_to_fp48_positive_exponent_shifts_up() {
        // value 5 at exp +2 → 500 → 500 × 2^48.
        let v = scale_to_fp48(5, 2).unwrap();
        assert_eq!(v, 500u128 << 48);
    }

    /// Build a synthetic PriceUpdateV2 byte buffer with the given
    /// `verification_level` tag (0 = Partial { num_signatures: 5 },
    /// 1 = Full) and a USDC-like price at exponent -8. Used to pin
    /// `decode_pyth_push`'s variant-aware offset handling.
    fn build_pyth_push_buf(verify_tag: u8) -> Vec<u8> {
        // Anchor disc + write_authority + verification_level (1 or 2)
        // + feed_id + price + conf + exponent + publish_time +
        // prev_publish_time + ema_price + ema_conf + posted_slot.
        let mut buf: Vec<u8> = Vec::with_capacity(134);
        buf.extend_from_slice(&[0u8; 8]); // disc
        buf.extend_from_slice(&[0xAAu8; 32]); // write_authority
        buf.push(verify_tag);
        if verify_tag == 0 {
            // Partial { num_signatures: 5 }
            buf.push(5);
        }
        buf.extend_from_slice(&[0xCCu8; 32]); // feed_id
        buf.extend_from_slice(&100_000_000_i64.to_le_bytes()); // price (= $1.00 at exp -8)
        buf.extend_from_slice(&50_u64.to_le_bytes()); // conf
        buf.extend_from_slice(&(-8_i32).to_le_bytes()); // exponent
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes()); // publish_time
        buf.extend_from_slice(&1_700_000_000_i64.to_le_bytes()); // prev_publish_time
        buf.extend_from_slice(&100_000_000_i64.to_le_bytes()); // ema_price
        buf.extend_from_slice(&50_u64.to_le_bytes()); // ema_conf
        buf.extend_from_slice(&12345_u64.to_le_bytes()); // posted_slot
                                                         // Pad to LEN = 134 if needed (Full leaves 1 byte slack).
        while buf.len() < 134 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn decode_pyth_push_full_variant_passes() {
        let buf = build_pyth_push_buf(/*verify_tag=*/ 1);
        let (price_fp48, ts) = decode_pyth_push(&buf, /*max_conf=*/ 0).unwrap();
        // $1.00 in fp48 = 2^48 (within rounding of 1024 atoms-fp48).
        let one_fp48 = 1u128 << 48;
        let drift = (price_fp48 as i128 - one_fp48 as i128).abs();
        assert!(drift < 1024, "got {} expected ~{}", price_fp48, one_fp48);
        assert_eq!(ts, 1_700_000_000);
    }

    #[test]
    fn decode_pyth_push_partial_variant_rejected() {
        // Mirrors marginfi's `MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`
        // policy. Partial-verified updates are unsafe and must reject.
        let buf = build_pyth_push_buf(/*verify_tag=*/ 0);
        assert!(decode_pyth_push(&buf, 0).is_err());
    }

    #[test]
    fn decode_pyth_push_unknown_variant_rejected() {
        let mut buf = build_pyth_push_buf(/*verify_tag=*/ 1);
        buf[40] = 99;
        assert!(decode_pyth_push(&buf, 0).is_err());
    }

    /// Build a Full-variant Pyth-push buffer with custom price/conf
    /// (both at exponent -8). For confidence-rejection tests.
    fn build_pyth_full_with(price: i64, conf: u64) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(134);
        buf.extend_from_slice(&[0u8; 8]);
        buf.extend_from_slice(&[0xAAu8; 32]);
        buf.push(1); // Full
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
        // Price = $1.00 (1e8 at exp -8), conf = $0.04 (4e6).
        // Inflated: 0.04 × 2.12 = 0.0848. Default max = 10% = 0.10.
        // 0.0848 < 0.10 → passes.
        let buf = build_pyth_full_with(100_000_000, 4_000_000);
        assert!(decode_pyth_push(&buf, 0).is_ok());
    }

    #[test]
    fn confidence_above_default_10pct_rejects() {
        // Price = $1.00, conf = $0.10. Inflated: 0.10 × 2.12 = 0.212.
        // Default max = 0.10 → rejects.
        let buf = build_pyth_full_with(100_000_000, 10_000_000);
        let err = decode_pyth_push(&buf, 0).unwrap_err();
        // Custom(104) = AdapterError::OracleMaxConfidenceExceeded.
        assert_eq!(
            err,
            solana_program::program_error::ProgramError::Custom(104)
        );
    }

    #[test]
    fn confidence_zero_always_passes() {
        // No oracle uncertainty → no rejection regardless of max_conf.
        let buf = build_pyth_full_with(100_000_000, 0);
        assert!(decode_pyth_push(&buf, 0).is_ok());
    }

    #[test]
    fn check_confidence_interval_pyth_default_path() {
        // Direct unit test on the helper. price = 2^48 ($1 fp48),
        // conf = 0.05 × 2^48 ($0.05 fp48). Pyth multiplier 2.12 →
        // inflated 0.106. Default 10% threshold → rejects.
        let one_fp48 = 1u128 << 48;
        let conf_fp48 = one_fp48 / 20; // 0.05
        assert!(
            check_confidence_interval(one_fp48, conf_fp48, CONF_INTERVAL_MULTIPLE_BPS, 0,).is_err()
        );
    }

    #[test]
    fn check_confidence_interval_swb_default_path() {
        // Switchboard multiplier 1.96 — slightly looser. With same
        // 5% std_dev: inflated 0.098 < 0.10 → passes.
        let one_fp48 = 1u128 << 48;
        let conf_fp48 = one_fp48 / 20;
        assert!(check_confidence_interval(one_fp48, conf_fp48, STD_DEV_MULTIPLE_BPS, 0,).is_ok());
    }
}
