//! Borrower-set LTV + vault `max_ltv_bps` enforcement tests.
//!
//! Native: layout + Borsh round-trip of the new `borrower_ltv_bps`
//! field. SBPF: pre-flight rejection + per-maker risk-tier walk +
//! default-passes-through behavior.

use borsh::{BorshDeserialize, BorshSerialize};
#[cfg(feature = "test-sbf")]
use solana_sdk::signer::Signer;

use ydelta::program::processor::place_order::PlaceOrderParams;
use ydelta::state::resting_order::RestingOrder;
use ydelta::state::{OrderType, Side};

#[test]
fn place_order_params_round_trips_borrower_ltv() {
    let original = PlaceOrderParams {
        seat_index_hint: None,
        side: Side::Bid as u8,
        order_type: OrderType::Limit as u8,
        flags: 0,
        kind: 0,
        rate_bps: 800,
        term_seconds: 30 * 86_400,
        principal_atoms: 1_000_000,
        collateral_atoms: 50_000_000,
        asking_price_atoms: 0,
        last_valid_unix_ts: 0,
        borrower_ltv_bps: Some(6_000),
    };
    let mut buf = Vec::new();
    original.serialize(&mut buf).unwrap();
    let decoded = PlaceOrderParams::try_from_slice(&buf).unwrap();
    assert_eq!(decoded.borrower_ltv_bps, Some(6_000));
    assert_eq!(decoded.principal_atoms, 1_000_000);

    // None round-trips identically — existing callers see no change.
    let none_params = PlaceOrderParams {
        borrower_ltv_bps: None,
        ..original
    };
    let mut buf2 = Vec::new();
    none_params.serialize(&mut buf2).unwrap();
    let decoded2 = PlaceOrderParams::try_from_slice(&buf2).unwrap();
    assert_eq!(decoded2.borrower_ltv_bps, None);
}

#[test]
fn resting_order_stamps_borrower_ltv_on_bid_only() {
    let bid = RestingOrder::new_primary(
        /*trader_seat_index=*/ 0,
        /*sequence=*/ 1,
        Side::Bid,
        OrderType::Limit,
        /*rate_bps=*/ 800,
        /*term_seconds=*/ 30 * 86_400,
        /*principal=*/ 1_000_000,
        /*collateral=*/ 50_000_000,
        /*last_valid=*/ 0,
        /*flags=*/ 0,
        /*share_price_snapshot=*/ 1u128 << 48,
        /*borrower_ltv_bps=*/ 6_500,
    );
    assert_eq!(bid.borrower_ltv_bps, 6_500);

    // Asks ignore the field — constructor zeroes it.
    let ask = RestingOrder::new_primary(
        0,
        1,
        Side::Ask,
        OrderType::Limit,
        500,
        30 * 86_400,
        1_000_000,
        0,
        0,
        0,
        1u128 << 48,
        9_000,
    );
    assert_eq!(ask.borrower_ltv_bps, 0, "ask should ignore borrower_ltv");
}

#[cfg(feature = "test-sbf")]
mod sbf {
    use super::*;
    use solana_program::pubkey::Pubkey;
    use ydelta::program::YdeltaError;

    use crate::assert_custom_error;
    use crate::test_utils::marginfi_fixture::mainnet;
    use crate::test_utils::market_fixture::MarketFixture;

    /// Borrower passes a `borrower_ltv_bps` far tighter than the
    /// actual loan LTV. Pre-flight rejects with `BorrowerLtvExceeded`.
    #[tokio::test]
    async fn pre_flight_rejects_borrower_ltv_below_actual() {
        let fixture = MarketFixture::new().await;
        let alice = fixture.create_trader().await; // lender
        let bob = fixture.create_trader().await; // borrower
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

        fixture
            .place_order(
                &alice,
                Side::Ask,
                OrderType::Limit,
                600,
                30 * 86_400,
                1_000_000,
                0,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;

        // Bob's bid declares `borrower_ltv_bps = Some(0)` — any
        // positive collateralization yields actual_ltv > 0, so the
        // pre-flight rejects.
        let result = fixture
            .place_order_with_borrower_ltv(
                &bob,
                Side::Bid,
                OrderType::Limit,
                800,
                30 * 86_400,
                1_000_000,
                100_000_000,
                Some(0),
            )
            .await;
        assert_custom_error!(result, YdeltaError::BorrowerLtvExceeded);
    }

    /// Default-LTV bid (no `borrower_ltv_bps` passed) matches
    /// uniformly — preserves pre-feature behavior.
    #[tokio::test]
    async fn default_borrower_ltv_matches_normally() {
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

        fixture
            .place_order(
                &alice,
                Side::Ask,
                OrderType::Limit,
                600,
                30 * 86_400,
                1_000_000,
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
                1_000_000,
                100_000_000,
            )
            .await;
        assert!(
            result.is_ok(),
            "default-LTV bid should match a wallet ask: {:?}",
            result
        );

        let market = fixture.read_market_fixed().await;
        assert_eq!(
            market.matched_loan_sequence, 1,
            "match should have been recorded"
        );
    }

    /// Wallet ask makers have no `risk_profile_max_ltv_bps` cached on their
    /// seat, so the per-maker gate is a no-op against them. Borrower
    /// can declare any cap and still match a wallet lender.
    #[tokio::test]
    async fn wallet_ask_unaffected_by_explicit_borrower_ltv() {
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

        fixture
            .place_order(
                &alice,
                Side::Ask,
                OrderType::Limit,
                600,
                30 * 86_400,
                1_000_000,
                0,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;

        let result = fixture
            .place_order_with_borrower_ltv(
                &bob,
                Side::Bid,
                OrderType::Limit,
                800,
                30 * 86_400,
                1_000_000,
                100_000_000,
                Some(6_000),
            )
            .await;
        assert!(
            result.is_ok(),
            "explicit borrower_ltv against wallet ask should match: {:?}",
            result
        );
    }
}
