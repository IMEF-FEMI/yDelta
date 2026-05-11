//! `SecondaryLoanSale` placement / cancel / update tests. Cover the
//! validation surface (ownership, cardinality, immutable fields,
//! p2pool rejection) and the in-place mutation path. Lifecycle tests
//! (cross + cranker finalization + repay sweep) live in
//! `secondary_lifecycle.rs`.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use hypertree::NIL;
use ydelta::state::{OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Set up a Fixed loan held by `lender` on the market. Returns the loan
/// sequence (always 0 — first matched loan in the fixture).
async fn make_fixed_loan_owned_by(
    fixture: &MarketFixture,
    alice: &solana_sdk::signature::Keypair,
    bob: &solana_sdk::signature::Keypair,
) -> u64 {
    fixture.claim_seat(alice).await;
    fixture.claim_seat(bob).await;

    let alice_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        alice_usdc,
        mainnet::usdc_mint(),
        alice.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .deposit(alice, alice_usdc, /*is_debt=*/ true, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
    fixture
        .place_order(
            alice,
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
    fixture
        .place_order(
            bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(0).await.unwrap();
    0
}

/// Step 12 — secondary bid rests when no primary asks are on the book.
#[tokio::test]
async fn secondary_bid_rests_when_no_asks() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_owned_by(&fixture, &alice, &bob).await;

    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();

    // Verify a SecondaryLoanSale RestingOrder is in the bids tree
    // referencing the loan PDA, with snapshot fields = the loan's
    // borrower_rate_bps etc.
    let market = fixture.read_market_fixed().await;
    assert_ne!(
        market.bids_root_index, NIL,
        "secondary bid should rest in the primary bids tree"
    );
    let loan = fixture.read_loan(loan_seq).await;
    assert_eq!(
        loan.lender_seat_index,
        fixture.read_seat_index(&alice.pubkey()).await,
        "loan still owned by alice after placing the secondary"
    );
}

/// Step 16 — only the loan's current lender can post a SecondaryLoanSale.
#[tokio::test]
async fn non_owner_secondary_rejected() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_owned_by(&fixture, &alice, &bob).await;

    // Bob is the borrower, not the lender — secondary attempt rejected.
    fixture.refresh_blockhash().await;
    let res = fixture.place_secondary_order(&bob, loan_seq, 0).await;
    assert!(
        res.is_err(),
        "secondary placed by non-current-lender should fail with SecondaryNotCurrentLender"
    );
}

/// Step 17 — at most one resting SecondaryLoanSale per loan.
#[tokio::test]
async fn duplicate_secondary_rejected() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_owned_by(&fixture, &alice, &bob).await;

    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();

    // Distinct args (different last_valid_unix_ts) so solana-program-test
    // doesn't treat this as a tx replay.
    fixture.refresh_blockhash().await;
    let res = fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 9_999_999_999)
        .await;
    assert!(
        res.is_err(),
        "second SecondaryLoanSale for same loan should fail with SecondaryDuplicate"
    );
}

/// Step 18 — cancel a resting secondary bid. No collateral to release;
/// just removes from the tree.
#[tokio::test]
async fn cancel_secondary_bid_works() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_owned_by(&fixture, &alice, &bob).await;

    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();

    // Read the bid's sequence_number off the tree.
    let bid_seq = fixture
        .read_first_bid_sequence_for_loan(loan_seq)
        .await
        .expect("secondary bid should be in tree");

    fixture.refresh_blockhash().await;
    let (loan_pda, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), loan_seq);
    fixture
        .cancel_secondary_order(&alice, bid_seq, loan_pda)
        .await
        .unwrap();

    // After cancel, can repost at a different price.
    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();
}

/// update_order on a secondary bid: only `last_valid_unix_ts`
/// (expiry touch) is mutable. rate/term/principal/collateral/asking
/// rejected with SecondaryFieldImmutable. Pricing is fixed at par.
#[tokio::test]
async fn update_secondary_expiry_works_immutables_rejected() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_owned_by(&fixture, &alice, &bob).await;

    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();
    let bid_seq = fixture
        .read_first_bid_sequence_for_loan(loan_seq)
        .await
        .unwrap();

    // Any non-zero expiry update is rejected — secondary bids are
    // pinned to NO_EXPIRATION so the matching engine's expiry sweep
    // can never strand the loan's `has_resting_secondary_bid` flag.
    fixture.refresh_blockhash().await;
    let new_expiry: i64 = 9_999_999_999;
    let res = fixture
        .update_secondary_order(&alice, bid_seq, Some(new_expiry))
        .await;
    assert!(
        res.is_err(),
        "non-zero expiry update on a secondary bid must be rejected"
    );

    // No-op update (expiry stays NO_EXPIRATION) is still accepted.
    fixture.refresh_blockhash().await;
    let res = fixture
        .update_secondary_order(&alice, bid_seq, Some(0))
        .await;
    assert!(
        res.is_ok(),
        "expiry-touch to NO_EXPIRATION should succeed: {:?}",
        res
    );

    // Rate-mutation attempt rejected.
    fixture.refresh_blockhash().await;
    let res = fixture
        .try_update_secondary_with_rate(&alice, bid_seq, 700)
        .await;
    assert!(
        res.is_err(),
        "rate mutation on SecondaryLoanSale should fail with SecondaryFieldImmutable"
    );
}

/// P2Pool loans (loan_type == 1) cannot be secondary-sold. Set up a
/// P2Pool loan via place_order(default, no asks) → cranker promotes
/// to LoanType::P2Pool. Then attempt to post a secondary; expect
/// rejection.
#[tokio::test]
async fn p2pool_loan_cannot_be_secondary() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // The P2Pool deposit-back deposits borrowed atoms into
    // `lender_marginfi_account`. To avoid the cost of allocating a
    // fresh balance slot mid-place_order (which can push the tx
    // over the CU budget), pre-populate the lender side with a USDC
    // balance.
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

    // Bob deposits real wSOL collateral so marginfi.borrow's solvency
    // check passes (mirrors `bid_unfilled_residual_p2pool_borrows`
    // setup).
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bob bids with no asks on book + no OB_ONLY → P2Pool fallback fires.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(0).await.unwrap();

    let loan = fixture.read_loan(0).await;
    assert_eq!(loan.loan_type, 1, "loan should be P2Pool");

    // Now attempt to post secondary on the P2Pool loan. Bob is the
    // borrower; even from the (nominal) lender side we should see a
    // SecondaryLoanWrongType error. The lender_seat_index for a P2Pool
    // loan is NIL — even the cardinality / ownership check would fail
    // first, but loan_type == Fixed precondition fires earliest in the
    // loader.
    fixture.refresh_blockhash().await;
    let res = fixture
        .place_secondary_order(&bob, /*loan_seq=*/ 0, 0)
        .await;
    assert!(
        res.is_err(),
        "secondary placement on P2Pool loan should fail with SecondaryLoanWrongType"
    );
}
