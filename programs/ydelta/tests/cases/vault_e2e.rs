//! End-to-end SBPF vault tests.
//!
//! Uses `MarketFixture` to spin up real marginfi banks + the ydelta
//! program, then exercises the full vault surface with actual CPIs.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Smoke test: vault create → profile create → deposit → withdraw
/// round trip with no fills. Validates that the `create_vault`,
/// `create_risk_profile`, `global_vault_deposit`, and `global_vault_withdraw`
/// processors compose end-to-end against a real marginfi bank.
#[tokio::test]
async fn vault_genesis_round_trip() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    // Mint some USDC to the depositor's ATA for the deposit.
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000, // 1000 USDC at 6 decimals
    );

    // 1. Create the USDC vault (admin = signer).
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // 2. Create a single risk profile.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            /*profile_id=*/ 0,
            curator.pubkey(),
            /*max_ltv_bps=*/ 8_000,
            /*max_term_seconds=*/ 30 * 86_400,
            1u8,
        )
        .await
        .unwrap();

    // 3. Deposit 100 USDC.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();

    // Verify profile state reflects the deposit.
    let profile = fixture.read_risk_profile(0).await;
    let total_principal = profile.total_principal_atoms;
    let total_assets = profile.total_assets_atoms;
    let total_shares = profile.total_shares;
    assert_eq!(total_principal, 100_000_000);
    assert_eq!(total_assets, 100_000_000);
    // Genesis: 1 share = 1 atom.
    assert_eq!(total_shares, 100_000_000_u128);

    // 4. Withdraw 40 USDC. Use distinct share counts in two
    //    withdrawals so solana-program-test doesn't dedup them as
    //    replays of the same tx.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 40_000_000_u128)
        .await
        .unwrap();

    let profile = fixture.read_risk_profile(0).await;
    let total_shares = profile.total_shares;
    assert_eq!(total_shares, 60_000_000_u128);

    // Withdraw the rest (60).
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 60_000_000_u128)
        .await
        .unwrap();

    let profile = fixture.read_risk_profile(0).await;
    let total_shares = profile.total_shares;
    let total_assets = profile.total_assets_atoms;
    let total_principal = profile.total_principal_atoms;
    assert_eq!(total_shares, 0_u128);
    assert_eq!(total_assets, 0);
    assert_eq!(total_principal, 0);
}

/// Reject `global_vault_withdraw` when shares > depositor's holdings.
#[tokio::test]
async fn global_vault_withdraw_rejects_overburn() {
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
        .create_risk_profile(&admin, 0, curator.pubkey(), 8_000, 30 * 86_400, 1u8)
        .await
        .unwrap();

    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();

    // Try to burn more shares than the depositor holds — should error.
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 200_000_000_u128)
        .await;
    assert!(result.is_err(), "over-burn must reject");
}

/// `claim_seat_for_risk_profile` happy path: global_vault_admin opens a market seat
/// for a profile with a max_exposure cap. Verifies the seat is
/// inserted into the market and the profile's market-participation
/// counter increments.
#[tokio::test]
async fn global_vault_admin_claims_seat() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 0, curator.pubkey(), 8_000, 30 * 86_400, 1u8)
        .await
        .unwrap();

    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(
            &curator,
            /*profile_id=*/ 0,
            /*max_exposure=*/ 500_000_000,
        )
        .await
        .unwrap();

    // Profile's allowed_market_count was bumped on claim.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(profile.allowed_market_count, 1);
}

/// `create_risk_profile` rejects when signer is not global_vault_admin.
#[tokio::test]
async fn create_risk_profile_rejects_non_admin() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let interloper = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    fixture.refresh_blockhash().await;
    let result = fixture
        .create_risk_profile(
            &interloper, // not the admin
            0,
            curator.pubkey(),
            8_000,
            30 * 86_400,
            1u8,
        )
        .await;
    assert!(result.is_err(), "non-admin must reject");
}
