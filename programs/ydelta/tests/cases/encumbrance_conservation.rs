//! Encumbrance / cancel / match all operate on fp48 shares snapshot
//! at place-order time. The snapshot is the placeholder 1.0, so the
//! math is identity (atom-as-share). These tests exercise the
//! round-trip symmetry that lets a future bank-read swap not touch
//! the framework.

use solana_sdk::signer::Signer;

use ydelta::state::{OrderType, Side};

use crate::test_utils::TestFixture;

#[tokio::test]
async fn encumber_then_cancel_round_trips_exactly() {
    let mut fixture = TestFixture::new().await;
    let alice = fixture.create_trader(0, 0).await;
    fixture.claim_seat(&alice).await.unwrap();

    let amt: u128 = 1_000;
    fixture
        .seed_seat_shares(&alice.pubkey(), amt, /*is_debt=*/ true)
        .await;

    let pre = fixture.read_seat(&alice.pubkey()).await;

    // Place an ask: encumbers 700 atoms of debt (which under the 1.0
    // placeholder snapshot become 700 fp48 shares — identity).
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

    let mid = fixture.read_seat(&alice.pubkey()).await;
    assert_eq!(
        mid.debt_withdrawable_shares,
        pre.debt_withdrawable_shares - 700
    );
    assert_eq!(mid.debt_encumbered_shares, pre.debt_encumbered_shares + 700);

    // Cancel — the order's recorded snapshot drives the decrement.
    let market = fixture.read_market().await;
    let seq = market.fixed.order_sequence_number - 1;
    fixture.cancel_order(&alice, seq).await.unwrap();

    let post = fixture.read_seat(&alice.pubkey()).await;
    assert_eq!(
        post.debt_withdrawable_shares, pre.debt_withdrawable_shares,
        "withdrawable shares must be byte-exact pre vs post (encumber→cancel round-trip)"
    );
    assert_eq!(
        post.debt_encumbered_shares, pre.debt_encumbered_shares,
        "encumbered shares must be byte-exact"
    );
    assert_eq!(post.open_lend_count, pre.open_lend_count);
}

#[tokio::test]
async fn match_decrement_is_byte_symmetric_with_encumber() {
    // Two traders place crossing orders; on full match, both seats'
    // encumbered shares should drop to exactly the value they held
    // pre-encumber (since match-time decrement uses the same snapshot
    // recorded at place-order time).
    let mut fixture = TestFixture::new().await;
    let alice = fixture.create_trader(0, 0).await;
    let bob = fixture.create_trader(0, 0).await;
    fixture.claim_seat(&alice).await.unwrap();
    fixture.claim_seat(&bob).await.unwrap();

    fixture
        .seed_seat_shares(&alice.pubkey(), 1_000, /*is_debt=*/ true)
        .await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 5_000, /*is_debt=*/ false)
        .await;

    let alice_pre = fixture.read_seat(&alice.pubkey()).await;
    let bob_pre = fixture.read_seat(&bob.pubkey()).await;

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
    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            700,
            1_000,
        )
        .await
        .unwrap();

    let alice_post = fixture.read_seat(&alice.pubkey()).await;
    let bob_post = fixture.read_seat(&bob.pubkey()).await;

    // Encumbered shares back to original on both sides — the match
    // fully drained the encumbrance both placed.
    assert_eq!(
        alice_post.debt_encumbered_shares,
        alice_pre.debt_encumbered_shares
    );
    assert_eq!(
        bob_post.collateral_encumbered_shares,
        bob_pre.collateral_encumbered_shares
    );
    // Withdrawable on alice's debt side: she encumbered 700 then the
    // match consumed it without re-credit at this stage (the credit
    // arrives via process_matched_loan + claim_repayment at SBF level
    // which this native test doesn't exercise). So withdrawable
    // dropped by 700 net.
    assert_eq!(
        alice_post.debt_withdrawable_shares,
        alice_pre.debt_withdrawable_shares - 700
    );
    // Bob's collateral_withdrawable: dropped by 1000.
    assert_eq!(
        bob_post.collateral_withdrawable_shares,
        bob_pre.collateral_withdrawable_shares - 1000
    );
}
