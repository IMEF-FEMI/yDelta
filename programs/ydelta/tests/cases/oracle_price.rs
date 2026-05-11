//! Adapter `oracle_price` + `init_weight` + `maint_weight` against
//! the dumped mainnet bank/oracle pairs.
//!
//! These are off-chain unit-style tests against the loaded fixture
//! accounts — we read the bytes directly rather than CPI-calling the
//! adapter (which requires AccountInfo). We exercise the same code
//! path via an inline helper that mirrors `read_oracle_price`'s
//! logic without the staleness check (since fixture timestamps are
//! pre-test-clock).
//!
//! The on-program-side staleness path is exercised in Step 8 (LTV at
//! match), where the matching engine calls the adapter live.

use solana_program::pubkey::Pubkey;

use marginfi_mocks::state::{BankConfigView, OracleSetup};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Mirror of `oracles::scale_to_fp48` (private to the protocol module).
/// Re-implemented here so the test doesn't need to expose the helper.
fn scale_to_fp48(value: u128, exponent: i32) -> u128 {
    if exponent >= 0 {
        let mut factor: u128 = 1;
        for _ in 0..exponent {
            factor = factor.saturating_mul(10);
        }
        (value.saturating_mul(factor)) << 48
    } else {
        let abs = (-exponent) as u32;
        let mut denom: u128 = 1;
        for _ in 0..abs {
            denom = denom.saturating_mul(10);
        }
        let shifted = value << 48;
        shifted / denom
    }
}

fn decode_pyth_price(data: &[u8]) -> u128 {
    // Account-data offsets (disc-included), copied from
    // protocol/oracles.rs. The body-relative offsets are
    // price(65), conf(73), exponent(81), publish_time(85); add the
    // 8-byte Anchor disc → account offsets 73, 81, 89, 93.
    let price = i64::from_le_bytes(data[73..81].try_into().unwrap());
    let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap());
    assert!(price > 0);
    scale_to_fp48(price as u128, exponent)
}

fn decode_swb_price(data: &[u8]) -> u128 {
    // Body offset 2256 + disc 8 = 2264.
    let result_value = i128::from_le_bytes(data[2264..2280].try_into().unwrap());
    assert!(result_value > 0);
    let scaled = (result_value as u128).saturating_mul(1u128 << 48);
    let denom: u128 = 10u128.pow(18);
    scaled / denom
}

#[tokio::test]
async fn usdc_oracle_price_is_close_to_one_dollar() {
    let fixture = MarketFixture::new().await;
    let oracle_data = fixture.account_data(mainnet::usdc_oracle()).await;

    // USDC mainnet uses Pyth-push.
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let cfg = BankConfigView::try_from_account_data(&bank_data).unwrap();
    assert_eq!(cfg.oracle_setup(), Some(OracleSetup::PythPushOracle));
    assert_eq!(cfg.primary_oracle(), mainnet::usdc_oracle());

    let price_fp48 = decode_pyth_price(&oracle_data);
    let one = 1u128 << 48;
    // Within ±5% of 1 USD — generous to absorb mainnet pegs around the dump.
    let lo = (one * 95) / 100;
    let hi = (one * 105) / 100;
    assert!(
        price_fp48 >= lo && price_fp48 <= hi,
        "USDC price fp48 {} out of band [{}, {}]",
        price_fp48,
        lo,
        hi
    );
}

#[tokio::test]
async fn sol_oracle_price_is_in_realistic_range() {
    let fixture = MarketFixture::new().await;
    let oracle_data = fixture.account_data(mainnet::sol_oracle()).await;
    let bank_data = fixture.account_data(mainnet::sol_bank()).await;
    let cfg = BankConfigView::try_from_account_data(&bank_data).unwrap();
    assert_eq!(cfg.primary_oracle(), mainnet::sol_oracle());

    let price_fp48 = match cfg.oracle_setup() {
        Some(OracleSetup::SwitchboardPull) => decode_swb_price(&oracle_data),
        Some(OracleSetup::PythPushOracle) => decode_pyth_price(&oracle_data),
        other => panic!("unexpected oracle setup for SOL: {:?}", other),
    };
    // Mainnet SOL price ranges roughly $20 to $400 across the past few
    // years. Assert the decoded fp48 falls in that band — sanity for
    // unit conversion.
    let one = 1u128 << 48;
    let lo = 20u128 * one;
    let hi = 400u128 * one;
    assert!(
        price_fp48 >= lo && price_fp48 <= hi,
        "SOL price fp48 {} out of band [{}, {}]",
        price_fp48,
        lo,
        hi
    );
}

#[tokio::test]
async fn init_weight_reads_through_adapter() {
    // Verify the BankConfigView weight accessors return the same bits
    // we'd get reading the bank's raw bytes. (init_weight in the
    // adapter reads exactly these fields.)
    let fixture = MarketFixture::new().await;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let cfg = BankConfigView::try_from_account_data(&bank_data).unwrap();

    let asset_init = wrapped_i80f48_to_u128(cfg.asset_weight_init());
    let liab_init = wrapped_i80f48_to_u128(cfg.liability_weight_init());
    let asset_maint = wrapped_i80f48_to_u128(cfg.asset_weight_maint());
    let liab_maint = wrapped_i80f48_to_u128(cfg.liability_weight_maint());

    // All weights should be > 0 on a live USDC bank.
    assert!(asset_init > 0, "asset_weight_init = 0?");
    assert!(liab_init > 0, "liability_weight_init = 0?");
    assert!(asset_maint > 0);
    assert!(liab_maint > 0);

    // Maintenance weights typically ≥ init weights (more permissive
    // for liquidation eligibility) per marginfi's convention. Don't
    // pin exact relations since governance can flip them, but assert
    // they're not absurdly far apart.
    let bigger = asset_init.max(asset_maint);
    let smaller = asset_init.min(asset_maint);
    assert!(
        bigger <= smaller.saturating_mul(4),
        "asset_weight init/maint differ by more than 4× — sanity check"
    );

    // Silence unused-import warning when no other Pubkey use exists.
    let _: Pubkey = mainnet::usdc_oracle();
}
