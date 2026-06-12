//! v1 D4/D5 rate-model coverage: placement computes the stored ask rate
//! as `live marginfi lending APR (ceil bps) + sub_vault.spread_bps`, and
//! the fill-time floor skips asks whose stored rate has fallen below the
//! live APR until the curator re-syncs.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

const TERM: u32 = 30 * 86_400;

/// Placement pins the spread-over-bank rate (v1 D4): the stored ask is
/// exactly the snapshot bank's live APR plus the creation spread.
#[tokio::test]
async fn placement_stores_bank_apr_plus_spread() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*spread (old rate param)=*/ 250,
            TERM,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    let stored = fixture
        .best_ask_rate_bps()
        .await
        .expect("ask must rest");
    assert_eq!(
        stored,
        250 + mainnet::USDC_LIVE_LENDING_APR_BPS,
        "stored ask rate = live bank APR (ceil) + sub-vault spread"
    );
}

/// Fill-time floor (v1 D5): after a utilization spike pushes the live
/// APR above a resting ask's stored rate, the ask stops filling; a
/// parameterless `update_order_for_sub_vault` re-syncs it and the next
/// bid fills.
#[tokio::test]
async fn floor_skips_stale_ask_until_resync() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

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
    let stale_rate = fixture.best_ask_rate_bps().await.unwrap();

    // Utilization spike: +20% liabilities → live APR rises well above
    // the resting ask's stored rate (the curve is steep near optimal).
    fixture.scale_usdc_bank_liabilities(12, 10).await;
    fixture.refresh_blockhash().await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 500_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 50_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bid far above every rate, OB_ONLY so the unfilled residual drops.
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            9_000,
            TERM,
            1_000_000,
            50_000_000,
            ydelta::state::market_helpers::RESIDUAL_MODE_DROP,
        )
        .await
        .expect("floor skip must drop the bid, not fail the tx");

    let sv = fixture.read_sub_vault(1).await;
    assert_eq!(
        sv.encumbered_in_orders_atoms, 0,
        "stale ask below the live APR must be skipped (v1 D5 floor)"
    );

    // Curator re-syncs — parameterless cancel-and-replace at the NEW
    // live APR + spread.
    fixture.refresh_blockhash().await;
    fixture
        .update_order_for_sub_vault(&curator, 1)
        .await
        .expect("parameterless re-sync");
    let synced_rate = fixture.best_ask_rate_bps().await.unwrap();
    assert!(
        synced_rate > stale_rate,
        "re-synced ask must carry the spiked APR (was {}, now {})",
        stale_rate,
        synced_rate
    );

    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            9_000,
            TERM,
            1_000_000,
            50_000_000,
            ydelta::state::market_helpers::RESIDUAL_MODE_DROP,
        )
        .await
        .unwrap();
    let sv = fixture.read_sub_vault(1).await;
    assert_eq!(
        sv.encumbered_in_orders_atoms, 1_000_000,
        "re-synced ask fills"
    );
}
