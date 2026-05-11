//! Vault-as-maker matching tests.
//!
//! Verifies the open-ended vault profile order design:
//!   1. Vault places an Ask without per-seat encumbrance.
//!   2. Borrower's Bid crosses → matching engine records a
//!      `VaultMatchDelta`, processor applies it atomically:
//!      `RiskProfile.encumbered_in_orders_atoms += matched` (vault state),
//!      market-side `ClaimedSeat.deployed_atoms() += matched` (market state).
//!   3. Cranker promotes via `do_vault_settle`: 3-CPI atom migration
//!      `vault.integration → market.lender_integration`, plus
//!      `encumbered -= matched` and `deployed_principal_atoms += matched`.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Vault posts an Ask, borrower's Bid crosses fully. Verifies vault
/// state is updated atomically at match time.
#[tokio::test]
async fn vault_ask_crossed_by_borrower_bid_full_fill() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Depositor funds the vault profile with 100 USDC.
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
            /*profile_id=*/ 0,
            curator.pubkey(),
            /*max_ltv_bps=*/ 8_000,
            /*max_term_seconds=*/ 30 * 86_400,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(
            &depositor,
            depositor_token,
            /*profile_id=*/ 0,
            100_000_000,
        )
        .await
        .unwrap();

    // Open the vault's per-market seat with a generous exposure cap.
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(
            &admin, /*profile_id=*/ 0, /*max_exposure=*/ 50_000_000,
        )
        .await
        .unwrap();

    // Curator places an open-ended Ask on the market — no commit
    // step required; the order's principal cap = max_exposure_atoms.
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(
            &curator,
            /*profile_id=*/ 0,
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*flags=*/ 0,
        )
        .await
        .unwrap();

    // Verify pre-cross profile state — encumbered should be zero
    // (vault hasn't matched yet; just rests open-ended).
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(profile.encumbered_in_orders_atoms, 0);
    assert_eq!(profile.deployed_principal_atoms, 0);

    // Borrower deposits wSOL collateral, then bids to cross the
    // vault's ask.
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid: rate ≥ vault.ask.rate (500), term ≤ ask.term, principal
    // ≤ vault.max_exposure (50M). Use principal=100, collateral=5000.
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

    // Post-match: vault state should be encumbered for the matched
    // amount, deployed_atoms on the seat bumped.
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

/// Match-time idle-pool gate: vault has only N atoms idle but the
/// matched principal would exceed it. Match-time check fails (vault
/// orders are open-ended; the gate fires at match, not at place).
#[tokio::test]
async fn vault_match_rejected_when_idle_pool_exceeded() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

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
    // Deposit only 50 atoms — idle pool is 50.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 50)
        .await
        .unwrap();
    // Generous exposure cap so this test exercises the idle gate.
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 0, /*max_exposure=*/ 1_000_000)
        .await
        .unwrap();
    // Vault places ask with principal=max_exposure (1M atoms — far
    // larger than the 50-atom idle pool).
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(&curator, 0, 500, 30 * 86_400, 0)
        .await
        .unwrap();

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid for 100 atoms — vault only has 50 idle. The matching loop
    // SKIPS the vault maker (instead of failing the whole tx) and
    // the bid rests un-matched. OB_ONLY flag set so the residual
    // doesn't fall through to P2Pool fallback (which would bump
    // matched_loan_sequence and create a MatchedLoan).
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
        "match-time idle gate (50 idle < 100 matched) must skip the maker, \
         not fail the tx; got: {:?}",
        result
    );

    // Vault profile state untouched by the skipped match.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, 0,
        "skipped vault maker must not encumber idle atoms"
    );
    assert_eq!(
        profile.deployed_principal_atoms, 0,
        "skipped vault maker must not deploy principal"
    );

    // The bid rested on the book — no MatchedLoan created.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 0,
        "no MatchedLoan should have been allocated for the skipped match"
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
    // Deposit ≥ max_exposure so the idle gate can admit a full-cap match.
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, 0, /*max_exposure=*/ 1_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(&curator, 0, 500, 30 * 86_400, 0)
        .await
        .unwrap();

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

    // Bid for the full max_exposure (1_000_000) — full-fill match.
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

    // The matching engine bumps seat.deployed_atoms to max_exposure
    // but must NOT remove the resting risk-profile order.
    let market = fixture.read_market_fixed().await;
    assert_ne!(
        market.asks_best_index,
        hypertree::NIL,
        "risk-profile ask must persist after full-fill (only the curator removes it)",
    );

    // Seat usage saturated at the cap.
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    let seat = fixture.read_vault_seat(&gv, 0).await;
    assert_eq!(
        seat.deployed_atoms(),
        1_000_000,
        "seat.deployed_atoms must reflect the full match",
    );
}

/// Once `seat.deployed_atoms == max_exposure_atoms`, the matching
/// engine silently skips subsequent matches — the resting order
/// stays on book, the new bid rests un-matched (with OB_ONLY flag)
/// instead of erroring.
#[tokio::test]
async fn risk_profile_match_skips_at_cap_saturation() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower_a = fixture.create_trader().await;
    let borrower_b = fixture.create_trader().await;

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
        .global_vault_deposit(&depositor, depositor_token, 0, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Cap = 100. First borrower will fully saturate it.
    fixture
        .claim_seat_for_risk_profile(&admin, 0, /*max_exposure=*/ 100)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(&curator, 0, 500, 30 * 86_400, 0)
        .await
        .unwrap();

    // Borrower A: takes the entire cap.
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

    // Borrower B: tries to match, but the seat is saturated at the
    // cap. Use OB_ONLY so the unfilled residual rests instead of
    // falling into the P2Pool fallback. Match-time should silently
    // skip the saturated risk-profile maker.
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
        "saturated cap must skip the maker, not error; got {:?}",
        result,
    );

    // Only one MatchedLoan was created (borrower A's). Borrower B's
    // bid did not match.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "only borrower A's match should produce a MatchedLoan; borrower B's bid skipped past the saturated maker",
    );

    // The seat is at exactly the cap.
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    let seat = fixture.read_vault_seat(&gv, 0).await;
    assert_eq!(seat.deployed_atoms(), 100);
    assert_eq!(seat.max_exposure_atoms(), 100);
}
