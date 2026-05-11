use solana_sdk::signer::Signer;

use ydelta::state::{OrderType, Side};

use crate::test_utils::TestFixture;

async fn fund_trader(
    fixture: &mut TestFixture,
    debt_amt: u64,
    coll_amt: u64,
) -> (
    solana_sdk::signature::Keypair,
    solana_program::pubkey::Pubkey,
    solana_program::pubkey::Pubkey,
) {
    let trader = fixture.create_trader(0, 0).await;
    fixture.claim_seat(&trader).await.unwrap();

    let debt_ata = fixture
        .create_token_account_and_mint(&trader.pubkey(), &fixture.debt_mint_key(), debt_amt)
        .await;
    let coll_ata = fixture
        .create_token_account_and_mint(&trader.pubkey(), &fixture.collateral_mint_key(), coll_amt)
        .await;
    fixture.refresh_blockhash().await;
    if debt_amt > 0 {
        // Deposit-via-marginfi requires real bank state. Bypass the
        // deposit ix and seed the seat directly — matching/ordering
        // tests don't exercise the SPL or marginfi side.
        fixture
            .seed_seat_shares(&trader.pubkey(), debt_amt as u128, true)
            .await;
    }
    if coll_amt > 0 {
        fixture
            .seed_seat_shares(&trader.pubkey(), coll_amt as u128, false)
            .await;
    }
    (trader, debt_ata, coll_ata)
}

#[tokio::test]
async fn place_ask_encumbers_debt() {
    let mut fixture = TestFixture::new().await;
    let (alice, _, _) = fund_trader(&mut fixture, 1_000, 0).await;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            700,
            0,
        )
        .await
        .unwrap();

    let seat = fixture.read_seat(&alice.pubkey()).await;
    assert_eq!(seat.debt_withdrawable_shares, 300);
    assert_eq!(seat.debt_encumbered_shares, 700);
    assert_eq!(seat.open_lend_count, 1);
    assert_eq!(seat.open_borrow_count, 0);
    assert_eq!(fixture.count_orders(Side::Ask).await, 1);
    assert_eq!(fixture.count_orders(Side::Bid).await, 0);
}

#[tokio::test]
async fn place_bid_encumbers_collateral() {
    let mut fixture = TestFixture::new().await;
    let (bob, _, _) = fund_trader(&mut fixture, 0, 5_000).await;

    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            60 * 86_400,
            400,
            500,
        )
        .await
        .unwrap();

    let seat = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(seat.collateral_withdrawable_shares, 4_500);
    assert_eq!(seat.collateral_encumbered_shares, 500);
    assert_eq!(seat.open_borrow_count, 1);
    assert_eq!(fixture.count_orders(Side::Bid).await, 1);
}

#[tokio::test]
async fn cancel_unencumbers() {
    let mut fixture = TestFixture::new().await;
    let (alice, _, _) = fund_trader(&mut fixture, 1_000, 0).await;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            700,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.cancel_order(&alice, 0).await.unwrap();

    let seat = fixture.read_seat(&alice.pubkey()).await;
    assert_eq!(seat.debt_withdrawable_shares, 1_000);
    assert_eq!(seat.debt_encumbered_shares, 0);
    assert_eq!(seat.open_lend_count, 0);
    assert_eq!(fixture.count_orders(Side::Ask).await, 0);
}

#[tokio::test]
async fn update_order_rate_change_is_rejected() {
    // Rate changes via `update_order` are rejected on both sides:
    // an Ask re-rated downward could cross a resting Bid that has
    // become undercollateralized since placement; the LTV must
    // re-check against current oracles, which requires a fresh
    // `place_order` call. `update_order` is bounded to non-rate
    // mutations (last_valid_unix_ts / flags).
    let mut fixture = TestFixture::new().await;
    let (alice, _, _) = fund_trader(&mut fixture, 1_000, 0).await;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            700,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let result = fixture.update_order_rate(&alice, 0, 750).await;
    assert!(
        result.is_err(),
        "rate change via update_order must be rejected; use cancel + place_order"
    );

    // Original order still rests at the original rate.
    let market = fixture.read_market().await;
    assert_eq!(market.fixed.order_sequence_number, 1);
    assert_eq!(fixture.count_orders(Side::Ask).await, 1);
}
