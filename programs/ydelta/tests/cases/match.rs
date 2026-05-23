//! Matching-engine mechanics for the quote-only book.
//!
//! The only resting orders are vault risk-profile asks; the taker is
//! always a borrower IOC bid. These
//! tests pin the cross mechanics — best-ask-first ordering, the
//! term-incompatible walk-past, partial fills, and the vault-side
//! match-time encumbrance bump — using one ask per (profile, market).
//!
//! Setup mirrors `vault_match.rs`: a vault is funded, one or more risk
//! profiles each post an open-ended ask, and a borrower bids to cross
//! them.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Spin up a vault with `n` risk profiles, each funded and resting one
/// open-ended ask at the supplied `(rate_bps, term_seconds, idle_atoms)`.
/// Returns the borrower keypair after seeding its wSOL collateral seat.
///
/// Vault asks are unbounded in the quote-only model — each cross is
/// capped only by the profile's live idle balance (`idle_atoms` here).
async fn vault_with_asks(
    fixture: &MarketFixture,
    profile_specs: &[(u16, u32, u64)],
) -> solana_sdk::signature::Keypair {
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        100_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    for (i, (rate_bps, term_seconds, idle_atoms)) in profile_specs.iter().enumerate() {
        let profile_id = i as u8;
        let curator = fixture.create_trader().await;
        fixture.refresh_blockhash().await;
        fixture
            .create_risk_profile(
                &admin,
                curator.pubkey(),
                /*max_ltv_bps=*/ 8_000,
                /*max_term_seconds=*/ 90 * 86_400,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;
        fixture
            .global_vault_deposit(&depositor, depositor_token, profile_id, *idle_atoms)
            .await
            .unwrap();
        fixture.refresh_blockhash().await;
        // No claim-seat step — the vault market-seat is auto-created on
        // the curator's first place_order_for_risk_profile.
        fixture
            .place_order_for_risk_profile(&curator, profile_id, *rate_bps, *term_seconds, 0)
            .await
            .unwrap();
    }

    let borrower = fixture.create_trader().await;
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    // The required collateral is denominated in wSOL lamports — backing
    // tens of dollars of USDC debt against ~$100-200 SOL needs billions
    // of lamports. Fund/deposit generously so any bid in these tests can
    // encumber its collateral.
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 6_000_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(
            &borrower,
            borrower_wsol,
            /*is_debt=*/ false,
            5_000_000_000,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    borrower
}

/// A borrower bid smaller than the vault ask leaves the ask resting and
/// only encumbers the matched principal. The resting risk-profile ask
/// is never removed by the matching engine.
#[tokio::test]
async fn match_partial_fill_leaves_vault_ask_resting() {
    let fixture = MarketFixture::new().await;
    // Single profile, ask at 500bps / 30d.
    let borrower = vault_with_asks(&fixture, &[(500, 30 * 86_400, 100_000_000)]).await;

    // Bid for 1M atoms (vault ask is open-ended; only the bid size and
    // the idle pool cap the cross).
    let principal_atoms: u64 = 1_000_000;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            /*rate_bps=*/ 800,
            30 * 86_400,
            principal_atoms,
            /*collateral_atoms=*/ 50_000_000,
            /*flags=*/ 0,
        )
        .await
        .unwrap();

    // Exactly one fill; the vault ask is still on book.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1, "one MatchedLoan inserted");
    assert_ne!(
        market.asks_best_index,
        hypertree::NIL,
        "risk-profile ask must persist (only the curator removes it)"
    );

    // Vault encumbrance bumped by exactly the matched principal.
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(profile.encumbered_in_orders_atoms, principal_atoms);
    // Borrower seat's collateral STAYS encumbered — it backs the open
    // loan and is released only at close. The single Fixed cross also
    // ticks `open_borrow_count` to 1.
    let borrower_seat = fixture.read_seat(&borrower.pubkey()).await;
    assert!(
        borrower_seat.collateral_encumbered_shares > 0,
        "fully-filled bid keeps the matched collateral encumbered (backs the open loan)"
    );
    assert_eq!(
        borrower_seat.open_borrow_count, 1,
        "one Fixed cross → one open loan"
    );
    // Sum of MatchedLoan collateral equals the bid's posted collateral
    // (dust-sweep invariant from the audit).
    assert_eq!(
        fixture.sum_matched_loan_collateral().await,
        50_000_000,
        "Σ MatchedLoan.collateral_atoms == bid collateral"
    );
    // Vault-idle invariant on the crossed profile.
    fixture.assert_vault_idle_invariant(0).await;
}

/// Three profiles post asks at 500 / 600 / 700 bps. A small borrower
/// bid must cross the *best* (lowest-rate) ask first — the matching
/// engine walks the asks tree from `asks_best_index`, which under the
/// `RestingOrder` Ord direction is the cheapest ask.
#[tokio::test]
async fn match_picks_best_ask_first() {
    let fixture = MarketFixture::new().await;
    let borrower = vault_with_asks(
        &fixture,
        &[
            (700, 30 * 86_400, 100_000_000), // profile 0 — worst rate
            (500, 30 * 86_400, 100_000_000), // profile 1 — best rate
            (600, 30 * 86_400, 100_000_000), // profile 2
        ],
    )
    .await;

    // Bid 1M atoms at 800bps — crosses exactly one ask. The engine
    // should hit profile 1 (500bps), the cheapest.
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

    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1, "one fill");

    // Only profile 1 (the 500bps ask) was encumbered.
    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    let p2 = fixture.read_risk_profile(2).await;
    assert_eq!(
        p1.encumbered_in_orders_atoms, 1_000_000,
        "best-priced (500bps) ask must be crossed first"
    );
    assert_eq!(p0.encumbered_in_orders_atoms, 0, "700bps ask untouched");
    assert_eq!(p2.encumbered_in_orders_atoms, 0, "600bps ask untouched");
    // Matched-collateral conservation: the single MatchedLoan must
    // carry exactly the bid's posted collateral.
    assert_eq!(
        fixture.sum_matched_loan_collateral().await,
        50_000_000,
        "Σ MatchedLoan.collateral_atoms == bid collateral"
    );
    // Vault-idle invariant must hold on every profile.
    fixture.assert_vault_idle_invariant(0).await;
    fixture.assert_vault_idle_invariant(1).await;
    fixture.assert_vault_idle_invariant(2).await;
}

