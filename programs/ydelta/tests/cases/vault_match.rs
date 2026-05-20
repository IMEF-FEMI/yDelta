//! Vault-as-maker matching tests.
//!
//! Verifies the quote-only vault profile order design:
//!   1. A risk profile rests one open-ended (unbounded) ask per market
//!      via `place_order_for_risk_profile` — the vault market-seat is
//!      auto-created on the curator's first such call.
//!   2. A borrower IOC Bid crosses the resting vault ask → the matching
//!      engine records a match and bumps the vault state inline:
//!      `RiskProfile.encumbered_in_orders_atoms += matched`.
//!   3. Each cross is capped by the profile's *live idle balance*
//!      (`total_principal - deployed - encumbered`), not a per-seat cap.
//!   4. The resting risk-profile ask is never removed by the engine —
//!      only the curator removes it.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Vault posts an Ask, borrower's Bid crosses it. Verifies vault state
/// is updated atomically at match time.
#[tokio::test]
async fn vault_ask_crossed_by_borrower_bid_full_fill() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Depositor funds the vault profile with 100 USDC and the curator
    // rests an unbounded ask at 500 bps / 30d.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    // Verify pre-cross profile state — encumbered should be zero
    // (vault hasn't matched yet; just rests open-ended).
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(profile.encumbered_in_orders_atoms, 0);
    assert_eq!(profile.deployed_principal_atoms, 0);

    // Borrower deposits wSOL collateral, then bids to cross the vault
    // ask.
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid: rate ≥ vault.ask.rate (500), term ≤ ask.term. Principal=100.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            /*flags=*/ 0,
        )
        .await
        .unwrap();

    // Post-match: vault profile is encumbered for the matched amount;
    // total principal is untouched until the cranker settles.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, principal_atoms,
        "match-time vault encumbrance should bump encumbered_in_orders_atoms"
    );
    assert_eq!(
        profile.total_principal_atoms, 100_000_000,
        "total_principal_atoms unchanged by match (atoms still in vault.integration)"
    );
}

/// Match-time idle-pool cap: the vault has only N atoms idle but the
/// borrower bids for more. The unbounded vault ask fills only up to the
/// profile's live idle balance — `matched = min(bid, profile_idle)` —
/// and the unfilled remainder of the bid drops (OB_ONLY) instead of
/// failing the whole tx.
#[tokio::test]
async fn vault_match_capped_at_idle_pool() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Deposit only 50 atoms — the profile's idle pool is 50.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            0,
            8_000,
            500,
            30 * 86_400,
            /*deposit_atoms=*/ 50,
        )
        .await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid for 100 atoms — the vault only has 50 idle. The cross fills
    // exactly 50; the 50-atom residual drops (OB_ONLY).
    let result = fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            100,
            5_000,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await;
    assert!(
        result.is_ok(),
        "match-time idle cap (50 idle < 100 bid) must partial-fill, \
         not fail the tx; got: {:?}",
        result
    );

    // Vault profile encumbered for exactly the idle balance — the cross
    // could not exceed it.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, 50,
        "vault cross is capped at the profile's idle balance (50)"
    );
    assert_eq!(
        profile.deployed_principal_atoms, 0,
        "deployed_principal_atoms only bumps when the cranker settles"
    );

    // Exactly one MatchedLoan for the 50-atom partial cross.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "the idle-capped partial cross produces one MatchedLoan",
    );
}

/// Risk-profile orders persist on full-fill: the matching engine never
/// removes them. Only the curator removes via `cancel_order_for_risk_profile`.
#[tokio::test]
async fn risk_profile_order_persists_after_full_fill() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Deposit 100M idle so the cross fully fills.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            0,
            8_000,
            500,
            30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    // 1 USDC principal (1M atoms) at ~$1; vault max_ltv = 80%, so the
    // borrower needs ≥ $1.25 of wSOL collateral. wSOL ≈ $84 → seed
    // 0.05 wSOL (50M atoms = ~$4.20) for safe margin.
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 100_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid for 1M atoms — full-fill match against the unbounded ask.
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            1_000_000,
            50_000_000,
            0,
        )
        .await
        .unwrap();

    // The matching engine matched the bid but must NOT remove the
    // resting risk-profile order.
    let market = fixture.read_market_fixed().await;
    assert_ne!(
        market.asks_best_index,
        hypertree::NIL,
        "risk-profile ask must persist after full-fill (only the curator removes it)",
    );

    // Vault profile encumbered for exactly the matched principal.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, 1_000_000,
        "vault profile encumbered for the full match",
    );
}

