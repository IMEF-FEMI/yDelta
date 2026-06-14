//! v1 D6 bid-side coverage (phase 4a): a borrower's unfilled residual
//! can REST in the bids tree instead of dropping or falling through to
//! P2Pool. The rested bid keeps its collateral encumbered (still earning
//! marginfi supply yield), is tracked on the borrower's `UserAccount`,
//! and `CancelOrder` releases exactly the stored-snapshot encumbrance.

use hypertree::NIL;
use solana_sdk::signer::Signer;

use ydelta::program::instruction_builders::cancel_order_instruction::cancel_order_instruction;
use ydelta::program::instruction_builders::update_order_instruction::update_order_instruction;
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

/// v1 D6: `UpdateOrder` cancel-and-replaces in place — new rate / term /
/// expiry under a fresh sequence; the encumbrance (and its snapshot) is
/// untouched, and the `UserOrderRef` re-keys to the new sequence.
#[tokio::test]
async fn update_order_replaces_in_place() {
    let fixture = MarketFixture::new().await;
    let borrower = fixture.create_trader().await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let withdrawable_pre = fixture
        .read_seat(&borrower.pubkey())
        .await
        .collateral_withdrawable_shares;

    // Empty book: the whole bid rests as sequence 0.
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
    let encumbered_at_rest = fixture
        .read_seat(&borrower.pubkey())
        .await
        .collateral_encumbered_shares;

    // Replace rate/term/expiry. The bid takes the next sequence (1).
    let update_ix = update_order_instruction(
        &fixture.market.pubkey(),
        &borrower.pubkey(),
        /*order_sequence=*/ 0,
        None,
        /*new_rate_bps=*/ 2_500,
        /*new_term_seconds=*/ TERM * 2,
        /*new_last_valid_unix_ts=*/ 0,
        /*new_ltv_buffer_bps=*/ 0,
    );
    fixture
        .process_ixs(&[update_ix], &[&borrower.insecure_clone()])
        .await
        .expect("owner update must succeed");

    let seat = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(
        seat.collateral_encumbered_shares, encumbered_at_rest,
        "update keeps the encumbrance untouched (no unencumber round-trip)"
    );
    let ua = fixture
        .account_data(ydelta::state::user_account::user_account_pda(&borrower.pubkey()).0)
        .await;
    let ua_fixed: &ydelta::state::user_account::UserAccountFixed =
        bytemuck::from_bytes(&ua[..ydelta::state::USER_ACCOUNT_FIXED_SIZE]);
    assert_eq!(ua_fixed.user_order_count, 1, "still exactly one open order");

    // The old sequence is dead…
    fixture.refresh_blockhash().await;
    let stale_cancel = cancel_order_instruction(&fixture.market.pubkey(), &borrower.pubkey(), 0, None);
    assert!(
        fixture
            .process_ixs(&[stale_cancel], &[&borrower.insecure_clone()])
            .await
            .is_err(),
        "cancel by the replaced sequence must fail"
    );

    // …and the fresh one cancels cleanly, releasing the full snapshot.
    fixture.refresh_blockhash().await;
    let cancel_ix = cancel_order_instruction(&fixture.market.pubkey(), &borrower.pubkey(), 1, None);
    fixture
        .process_ixs(&[cancel_ix], &[&borrower.insecure_clone()])
        .await
        .expect("cancel by the new sequence must succeed");

    let seat_post = fixture.read_seat(&borrower.pubkey()).await;
    assert_eq!(seat_post.collateral_encumbered_shares, 0);
    assert_eq!(
        seat_post.collateral_withdrawable_shares, withdrawable_pre,
        "snapshot preserved across the update: cancel restores exactly the pre-bid balance"
    );
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.bids_root_index, NIL);
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

/// v1 D7: a curator's ask placement TAKES — a bid resting against an
/// empty book fills the moment a crossable ask arrives.
#[tokio::test]
async fn ask_placement_takes_resting_bid() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Borrower rests first (empty book).
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
            3_000,
            TERM,
            1_000_000,
            50_000_000,
            RESIDUAL_MODE_REST,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Curator funds + places — the placement's take pass crosses the
    // resting bid in the same instruction.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            1,
            Some(8_000),
            /*spread=*/ 100,
            TERM,
            100_000_000,
        )
        .await;

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "ask placement must take the crossable resting bid (v1 D7)"
    );
    assert_eq!(market.bids_root_index, NIL, "fully-consumed bid leaves the tree");
    let sv = fixture.read_sub_vault(1).await;
    assert_eq!(sv.encumbered_in_orders_atoms, 1_000_000);
    assert_eq!(sv.open_loans_count, 1);
    // The fill's collateral stays encumbered on the bidder's seat.
    let seat = fixture.read_seat(&borrower.pubkey()).await;
    assert!(seat.collateral_encumbered_shares > 0);
    assert_eq!(seat.open_borrow_count, 1);
}

/// v1 D8 canonical: a crossed-at-rest book (bid rested while the ask's
/// sub-vault had no idle) is resolved by the permissionless MatchCrank
/// after a vault deposit replenishes idle — no order event in between.
#[tokio::test]
async fn crank_fills_after_idle_replenishment() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;
    let keeper = fixture.create_trader().await;

    // Ask rests with a tiny idle pool (1_000 atoms): the borrower's 1M
    // bid partially fills and the remainder RESTS against the now
    // idle-exhausted ask.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            1,
            Some(8_000),
            /*spread=*/ 100,
            TERM,
            /*deposit_atoms=*/ 1_000,
        )
        .await;

    // Borrower's bid skips the idle-exhausted ask and RESTS → book is
    // crossed at rest (legal, v1 D8).
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
            3_000,
            TERM,
            1_000_000,
            50_000_000,
            RESIDUAL_MODE_REST,
        )
        .await
        .unwrap();
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "the tiny idle partially fills; the remainder rests"
    );
    assert_ne!(market.bids_root_index, NIL, "residual bid rests (crossed-at-rest)");
    assert_ne!(market.asks_root_index, NIL, "ask still rests");

    // Idle replenishes via a VAULT event — no market order flow.
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        crate::test_utils::mainnet::usdc_mint(),
        depositor.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Permissionless crank resolves the cross.
    fixture.match_crank(&keeper, 16).await.expect("crank");

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "crank crosses the rested remainder once idle returns (v1 D8)"
    );
    assert_eq!(market.bids_root_index, NIL, "bid consumed");
    // Partial fill at rest-time + crank fill = the full bid principal.
    let sv = fixture.read_sub_vault(1).await;
    assert_eq!(sv.encumbered_in_orders_atoms, 1_000_000);
    assert_eq!(sv.open_loans_count, 2);
}