/// The best ask has a term too short for the bid; a worse-rate ask
/// accepts a long enough term. The matching engine must walk past the
/// term-incompatible best maker rather than bailing out.
#[tokio::test]
async fn match_skips_term_incompatible_best_and_walks_on() {
    let fixture = MarketFixture::new().await;
    let borrower = vault_with_asks(
        &fixture,
        &[
            (500, 7 * 86_400, 100_000_000), // profile 0 — best rate, term too short
            (700, 60 * 86_400, 100_000_000), // profile 1 — worse rate, long term
        ],
    )
    .await;

    // Bid term = 30d. Profile 0's 7d ask is term-incompatible
    // (bid_term 30d > ask_term 7d) → skipped. Profile 1's 60d ask
    // accepts it.
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

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "exactly one fill (profile 1)"
    );

    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    assert_eq!(
        p0.encumbered_in_orders_atoms, 0,
        "term-incompatible best ask must be skipped"
    );
    assert_eq!(
        p1.encumbered_in_orders_atoms, 1_000_000,
        "compatible worse-rate ask must be crossed"
    );
    // Vault-idle invariant on both profiles.
    fixture.assert_vault_idle_invariant(0).await;
    fixture.assert_vault_idle_invariant(1).await;
}

/// A bid larger than the best ask's idle-pool-capped depth sweeps
/// across multiple vault asks. Each cross records its own MatchedLoan.
#[tokio::test]
async fn match_sweeps_multiple_vault_asks() {
    let fixture = MarketFixture::new().await;
    // Two profiles, both at the same rate/term — the bid crosses both.
    let borrower = vault_with_asks(
        &fixture,
        &[
            (500, 30 * 86_400, 50_000_000),
            (500, 30 * 86_400, 50_000_000),
        ],
    )
    .await;

    // Each profile has only 50M idle. A 70M bid fully consumes
    // profile 0's idle (50M) and partially fills profile 1 (20M);
    // the unbounded asks let the bid sweep across both profiles.
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            70_000_000,
            // 70_000_000 USDC atoms ($70) of debt; post ~3 SOL of wSOL
            // collateral — comfortably over the requirement.
            3_000_000_000,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await
        .unwrap();

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "the bid swept both vault asks — one MatchedLoan each"
    );

    // Total encumbrance across the two profiles equals the bid size
    // minus the matching engine's per-profile marginfi-rounding
    // reserve (`MARGINFI_ROUNDING_RESERVE_ATOMS = 1`). Profile 0 was
    // idle-capped at its 50M − 1 reserve = 49_999_999; profile 1
    // absorbs the remainder.
    use ydelta::state::market_helpers::MARGINFI_ROUNDING_RESERVE_ATOMS;
    let p0 = fixture.read_risk_profile(0).await;
    let p1 = fixture.read_risk_profile(1).await;
    let p0_expected: u64 = 49_999_999 - MARGINFI_ROUNDING_RESERVE_ATOMS;
    let p1_expected: u64 = 70_000_000 - p0_expected;
    assert_eq!(
        p0.encumbered_in_orders_atoms + p1.encumbered_in_orders_atoms,
        70_000_000,
        "swept fills sum to the bid principal"
    );
    assert_eq!(
        p0.encumbered_in_orders_atoms, p0_expected,
        "profile 0 idle-capped at 50M minus the marginfi-rounding reserve"
    );
    assert_eq!(
        p1.encumbered_in_orders_atoms, p1_expected,
        "profile 1 absorbs the residual after profile 0's reserved cap"
    );
    // Vault-idle invariant must hold on both profiles after the
    // double-cross.
    fixture.assert_vault_idle_invariant(0).await;
    fixture.assert_vault_idle_invariant(1).await;
}

