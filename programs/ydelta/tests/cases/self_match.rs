//! Self-match prevention.

use solana_sdk::signer::Signer;

use ydelta::program::YdeltaError;
use ydelta::state::{OrderType, Side};

use crate::assert_custom_error;
use crate::test_utils::TestFixture;

#[tokio::test]
async fn self_match_rejected() {
    // Trader places an ask, then a bid that crosses it. The bid's
    // taker-side match should reject with `SelfMatchForbidden` rather
    // than wash the trader's encumbered balances.
    let mut fixture = TestFixture::new().await;
    let alice = fixture.create_trader(0, 0).await;
    fixture.claim_seat(&alice).await.unwrap();

    fixture
        .seed_seat_shares(&alice.pubkey(), 1_000, /*is_debt=*/ true)
        .await;
    fixture
        .seed_seat_shares(&alice.pubkey(), 5_000, /*is_debt=*/ false)
        .await;

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

    let result = fixture
        .place_order(
            &alice,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            700,
            1_000,
        )
        .await;
    assert_custom_error!(result, YdeltaError::SelfMatchForbidden);
}
