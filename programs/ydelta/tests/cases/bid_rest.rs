//! v1 D6 bid-side coverage (phase 4a): a borrower's unfilled residual
//! can REST in the bids tree instead of dropping or falling through to
//! P2Pool. The rested bid keeps its collateral encumbered (still earning
//! marginfi supply yield), is tracked on the borrower's `UserAccount`,
//! and `CancelOrder` releases exactly the stored-snapshot encumbrance.

use hypertree::NIL;
use solana_sdk::signer::Signer;

use ydelta::program::instruction_builders::cancel_order_instruction::cancel_order_instruction;
use ydelta::state::market_helpers::RESIDUAL_MODE_REST;

use crate::test_utils::MarketFixture;

const TERM: u32 = 30 * 86_400;

#[tokio::test]
async fn residual_rests_and_cancel_releases_collateral() {
    let fixture = MarketFixture::new().await;
    let borrower = fixture.create_trader().await;

    // Empty book: the whole bid is residual → rests (mode = Rest).
    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, /*is_debt=*/ false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let seat_pre = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(seat_pre.collateral_encumbered_shares, 0);
    let withdrawable_pre = seat_pre.collateral_withdrawable_shares;

    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            2_000,
            TERM,
            1_000_000,
            50_000_000,
            RESIDUAL_MODE_REST,
        )
        .await
        .expect("empty book + Rest mode must rest the residual, not fail");

    // The bid rests in the BIDS tree…
    let market = fixture.read_market_fixed().await;
    assert_ne!(market.bids_root_index, NIL, "bids tree holds the residual");
    assert_eq!(
        market.matched_loan_sequence, 0,
        "nothing crossed against an empty ask book"
    );

    // …its collateral stays encumbered (yield-alive while waiting)…
    let seat_rested = fixture.read_seat(&borrower.pubkey()).await;
    assert!(
        seat_rested.collateral_encumbered_shares > 0,
        "rested bid keeps its collateral encumbered"
    );
    assert!(seat_rested.collateral_withdrawable_shares < withdrawable_pre);

    // …and the borrower's UserAccount tracks it.
    let ua = fixture
        .account_data(ydelta::state::user_account::user_account_pda(&borrower.pubkey()).0)
        .await;
    let ua_fixed: &ydelta::state::user_account::UserAccountFixed =
        bytemuck::from_bytes(&ua[..ydelta::state::USER_ACCOUNT_FIXED_SIZE]);
    assert_eq!(ua_fixed.user_order_count, 1, "UserOrderRef tracked (v1 D6)");

    // Cancel: collateral fully restored, ref dropped, tree empty.
    fixture.refresh_blockhash().await;
    let cancel_ix = cancel_order_instruction(
        &fixture.market.pubkey(),
        &borrower.pubkey(),
        /*order_sequence=*/ 0,
        None,
    );
    fixture
        .process_ixs(&[cancel_ix], &[&borrower.insecure_clone()])
        .await
        .expect("owner cancel must succeed");

    let seat_post = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(
        seat_post.collateral_encumbered_shares, 0,
        "cancel releases the rested bid's encumbrance"
    );
    assert_eq!(
        seat_post.collateral_withdrawable_shares, withdrawable_pre,
        "withdrawable restored to the pre-bid balance (same snapshot)"
    );
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.bids_root_index, NIL, "bids tree empty after cancel");
    let ua = fixture
        .account_data(ydelta::state::user_account::user_account_pda(&borrower.pubkey()).0)
        .await;
    let ua_fixed: &ydelta::state::user_account::UserAccountFixed =
        bytemuck::from_bytes(&ua[..ydelta::state::USER_ACCOUNT_FIXED_SIZE]);
    assert_eq!(ua_fixed.user_order_count, 0, "UserOrderRef dropped");
}

#[tokio::test]
async fn cancel_rejects_non_owner() {
    let fixture = MarketFixture::new().await;
    let borrower = fixture.create_trader().await;
    let stranger = fixture.create_trader().await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            2_000,
            TERM,
            1_000_000,
            50_000_000,
            RESIDUAL_MODE_REST,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // The stranger has no seat → the cancel must fail (NoSeatClaimed),
    // and even with a seat they could never match the bid's seat owner.
    let cancel_ix = cancel_order_instruction(
        &fixture.market.pubkey(),
        &stranger.pubkey(),
        0,
        None,
    );
    let result = fixture
        .process_ixs(&[cancel_ix], &[&stranger.insecure_clone()])
        .await;
    assert!(result.is_err(), "non-owner cancel must be rejected");
}
