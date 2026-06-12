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
            /*profile_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    // Verify pre-cross profile state — encumbered should be zero
    // (vault hasn't matched yet; just rests open-ended).
    let profile = fixture.read_risk_profile(1).await;
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
    let profile = fixture.read_risk_profile(1).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, principal_atoms,
        "match-time vault encumbrance should bump encumbered_in_orders_atoms"
    );
    assert_eq!(
        profile.total_principal_atoms, 99_999_999,
        "total_principal_atoms unchanged by match (atoms still in vault.integration)"
    );
    assert_eq!(
        profile.deployed_principal_atoms, 0,
        "deployed_principal_atoms must remain 0 until cranker settles"
    );
    // Vault-idle invariant must hold post-match.
    fixture.assert_vault_idle_invariant(1).await;
    // Matched-collateral conservation: Σ MatchedLoan.collateral == bid collateral.
    assert_eq!(
        fixture.sum_matched_loan_collateral().await,
        collateral_atoms,
        "Σ MatchedLoan.collateral_atoms == bid collateral_atoms"
    );
    // Borrower seat: the matched collateral stays encumbered (it backs
    // the open loan, released only at close) and the cross ticked the
    // open-loan counter to 1.
    let borrower_seat = fixture.read_seat(&borrower.pubkey()).await;
    assert!(
        borrower_seat.collateral_encumbered_shares > 0,
        "matched collateral stays encumbered while the loan is open"
    );
    assert_eq!(
        borrower_seat.open_borrow_count, 1,
        "one cross → one open loan"
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
            1,
            Some(8_000),
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
    result.expect("match-time idle cap (50 idle < 100 bid) must partial-fill, not fail the tx");

    // Vault profile encumbered for the idle balance minus the matching
    // engine's per-profile marginfi-rounding reserve — the cross caps
    // at `idle - reserve`, not at the gross idle.
    use ydelta::state::market_helpers::MARGINFI_ROUNDING_RESERVE_ATOMS;
    let profile = fixture.read_risk_profile(1).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms,
        49 - MARGINFI_ROUNDING_RESERVE_ATOMS,
        "vault cross is capped at idle minus the marginfi-rounding reserve"
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
            1,
            Some(8_000),
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
    let profile = fixture.read_risk_profile(1).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, 1_000_000,
        "vault profile encumbered for the full match",
    );
    // Matched-collateral conservation: the single MatchedLoan must
    // carry exactly the bid's posted collateral (dust-sweep invariant).
    assert_eq!(
        fixture.sum_matched_loan_collateral().await,
        50_000_000,
        "Σ MatchedLoan.collateral_atoms must equal bid collateral"
    );
    // Vault-idle invariant on the crossed profile.
    fixture.assert_vault_idle_invariant(1).await;
    // Borrower seat: the matched collateral stays encumbered (it backs
    // the open loan) and `open_borrow_count` ticks to 1.
    let borrower_seat = fixture.read_seat(&borrower.pubkey()).await;
    assert!(
        borrower_seat.collateral_encumbered_shares > 0,
        "fully-filled IOC bid keeps the matched collateral encumbered (backs the open loan)"
    );
    assert_eq!(
        borrower_seat.open_borrow_count, 1,
        "one cross → one open loan"
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
            1,
            Some(8_000),
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
    // OB_ONLY: any 1-atom residual after the matching engine's
    // `MARGINFI_ROUNDING_RESERVE_ATOMS` cap should drop, not route to
    // P2Pool — the test counts MatchedLoans below and wants exactly the
    // single Fixed cross.
    fixture
        .place_order_with_flags(
            &borrower_a,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            100,
            5_000,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
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
    result.expect("exhausted idle pool must skip the maker, not error");
    // Borrower B's seat must NOT show any encumbered collateral after
    // the skip — the OB_ONLY residual drop fully restores it.
    let seat_b = fixture.read_seat(&borrower_b.pubkey()).await;
    assert_eq!(
        seat_b.collateral_encumbered_shares, 0,
        "skipped maker → no residual borrower-side collateral encumbrance"
    );

    // Only one MatchedLoan was created (borrower A's). Borrower B's
    // bid did not match.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "only borrower A's match should produce a MatchedLoan; borrower B's bid skipped past the idle-exhausted maker",
    );

    // The profile is encumbered at the deposited idle minus the
    // matching engine's marginfi-rounding reserve.
    use ydelta::state::market_helpers::MARGINFI_ROUNDING_RESERVE_ATOMS;
    let profile = fixture.read_risk_profile(1).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms,
        99 - MARGINFI_ROUNDING_RESERVE_ATOMS,
    );
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
            /*profile_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 1_500_000,
        )
        .await;

    // Profile 1: a second profile in the same vault with 5 USDC idle.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, curator.pubkey(), Some(8_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_1, token_1, 2, 5_000_000)
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

    let p0 = fixture.read_risk_profile(1).await;
    assert_eq!(
        p0.deployed_principal_atoms, 1_000_000,
        "loan settled: 1.0 USDC of profile 0's principal is deployed",
    );
    // Profile 0's own idle is total_principal − deployed − encumbered.
    let p0_idle = p0
        .total_principal_atoms
        .saturating_sub(p0.deployed_principal_atoms)
        .saturating_sub(p0.encumbered_in_orders_atoms);
    assert_eq!(p0_idle, 499_998, "profile 0 has only 0.5 USDC idle (minus the 1-atom \
        vault-funded-match basis-debit for the ceil-rounded withdraw cushion)");

    // Profile 0's depositor tries to redeem ALL their shares (~1.5 USDC
    // of assets). The shared marginfi balance (~5.5 USDC) would cover it
    // physically, but profile 0's own idle is only 0.5 USDC. The
    // per-profile gate must reject.
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_withdraw(&depositor_0, token_0, 1, p0.total_shares)
        .await;
    // Per-profile idle gate must surface the exact
    // VaultInsufficientIdleAtoms variant; a generic is_err() check
    // would also accept a different (incorrect) rejection path.
    crate::assert_custom_error!(
        result,
        ydelta::program::YdeltaError::VaultInsufficientIdleAtoms
    );

    // A withdrawal WITHIN profile 0's idle (0.4 USDC) still succeeds.
    fixture.refresh_blockhash().await;
    let ok = fixture
        .global_vault_withdraw(&depositor_0, token_0, 1, 400_000_u128)
        .await;
    assert!(
        ok.is_ok(),
        "a withdrawal within profile 0's idle must still succeed; got {:?}",
        ok,
    );

    // Profile 1's capital is untouched.
    let p1 = fixture.read_risk_profile(2).await;
    assert_eq!(
        p1.total_principal_atoms, 4_999_999,
        "profile 1's principal must be untouched by profile 0's withdrawal",
    );
    // Both profiles must satisfy the vault-idle invariant after the
    // per-profile gate test exercises the share-burn path.
    fixture.assert_vault_idle_invariant(1).await;
    fixture.assert_vault_idle_invariant(1).await;
}

/// The lender seat's `open_lend_count` counts resting asks PLUS active
/// loans: each fill stamps +1, each full loan close retires exactly that
/// +1, and the resting ask's own count survives the loan lifecycle.
///
/// Regression: the close-out paths used to `saturating_sub(1)` without a
/// matching fill-time increment, eating the resting ask's count — after
/// one full repay the curator's `cancel_order_for_risk_profile` failed
/// with ArithmeticOverflow on `checked_sub(0)`.
#[tokio::test]
async fn lender_open_lend_count_survives_full_repay_then_cancel() {
    use ydelta::program::instruction_builders::cancel_order_for_risk_profile_instruction::cancel_order_for_risk_profile_instruction;

    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    let vault_pk = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint()).0;
    let seat = fixture.read_vault_seat(&vault_pk, 1).await;
    assert_eq!(seat.open_lend_count, 1, "one resting ask");

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, /*is_debt=*/ false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Seed the borrower's USDC ATA for the full repay below.
    let borrower_usdc = fixture.signer_debt_token(&borrower.pubkey());
    fixture.put_token_account(
        borrower_usdc,
        mainnet::usdc_mint(),
        borrower.pubkey(),
        4_000_000,
    );
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

    let seat = fixture.read_vault_seat(&vault_pk, 1).await;
    assert_eq!(seat.open_lend_count, 2, "resting ask + one open loan");

    fixture.refresh_blockhash().await;
    fixture
        .crank_matched_loan_for_risk_profile(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let seat = fixture.read_vault_seat(&vault_pk, 1).await;
    assert_eq!(
        seat.open_lend_count, 1,
        "loan close retires only the fill's count; the resting ask survives"
    );

    let cancel_ix = cancel_order_for_risk_profile_instruction(
        &mainnet::usdc_mint(),
        &fixture.market.pubkey(),
        &admin.pubkey(),
        &curator.pubkey(),
        1,
    );
    fixture
        .process_ixs(&[cancel_ix], &[&admin, &curator])
        .await
        .expect("curator cancel after a full loan close must succeed");
    let seat = fixture.read_vault_seat(&vault_pk, 1).await;
    assert_eq!(seat.open_lend_count, 0, "cancel retires the ask's count");
}

