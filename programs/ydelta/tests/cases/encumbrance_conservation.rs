//! Encumbrance conservation across a borrower IOC fill.
//!
//! Only the borrower (taker) seat carries a per-seat encumbrance —
//! vault risk-profile asks are open-ended and tracked via
//! `RiskProfile.encumbered_in_orders_atoms`, not per-seat shares.
//! There is no resting-bid encumbrance.
//!
//! These tests pin two invariants on an IOC cross:
//!   1. Borrower seat: the collateral encumbered by `place_order` for
//!      the *matched* slice is released atom-for-atom at match time
//!      (the snapshot used to encumber is the same one used to
//!      decrement), so a fully-matched bid leaves the borrower's
//!      `collateral_encumbered_shares` back at zero.
//!   2. Vault side: the crossed profile's
//!      `encumbered_in_orders_atoms` is bumped by exactly the matched
//!      principal, and `total_principal_atoms` is untouched until the
//!      cranker settles.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// A borrower bid fully matched by a vault ask: the borrower seat's
/// collateral encumbrance returns to zero (match-time decrement is
/// byte-symmetric with the place-time encumber) and the vault profile
/// records the matched principal as encumbered.
#[tokio::test]
async fn borrower_encumbrance_released_on_full_match() {
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
        .create_risk_profile(&admin, 0, curator.pubkey(), 8_000, 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // No claim-seat step — the vault market-seat is auto-created on the
    // curator's first place_order_for_risk_profile. The ask is
    // unbounded; each cross is capped by the profile's idle balance.
    fixture
        .place_order_for_risk_profile(&curator, 0, /*rate_bps=*/ 500, 30 * 86_400, 0)
        .await
        .unwrap();

    // Borrower deposits collateral.
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 200_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(
            &borrower,
            borrower_wsol,
            /*is_debt=*/ false,
            100_000_000,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Pre-cross borrower seat: nothing encumbered yet.
    let pre = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(pre.collateral_encumbered_shares, 0);

    // Bid fully crossed by the vault ask.
    let principal_atoms: u64 = 1_000_000;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            /*collateral_atoms=*/ 50_000_000,
            0,
        )
        .await
        .unwrap();

    // Borrower seat: the IOC bid never rests, so the place-time
    // collateral encumbrance is fully released by the match-time
    // decrement — back to exactly the pre-cross value.
    let post = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(
        post.collateral_encumbered_shares, pre.collateral_encumbered_shares,
        "fully-matched IOC bid must leave zero collateral encumbrance on the borrower seat"
    );

    // Vault side: the crossed profile is encumbered by exactly the
    // matched principal; total principal is untouched (atoms migrate
    // only when the cranker settles).
    let profile = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile.encumbered_in_orders_atoms, principal_atoms,
        "vault profile encumbered for exactly the matched principal"
    );
    assert_eq!(
        profile.total_principal_atoms, 100_000_000,
        "total_principal_atoms unchanged until the cranker settles"
    );
    assert_eq!(
        profile.deployed_principal_atoms, 0,
        "deployed_principal_atoms only bumps when the cranker settles"
    );
}

/// A borrower bid whose residual drops (OB_ONLY, empty book) must
/// release the *entire* place-time collateral encumbrance — nothing
/// rests, nothing stays encumbered.
#[tokio::test]
async fn dropped_residual_releases_all_borrower_encumbrance() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // Borrower seat with seeded collateral shares — no vault ask
    // rests, so the whole bid is a residual.
    fixture.claim_seat(&borrower).await;
    fixture
        .seed_seat_shares(&borrower.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    let pre = fixture.read_seat(&borrower.pubkey()).await;

    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            /*principal_atoms=*/ 1_000_000,
            /*collateral_atoms=*/ 100_000_000,
            ydelta::state::market_helpers::FLAG_OB_ONLY,
        )
        .await
        .unwrap();

    let post = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(
        post.collateral_encumbered_shares, pre.collateral_encumbered_shares,
        "OB_ONLY dropped residual must release all collateral encumbrance"
    );
    assert_eq!(
        post.collateral_withdrawable_shares, pre.collateral_withdrawable_shares,
        "withdrawable collateral restored — the bid neither rested nor matched"
    );

    // Nothing rested; no MatchedLoan.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.asks_root_index, hypertree::NIL);
    assert_eq!(market.matched_loan_sequence, 0);
}
