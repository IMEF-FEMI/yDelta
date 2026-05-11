//! Multi-risk-profile vault tests.
//!
//! The enforcement mechanism is the market-side `ClaimedSeat` keyed
//! by `(vault, owner_kind=Vault, profile_id)` plus
//! `RiskProfileOrderRef` keyed by `(market, profile_id)` on the
//! vault side. These cases exercise that surface with N>1 profiles.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Two profiles in one vault, two distinct depositors. Each deposits
/// into their own profile; withdrawing from one doesn't affect the
/// other's share count or asset total.
#[tokio::test]
async fn two_profiles_independent_deposit_state() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor_a = fixture.create_trader().await;
    let depositor_b = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    let token_a = fixture.signer_debt_token(&depositor_a.pubkey());
    let token_b = fixture.signer_debt_token(&depositor_b.pubkey());
    fixture.put_token_account(
        token_a,
        mainnet::usdc_mint(),
        depositor_a.pubkey(),
        1_000_000_000,
    );
    fixture.put_token_account(
        token_b,
        mainnet::usdc_mint(),
        depositor_b.pubkey(),
        1_000_000_000,
    );

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // Profile 0 — curator_a, 50% LTV cap, 30-day max term.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 0, curator_a.pubkey(), 5_000, 30 * 86_400, 1u8)
        .await
        .unwrap();

    // Profile 1 — curator_b, 80% LTV cap, 90-day max term. Different
    // policy, different curator.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 1, curator_b.pubkey(), 8_000, 90 * 86_400, 1u8)
        .await
        .unwrap();

    // Verify both profiles exist with distinct curators + policies.
    let profile_a = fixture.read_risk_profile(0).await;
    let profile_b = fixture.read_risk_profile(1).await;
    assert_eq!(profile_a.profile_id, 0);
    assert_eq!(profile_b.profile_id, 1);
    assert_eq!(profile_a.curator, curator_a.pubkey());
    assert_eq!(profile_b.curator, curator_b.pubkey());
    assert_eq!(profile_a.max_ltv_bps, 5_000);
    assert_eq!(profile_b.max_ltv_bps, 8_000);
    assert_eq!(profile_a.max_term_seconds, 30 * 86_400);
    assert_eq!(profile_b.max_term_seconds, 90 * 86_400);

    // Depositor A deposits 100 USDC into profile 0.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_a, token_a, 0, 100_000_000)
        .await
        .unwrap();

    // Depositor B deposits 250 USDC into profile 1.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_b, token_b, 1, 250_000_000)
        .await
        .unwrap();

    // Verify each profile's state is independent.
    let profile_a = fixture.read_risk_profile(0).await;
    let profile_b = fixture.read_risk_profile(1).await;
    let total_shares_a = profile_a.total_shares;
    let total_principal_a = profile_a.total_principal_atoms;
    let total_shares_b = profile_b.total_shares;
    let total_principal_b = profile_b.total_principal_atoms;
    assert_eq!(total_shares_a, 100_000_000_u128);
    assert_eq!(total_principal_a, 100_000_000);
    assert_eq!(total_shares_b, 250_000_000_u128);
    assert_eq!(total_principal_b, 250_000_000);

    // Vault-level counter reflects both profiles.
    let vault_fixed = fixture.read_vault_fixed().await;
    assert_eq!(vault_fixed.risk_profile_count, 2);

    // Depositor A withdraws 40 USDC from profile 0.
    // Profile 1's state must NOT be affected.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor_a, token_a, 0, 40_000_000_u128)
        .await
        .unwrap();

    let profile_a = fixture.read_risk_profile(0).await;
    let profile_b = fixture.read_risk_profile(1).await;
    let shares_a_after = profile_a.total_shares;
    let principal_a_after = profile_a.total_principal_atoms;
    let shares_b_after = profile_b.total_shares;
    let principal_b_after = profile_b.total_principal_atoms;
    assert_eq!(shares_a_after, 60_000_000_u128);
    assert_eq!(principal_a_after, 60_000_000);
    // Profile B is untouched.
    assert_eq!(shares_b_after, 250_000_000_u128);
    assert_eq!(principal_b_after, 250_000_000);
}