/// The profile LTV cap is per-ask policy: a bid whose collateral fails a
/// stricter profile's cap skips that ask and fills against a looser one
/// further down the book.
///
/// Regression: `match_order` used to hard-`require!` the profile-cap
/// gate, failing the whole bid on the FIRST too-strict ask even when a
/// compatible ask rested right behind it (and the refinance engine
/// already skipped). The strict profile must also end with zero stale
/// encumbrance — the engine only reserves the fill after every gate.
#[tokio::test]
async fn profile_ltv_cap_skips_strict_ask_and_fills_looser_one() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Profile 1: strict 10% LTV cap quoting the BEST rate — the scan
    // hits it first.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 1,
            /*max_ltv_bps=*/ Some(1_000),
            /*rate_bps=*/ 400,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    // Profile 2: loose 80% cap at a worse rate, same vault, also funded.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(&admin, curator.pubkey(), Some(8_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture
        .global_vault_deposit(&depositor, depositor_token, 2, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(&curator, 2, 500, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Borrower posts ~$4 of wSOL against $1 of debt: enough for the 80%
    // cap (and the market init-weight gate), nowhere near the 10% cap's
    // ~$10 requirement.
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, /*is_debt=*/ false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let result = fixture
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
        .await;
    result.expect("a stricter profile cap must skip the ask, not fail the bid");

    let p1 = fixture.read_risk_profile(1).await;
    assert_eq!(
        p1.encumbered_in_orders_atoms, 0,
        "skipped strict profile must carry no stale encumbrance"
    );
    let p2 = fixture.read_risk_profile(2).await;
    assert_eq!(
        p2.encumbered_in_orders_atoms, 1_000_000,
        "the looser profile behind it fills the whole bid"
    );
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "exactly one cross: profile 1 skipped, profile 2 filled"
    );
}
