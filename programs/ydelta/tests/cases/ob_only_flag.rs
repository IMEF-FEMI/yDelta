//! `OB_ONLY` flag sanity test.
//!
//! Native `TestFixture` uses synthetic banks, so the actual P2Pool
//! `marginfi.borrow` CPI cannot fire here; both `OB_ONLY` and the
//! default observe the same external behaviour (residual rests).
//! This test
//! pins the plumbing — flag accepted, ix succeeds, residual rests as
//! expected — so the regression set is in place when Step 9.5 wires
//! the CPI.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::state::market_helpers::FLAG_OB_ONLY;
use ydelta::state::{OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn ob_only_flag_accepted_by_place_order() {
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

    // Bob places a Limit Bid with the OB_ONLY flag set + a principal
    // big enough to leave a residual after any matching pass. Asks
    // tree is empty, so the entire principal will rest.
    let principal: u64 = 1_000_000;
    let collateral: u64 = 100_000_000;
    let result = fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
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

    // Residual rests as a Bid on the book (one resting bid, no asks).
    let market = fixture.read_market_fixed().await;
    use hypertree::NIL;
    assert_ne!(
        market.bids_root_index, NIL,
        "Limit Bid with residual should have rested"
    );
}
