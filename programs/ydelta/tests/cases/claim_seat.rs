use solana_sdk::signer::Signer;

use ydelta::program::YdeltaError;

use crate::assert_custom_error;
use crate::test_utils::TestFixture;

#[tokio::test]
async fn claim_seat_inserts_zeroed_seat() {
    let mut fixture = TestFixture::new().await;
    let trader = fixture.create_trader(0, 0).await;

    fixture.claim_seat(&trader).await.unwrap();
    let seat = fixture.read_seat(&trader.pubkey()).await;

    assert_eq!(seat.owner, trader.pubkey());
    assert_eq!(seat.debt_withdrawable_shares, 0);
    assert_eq!(seat.collateral_withdrawable_shares, 0);
    assert_eq!(seat.debt_encumbered_shares, 0);
    assert_eq!(seat.collateral_encumbered_shares, 0);
    assert_eq!(seat.open_borrow_count, 0);
    assert_eq!(seat.open_lend_count, 0);
}

#[tokio::test]
async fn claim_seat_rejects_duplicate() {
    let mut fixture = TestFixture::new().await;
    let trader = fixture.create_trader(0, 0).await;

    fixture.claim_seat(&trader).await.unwrap();
    fixture.refresh_blockhash().await;
    let result = fixture.claim_seat(&trader).await;
    assert_custom_error!(result, YdeltaError::AlreadyClaimedSeat);
}

#[tokio::test]
async fn claim_seat_bumps_position_count() {
    let mut fixture = TestFixture::new().await;
    let alice = fixture.create_trader(0, 0).await;
    let bob = fixture.create_trader(0, 0).await;

    fixture.claim_seat(&alice).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture.claim_seat(&bob).await.unwrap();

    let market = fixture.read_market().await;
    assert_eq!(market.fixed.position_count, 2);
}
