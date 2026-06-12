//! v1 D9: self-cross is blocked at the OWNER level — a wallet never
//! lends to itself through a sub-vault it curates. The gate is a SKIP
//! (the scan moves to the next order), never an abort, on all engine
//! paths: borrower IOC bid, ask-placement take pass, and MatchCrank.

use hypertree::NIL;
use solana_sdk::signer::Signer;

use ydelta::state::market_helpers::RESIDUAL_MODE_REST;

use crate::test_utils::MarketFixture;

const TERM: u32 = 30 * 86_400;

/// A borrower IOC bid skips the BEST ask when its sub-vault is curated
/// by the bidder, and fills from the next (worse) ask instead.
#[tokio::test]
async fn ioc_bid_skips_own_curated_ask() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let other_curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Sub-vault 1: curated by the borrower, BEST ask (lowest spread).
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &borrower,
            1,
            Some(8_000),
            /*spread=*/ 100,
            TERM,
            100_000_000,
        )
        .await;
    // Sub-vault 2: someone else's, worse rate.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &other_curator,
            2,
            Some(8_000),
            /*spread=*/ 200,
            TERM,
            100_000_000,
        )
        .await;

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
    assert_eq!(market.matched_loan_sequence, 1, "exactly one fill");
    assert_eq!(market.bids_root_index, NIL, "bid fully consumed");
    let own = fixture.read_sub_vault(1).await;
    assert_eq!(own.open_loans_count, 0, "own sub-vault skipped (D9)");
    assert_eq!(own.encumbered_in_orders_atoms, 0);
    let other = fixture.read_sub_vault(2).await;
    assert_eq!(other.open_loans_count, 1, "fill came from the next ask");
    assert_eq!(other.encumbered_in_orders_atoms, 1_000_000);
}

/// The take pass of a curator's own ask placement — and a later crank —
/// both skip the curator's resting bid; the cross stays at rest (legal,
/// D8) until a third party's ask arrives.
#[tokio::test]
async fn take_pass_and_crank_skip_curators_own_bid() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let other_curator = fixture.create_trader().await;
    let wallet = fixture.create_trader().await; // bidder AND sv1 curator
    let keeper = fixture.create_trader().await;

    // The wallet rests a bid first (empty book).
    fixture.claim_seat(&wallet).await;
    let wallet_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(wallet_wsol, wallet.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&wallet, wallet_wsol, false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &wallet,
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

    // The wallet's own ask placement must NOT take its bid.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &wallet,
            1,
            Some(8_000),
            /*spread=*/ 100,
            TERM,
            100_000_000,
        )
        .await;
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 0,
        "take pass skips the curator's own bid (D9)"
    );
    assert_ne!(market.bids_root_index, NIL, "bid still rests");
    assert_ne!(market.asks_root_index, NIL, "crossed-at-rest is legal (D8)");

    // A permissionless crank must skip it too.
    fixture.refresh_blockhash().await;
    fixture.match_crank(&keeper, 16).await.expect("crank runs");
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 0, "crank skips the self-cross");
    assert_ne!(market.bids_root_index, NIL);

    // A third party's ask fills it.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &other_curator,
            2,
            Some(8_000),
            /*spread=*/ 200,
            TERM,
            100_000_000,
        )
        .await;
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1, "third-party ask takes the bid");
    assert_eq!(market.bids_root_index, NIL);
    assert_eq!(fixture.read_sub_vault(1).await.open_loans_count, 0);
    assert_eq!(fixture.read_sub_vault(2).await.open_loans_count, 1);
}
