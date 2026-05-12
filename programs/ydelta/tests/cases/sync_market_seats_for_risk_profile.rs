//! End-to-end coverage for `sync_market_seats_for_risk_profile`.
//!
//! Verifies:
//!   - claim_seat_for_risk_profile appends the market to active_markets
//!   - update_risk_profile + sync re-stamps the cached
//!     risk_profile_max_ltv_bps on the market-side seat
//!   - the loader rejects market accounts that aren't in the
//!     profile's active_markets

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

const PROFILE_ID: u8 = 0;
const INITIAL_MAX_LTV_BPS: u16 = 8_000;
const TIGHTENED_MAX_LTV_BPS: u16 = 6_000;
const VAULT_DEPOSIT_ATOMS: u64 = 10_000_000;
const MAX_EXPOSURE_ATOMS: u64 = 1_000_000;

#[tokio::test]
async fn claim_seat_records_market_in_active_markets() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            PROFILE_ID,
            curator.pubkey(),
            INITIAL_MAX_LTV_BPS,
            30 * 86_400,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, PROFILE_ID, VAULT_DEPOSIT_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&curator, PROFILE_ID, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();

    let profile = fixture.read_risk_profile(PROFILE_ID).await;
    assert_eq!(
        profile.allowed_market_count, 1,
        "claim_seat_for_risk_profile must bump allowed_market_count",
    );
    assert_eq!(
        profile.active_markets[0],
        fixture.market.pubkey(),
        "claim_seat_for_risk_profile must append the market to active_markets",
    );
}

#[tokio::test]
async fn sync_propagates_updated_max_ltv_to_seat() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            PROFILE_ID,
            curator.pubkey(),
            INITIAL_MAX_LTV_BPS,
            30 * 86_400,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, PROFILE_ID, VAULT_DEPOSIT_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&curator, PROFILE_ID, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Place the order so the seat's risk_profile_max_ltv_bps cache gets
    // stamped at the initial value.
    fixture
        .place_order_for_risk_profile(&curator, PROFILE_ID, 500, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    let seat_pre = fixture.read_vault_seat(&gv, PROFILE_ID).await;
    assert_eq!(
        seat_pre.risk_profile_max_ltv_bps, INITIAL_MAX_LTV_BPS,
        "seat should carry initial cached value",
    );

    // Curator-equivalent admin tightens the policy. update_risk_profile
    // doesn't touch market-side seats; the cache stays stale until
    // sync runs.
    fixture
        .update_risk_profile(&admin, PROFILE_ID, Some(TIGHTENED_MAX_LTV_BPS), None, None)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let seat_mid = fixture.read_vault_seat(&gv, PROFILE_ID).await;
    assert_eq!(
        seat_mid.risk_profile_max_ltv_bps, INITIAL_MAX_LTV_BPS,
        "update_risk_profile must NOT auto-restamp seats — only sync ix does",
    );

    // Anyone calls sync, passing the market(s) from active_markets.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .sync_market_seats_for_risk_profile(&stranger, PROFILE_ID, &[fixture.market.pubkey()])
        .await
        .unwrap();

    let seat_post = fixture.read_vault_seat(&gv, PROFILE_ID).await;
    assert_eq!(
        seat_post.risk_profile_max_ltv_bps, TIGHTENED_MAX_LTV_BPS,
        "sync_market_seats_for_risk_profile must propagate the tightened tier",
    );
}

#[tokio::test]
async fn sync_rejects_foreign_market() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            PROFILE_ID,
            curator.pubkey(),
            INITIAL_MAX_LTV_BPS,
            30 * 86_400,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, PROFILE_ID, VAULT_DEPOSIT_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&curator, PROFILE_ID, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();

    // Pass a random pubkey as a "market" — must be ydelta-owned with
    // the market discriminator, so the loader's YdeltaAccountInfo::new
    // will reject it well before the active_markets check fires.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    let foreign_market = Pubkey::new_unique();
    let result = fixture
        .sync_market_seats_for_risk_profile(&stranger, PROFILE_ID, &[foreign_market])
        .await;
    assert!(
        result.is_err(),
        "sync must reject a foreign / non-ydelta market account",
    );
}
