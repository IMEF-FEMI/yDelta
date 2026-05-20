//! `place_order` is borrower-IOC-only: the taker is always a
//! `Side::Bid` borrower and the order type is always
//! `ImmediateOrCancel`. The bid crosses resting vault risk-profile asks
//! and any residual either fires the P2Pool fallback or drops — it
//! never rests on the book.

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

/// A borrower IOC bid against an empty book leaves no resting order:
/// with no crossable vault ask the residual drops (`OB_ONLY` is set by
/// the native `place_order` helper) and the seat's encumbrance is
/// fully refunded — the bid never rests.
#[tokio::test]
async fn borrower_bid_against_empty_book_drops_and_unencumbers() {
    let mut fixture = TestFixture::new().await;
    let (bob, _, _) = fund_trader(&mut fixture, 0, 5_000).await;

    let pre = fixture.read_seat(&bob.pubkey()).await;

    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::ImmediateOrCancel,
            800,
            60 * 86_400,
            400,
            500,
        )
        .await
        .unwrap();

    // No resting orders — the IOC bid never rests on either tree.
    assert_eq!(fixture.count_orders(Side::Bid).await, 0);
    assert_eq!(fixture.count_orders(Side::Ask).await, 0);

    // The residual dropped: the borrower seat's collateral is fully
    // refunded and the open-borrow counter is back to its pre value.
    let post = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        post.collateral_withdrawable_shares,
        pre.collateral_withdrawable_shares
    );
    assert_eq!(
        post.collateral_encumbered_shares,
        pre.collateral_encumbered_shares
    );
    assert_eq!(post.open_borrow_count, pre.open_borrow_count);
}

/// `place_order` consumes one order sequence number per call even
/// though the bid never rests — the sequence is stamped on the
/// `OrderPlaced` log for off-chain indexers.
#[tokio::test]
async fn borrower_bid_consumes_one_sequence() {
    let mut fixture = TestFixture::new().await;
    let (bob, _, _) = fund_trader(&mut fixture, 0, 5_000).await;

    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::ImmediateOrCancel,
            800,
            60 * 86_400,
            400,
            500,
        )
        .await
        .unwrap();

    let market = fixture.read_market().await;
    assert_eq!(market.fixed.order_sequence_number, 1);
    assert_eq!(fixture.count_orders(Side::Bid).await, 0);
    assert_eq!(fixture.count_orders(Side::Ask).await, 0);
}