/// A fully-filled multi-cross bid must leave ZERO collateral frozen in
/// the borrower's encumbered bucket. The per-cross collateral split
/// FLOORS (`mul_div_u64`); without a dust sweep on the final cross,
/// `Σ matched_collateral < bid collateral` and the floored-off atoms
/// stay permanently stuck in the borrower seat's
/// `collateral_encumbered_shares` (un-withdrawable, unattributed to any
/// loan). The matcher sweeps ALL remaining collateral into the last cross.
///
/// Setup: two profiles split a 70M-atom bid 50M / 20M. With 3 SOL of
/// collateral the pro-rata split is `3e9 × 50/70` and `3e9 × 20/70` —
/// neither divides evenly, so a plain floored sum is 2_999_999_999
/// (1 atom of dust). The dust sweep makes the sum exactly 3_000_000_000
/// and the borrower's encumbered collateral drain to 0.
#[tokio::test]
async fn match_multi_cross_full_fill_freezes_no_collateral_dust() {
    let fixture = MarketFixture::new().await;
    // Two profiles, same rate/term — the bid crosses both. 50M idle on
    // profile 0, plenty on profile 1, so a 70M bid fully fills.
    let borrower = vault_with_asks(
        &fixture,
        &[
            (500, 30 * 86_400, 50_000_000),
            (500, 30 * 86_400, 50_000_000),
        ],
    )
    .await;

    let bid_principal: u64 = 70_000_000;
    // 3 SOL of wSOL collateral. The 50/70 and 20/70 pro-rata splits both
    // floor — the dust-sweep fix must still account every atom.
    let bid_collateral: u64 = 3_000_000_000;

    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            /*rate_bps=*/ 800,
            30 * 86_400,
            bid_principal,
            bid_collateral,
            // OB_ONLY so the bid fully fills against the book (no P2Pool
            // residual) — this is the `remaining_principal == 0` branch
            // the dust-sweep fix targets.
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await
        .unwrap();

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "the bid swept both vault asks — two MatchedLoans"
    );

    // (1) Every collateral atom is attributed to a loan — the sum of
    //     per-loan collateral equals the bid's posted collateral. The
    //     buggy floored split would sum to 2_999_999_999.
    let total_loan_collateral = fixture.sum_matched_loan_collateral().await;
    assert_eq!(
        total_loan_collateral, bid_collateral,
        "Σ MatchedLoan.collateral_atoms must equal the bid collateral — \
         no floored-off dust"
    );

    // (2) The borrower's collateral STAYS encumbered — it backs the two
    //     open loans and is released only at close. Because the collateral
    //     is encumbered once up front (the full bid amount) and matching
    //     does not decrement it per cross, the encumbered bucket holds
    //     exactly the bid collateral with NO share dust. Each of the two
    //     crosses bumped `open_borrow_count`, so it reads 2.
    let borrower_seat = fixture.read_seat(&borrower.pubkey()).await;
    assert!(
        borrower_seat.collateral_encumbered_shares > 0,
        "fully-filled multi-cross bid keeps its collateral encumbered (backs the open loans)"
    );
    assert_eq!(
        borrower_seat.open_borrow_count, 2,
        "two Fixed crosses created two open loans → open_borrow_count == 2"
    );
}
