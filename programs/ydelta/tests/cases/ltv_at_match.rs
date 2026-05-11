//! Per-match LTV check fires at the matching engine using real
//! oracle prices + bank weights from the loaded mainnet fixtures.
//!
//! Setup mirrors `loan_lifecycle::lifecycle_smoke`: alice asks USDC
//! debt, bob bids matching principal with X collateral. Tests vary X
//! to span the LTV-required boundary.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::program::YdeltaError;
use ydelta::state::{OrderType, Side};

use crate::assert_custom_error;
use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn match_passes_with_overcollateralized_bid() {
    // SOL @ ~$100, USDC @ $1. liab_weight_init for USDC ≈ 1.25,
    // asset_weight_init for SOL ≈ 0.65 (mainnet).
    // For 1_000_000 atoms USDC debt ($1) we'd need
    //   required ≈ 1_000_000 × 1 × 1.25 / (100 × 0.65)
    //           ≈ 19_230 wSOL atoms (lamports).
    // Posting 100_000_000 lamports (0.1 SOL ≈ $10) is ~5_200×
    // over-collateralised at any reasonable SOL price.
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    let alice_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        alice_usdc,
        mainnet::usdc_mint(),
        alice.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&alice, alice_usdc, /*is_debt=*/ true, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000; // very-over-collateralised
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            principal_atoms,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let result = fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
        )
        .await;
    assert!(
        result.is_ok(),
        "well-collateralised match should pass LTV: {:?}",
        result
    );

    // Match landed.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1);
}

#[tokio::test]
async fn match_rejected_with_undercollateralized_bid() {
    // Post WAY too little collateral. With realistic mainnet weights
    // and SOL ~ $100, 100 lamports of SOL collateral can't back
    // 1_000_000 USDC atoms.
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    let alice_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        alice_usdc,
        mainnet::usdc_mint(),
        alice.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&alice, alice_usdc, true, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, false)
        .await;

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100; // ≈ $0.0001 of SOL backing $1 of debt — laughable.
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            principal_atoms,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let result = fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
        )
        .await;
    // Pre-flight in `place_order` rejects the bid before the matching
    // loop runs — `actual_ltv` (computed at oracle prices) exceeds the
    // effective borrower-LTV cap (defaulted to marginfi-init when the
    // caller passes `None`). Both pre-flight and per-cross checks are
    // mathematically equivalent for an all-bid undercollateralization;
    // pre-flight just fires earlier with a more specific error.
    assert_custom_error!(result, YdeltaError::BorrowerLtvExceeded);

    // No match landed.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 0);
}