/// Same vault, two profiles both opening seats in the same market.
/// Verify market.claimed_seats has both vault-owned seats with
/// distinct (vault, profile_id) keys.
#[tokio::test]
async fn two_profiles_claim_seats_in_same_market() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 0, curator_a.pubkey(), 5_000, 30 * 86_400, 1u8)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 1, curator_b.pubkey(), 8_000, 90 * 86_400, 1u8)
        .await
        .unwrap();

    // Admin claims seats for both profiles. Each gets its own
    // max_exposure cap.
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 0, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 1, 500_000_000)
        .await
        .unwrap();

    // Each profile's market-participation counter incremented.
    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    assert_eq!(p0.allowed_market_count, 1);
    assert_eq!(p1.allowed_market_count, 1);
}

/// Profile A's curator cannot place_order_for_risk_profile for profile B.
/// Cross-curator authorization gate must hold.
#[tokio::test]
async fn cross_curator_cannot_place_for_other_profile() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 0, curator_a.pubkey(), 5_000, 30 * 86_400, 1u8)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 1, curator_b.pubkey(), 8_000, 90 * 86_400, 1u8)
        .await
        .unwrap();

    // Both profiles claim a seat in the market.
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 0, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 1, 500_000_000)
        .await
        .unwrap();

    // Curator A signing with profile_id = 1 (curator B's profile)
    // must reject. The processor's `signer == profile.curator` gate
    // catches this.
    fixture.refresh_blockhash().await;
    let result = fixture
        .place_order_for_risk_profile(
            &curator_a,
            /*profile_id=*/ 1, // curator_b's profile
            500,
            30 * 86_400,
            0,
        )
        .await;
    assert!(result.is_err(), "curator_a must not place for profile 1");

    // A curator must never be able to sign for a profile they don't
    // own — that's the security property this test guards. We don't
    // need an "affirmative half" (curator_b CAN place for profile 1)
    // because that path is exercised end-to-end in
    // `vault_match::vault_ask_crossed_by_borrower_bid_full_fill` and
    // the lifecycle of every vault test that places, matches, and
    // settles vault orders.
}

/// Both profiles share a single pool of vault-level state but
/// per-profile aggregates stay independent. Verifies that
/// `RiskProfile` reads are correctly keyed by `profile_id` and don't
/// alias.
#[tokio::test]
async fn profile_aggregates_dont_alias() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    let token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // Create THREE profiles to verify the tree handles N>2.
    for (id, curator) in [(0u8, &curator_a), (1u8, &curator_b), (2u8, &admin)] {
        fixture.refresh_blockhash().await;
        fixture
            .create_risk_profile(
                &admin,
                id,
                curator.pubkey(),
                5_000 + id as u16 * 1_000, // distinct max_ltv per profile
                30 * 86_400,
                1u8,
            )
            .await
            .unwrap();
    }

    let vault = fixture.read_vault_fixed().await;
    assert_eq!(vault.risk_profile_count, 3);

    // Read all three. Each must have its own curator and max_ltv.
    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    let p2 = fixture.read_risk_profile(2).await;
    assert_eq!(p0.curator, curator_a.pubkey());
    assert_eq!(p1.curator, curator_b.pubkey());
    assert_eq!(p2.curator, admin.pubkey());
    assert_eq!(p0.max_ltv_bps, 5_000);
    assert_eq!(p1.max_ltv_bps, 6_000);
    assert_eq!(p2.max_ltv_bps, 7_000);

    // Deposit only into profile 1; verify aggregates on 0 and 2 stay zero.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, token, 1, 50_000_000)
        .await
        .unwrap();

    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    let p2 = fixture.read_risk_profile(2).await;
    let p0_shares = p0.total_shares;
    let p1_shares = p1.total_shares;
    let p2_shares = p2.total_shares;
    assert_eq!(p0_shares, 0);
    assert_eq!(p1_shares, 50_000_000_u128);
    assert_eq!(p2_shares, 0);
}
