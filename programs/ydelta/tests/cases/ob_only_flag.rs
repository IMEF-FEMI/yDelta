//! `OB_ONLY` flag sanity test.
//!
//! Native `TestFixture` uses synthetic banks, so the actual P2Pool
//! `marginfi.borrow` CPI cannot fire here; both `OB_ONLY` and the
//! default observe the same external behaviour (residual rests).
//! This test pins the plumbing — flag accepted, ix succeeds, residual
//! rests as expected.

use hypertree::NIL;
use solana_sdk::signer::Signer;

use ydelta::state::market_helpers::FLAG_OB_ONLY;
use ydelta::state::{OrderType, Side};

use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn ob_only_flag_accepted_by_place_order() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // Quote-only: lenders fund via vaults, not seat debt-deposits. This
    // test only needs Bob's borrower bid against an empty asks tree, so
    // seed his collateral directly.
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    // Bob places a borrower IOC bid with the OB_ONLY flag set. The
    // asks tree is empty, so nothing matches and the entire principal
    // is a residual. With OB_ONLY set the residual is dropped (it does
    // not fall through to the P2Pool fallback) — and a borrower bid
    // never rests on the book.
    let principal: u64 = 1_000_000;
    let collateral: u64 = 100_000_000;
    let result = fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            3_000,
            30 * 86_400,
            principal,
            collateral,
            FLAG_OB_ONLY,
        )
        .await;
    assert!(
        result.is_ok(),
        "place_order with OB_ONLY flag should succeed: {:?}",
        result
    );

    // No asks-tree resting order and no MatchedLoan: the OB_ONLY
    // residual was dropped, not rested and not auto-borrowed.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.asks_root_index, NIL,
        "no order should rest — a borrower bid is IOC-only"
    );
    assert_eq!(
        market.matched_loan_sequence, 0,
        "OB_ONLY residual must drop, not trigger the P2Pool fallback"
    );
}