/// Once the profile's idle pool is exhausted, the matching engine
/// silently skips subsequent matches — the resting order stays on book,
/// the new bid rests un-matched (with OB_ONLY) instead of erroring.
#[tokio::test]
async fn risk_profile_match_skips_at_idle_exhaustion() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower_a = fixture.create_trader().await;
    let borrower_b = fixture.create_trader().await;

    // Idle pool = 100 atoms. Borrower A will fully consume it.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            0,
            8_000,
            500,
            30 * 86_400,
            /*deposit_atoms=*/ 100,
        )
        .await;

    // Borrower A: takes the entire idle pool.
    fixture.claim_seat(&borrower_a).await;
    let a_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(a_wsol, borrower_a.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower_a, a_wsol, false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &borrower_a,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            100,
            5_000,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Borrower B: tries to match, but the profile's idle pool is now
    // fully encumbered. Use OB_ONLY so the unfilled residual rests
    // instead of falling into the P2Pool fallback. Match-time should
    // silently skip the now-idle-exhausted risk-profile maker.
    fixture.claim_seat(&borrower_b).await;
    let b_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(b_wsol, borrower_b.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower_b, b_wsol, false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let result = fixture
        .place_order_with_flags(
            &borrower_b,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            50,
            2_500,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await;
    assert!(
        result.is_ok(),
        "exhausted idle pool must skip the maker, not error; got {:?}",
        result,
    );

    // Only one MatchedLoan was created (borrower A's). Borrower B's
    // bid did not match.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "only borrower A's match should produce a MatchedLoan; borrower B's bid skipped past the idle-exhausted maker",
    );

    // The profile is encumbered at exactly the deposited idle amount.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(profile.encumbered_in_orders_atoms, 100);
    let _ = mainnet::usdc_mint();
}

/// `global_vault_withdraw` must gate per-profile.
///
/// Profile 0 deposits 1.5 USDC and a borrower draws a 1.0-USDC loan that
/// the cranker SETTLES — those 1.0 USDC physically leave the shared
/// marginfi integration account, leaving profile 0 with only 0.5 USDC
/// idle. Profile 1 separately deposits 5 USDC, all still sitting idle in
/// the SAME marginfi account.
///
/// Profile 0's depositor then tries to redeem all their shares (~1.5
/// USDC). The shared marginfi balance (5.5 USDC) would physically cover
/// it — gating only on the vault-wide marginfi balance would let
/// profile 0 drain 1.0 USDC that economically backs profile 1. The
/// per-profile idle gate must REJECT: profile 0's own idle is only
/// 0.5 USDC.
#[tokio::test]
async fn vault_withdraw_per_profile_gate_rejects_cross_profile_drain() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor_0 = fixture.create_trader().await;
    let depositor_1 = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    let token_0 = fixture.signer_debt_token(&depositor_0.pubkey());
    let token_1 = fixture.signer_debt_token(&depositor_1.pubkey());
    fixture.put_token_account(
        token_0,
        mainnet::usdc_mint(),
        depositor_0.pubkey(),
        1_000_000_000,
    );
    fixture.put_token_account(
        token_1,
        mainnet::usdc_mint(),
        depositor_1.pubkey(),
        1_000_000_000,
    );

    // Profile 0: deposit 1.5 USDC and rest an unbounded ask. The
    // `provide_vault_liquidity` helper runs create_vault →
    // create_risk_profile → deposit → place_order.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor_0,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 1_500_000,
        )
        .await;

    // Profile 1: a second profile in the same vault with 5 USDC idle.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, 1, curator.pubkey(), 8_000, 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_1, token_1, 1, 5_000_000)
        .await
        .unwrap();

    // Borrower draws a 1.0-USDC loan against profile 0's ask.
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 200_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(
            &borrower,
            borrower_wsol,
            /*is_debt=*/ false,
            50_000_000,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            /*principal=*/ 1_000_000,
            /*collateral=*/ 50_000_000,
            /*flags=*/ 0,
        )
        .await
        .unwrap();

    // Settle the matched loan — moves the 1.0 USDC out of the shared
    // marginfi account and bumps profile 0's deployed_principal_atoms.
    fixture.refresh_blockhash().await;
    fixture
        .crank_matched_loan_for_risk_profile(0)
        .await
        .unwrap();

    let p0 = fixture.read_risk_profile(0).await;
    assert_eq!(
        p0.deployed_principal_atoms, 1_000_000,
        "loan settled: 1.0 USDC of profile 0's principal is deployed",
    );
    // Profile 0's own idle is total_principal − deployed − encumbered.
    let p0_idle = p0
        .total_principal_atoms
        .saturating_sub(p0.deployed_principal_atoms)
        .saturating_sub(p0.encumbered_in_orders_atoms);
    assert_eq!(p0_idle, 500_000, "profile 0 has only 0.5 USDC idle");

    // Profile 0's depositor tries to redeem ALL their shares (~1.5 USDC
    // of assets). The shared marginfi balance (~5.5 USDC) would cover it
    // physically, but profile 0's own idle is only 0.5 USDC. The
    // per-profile gate must reject.
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_withdraw(&depositor_0, token_0, 0, p0.total_shares)
        .await;
    assert!(
        result.is_err(),
        "withdrawing past profile 0's own idle (0.5 USDC) must reject \
         even though the shared marginfi balance covers it — that capital \
         backs profile 1",
    );

    // A withdrawal WITHIN profile 0's idle (0.4 USDC) still succeeds.
    fixture.refresh_blockhash().await;
    let ok = fixture
        .global_vault_withdraw(&depositor_0, token_0, 0, 400_000_u128)
        .await;
    assert!(
        ok.is_ok(),
        "a withdrawal within profile 0's idle must still succeed; got {:?}",
        ok,
    );

    // Profile 1's capital is untouched.
    let p1 = fixture.read_risk_profile(1).await;
    assert_eq!(
        p1.total_principal_atoms, 5_000_000,
        "profile 1's principal must be untouched by profile 0's withdrawal",
    );
}
