//! `set_fee_config` admin-gated ix tests.

use solana_sdk::signer::Signer;

use crate::test_utils::MarketFixture;

#[tokio::test]
async fn admin_can_update_liquidation_keeper_bps() {
    let fixture = MarketFixture::new().await;

    // The fixture's `payer` is the create_market signer, which becomes
    // the admin. Build the ix with payer as signer.
    use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
    use ydelta::program::processor::set_fee_config::SetFeeConfigParams;

    let mut params = SetFeeConfigParams::default();
    params.liquidation_keeper_bps = Some(750);
    params.ltv_buffer_bps = Some(200);

    let ix = set_fee_config_instruction(&fixture.market.pubkey(), &fixture.payer.pubkey(), params);
    let kp = fixture.payer.insecure_clone();
    fixture.process(ix, &[&kp]).await.unwrap();

    let market = fixture.read_market_fixed().await;
    assert_eq!(market.fee_config.liquidation_keeper_bps, 750);
    assert_eq!(market.fee_config.ltv_buffer_bps, 200);
}

#[tokio::test]
async fn non_admin_set_fee_config_rejects() {
    let fixture = MarketFixture::new().await;
    let interloper = fixture.create_trader().await;

    use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
    use ydelta::program::processor::set_fee_config::SetFeeConfigParams;

    let mut params = SetFeeConfigParams::default();
    params.liquidation_keeper_bps = Some(750);

    let ix = set_fee_config_instruction(&fixture.market.pubkey(), &interloper.pubkey(), params);
    let kp = interloper.insecure_clone();
    let result = fixture.process(ix, &[&kp]).await;
    assert!(
        result.is_err(),
        "non-admin must be rejected by set_fee_config gate"
    );
}

#[tokio::test]
async fn out_of_range_bps_rejected() {
    let fixture = MarketFixture::new().await;

    use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
    use ydelta::program::processor::set_fee_config::SetFeeConfigParams;

    let mut params = SetFeeConfigParams::default();
    // 10_001 bps > 100% — must be rejected.
    params.liquidation_keeper_bps = Some(10_001);

    let ix = set_fee_config_instruction(&fixture.market.pubkey(), &fixture.payer.pubkey(), params);
    let kp = fixture.payer.insecure_clone();
    let result = fixture.process(ix, &[&kp]).await;
    assert!(
        result.is_err(),
        "out-of-range bps (>10_000) must be rejected"
    );
}

/// End-to-end: protocol takes an origination fee on Fixed matches when
/// `origination_bps > 0`. Default fee_config has `origination_bps = 0`
/// (which is why every other lifecycle test asserts the accumulator
/// stays at 0). This test sets it to 50 bps (0.5%), runs a Fixed match,
/// and asserts `accumulated_protocol_fee_shares` strictly grew.
///
/// Pairs with `p2pool::bid_unfilled_residual_p2pool_borrows` which
/// asserts the SAME accumulator does NOT grow on P2Pool promotion —
/// together they prove "protocol earns origination on Fixed, not P2Pool".
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn protocol_accumulates_origination_fee_on_fixed_match() {
    use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
    use ydelta::program::processor::set_fee_config::SetFeeConfigParams;
    use ydelta::state::{OrderType, Side};

    use crate::test_utils::marginfi_fixture::mainnet;
    use solana_program::pubkey::Pubkey;

    let fixture = MarketFixture::new().await;

    // ── Set origination_bps = 50 (0.5%) before any matching. ──
    let mut params = SetFeeConfigParams::default();
    params.origination_bps = Some(50);
    let ix = set_fee_config_instruction(&fixture.market.pubkey(), &fixture.payer.pubkey(), params);
    let kp = fixture.payer.insecure_clone();
    fixture.process(ix, &[&kp]).await.unwrap();
    fixture.refresh_blockhash().await;

    let market_pre = fixture.read_market_fixed().await;
    assert_eq!(market_pre.fee_config.origination_bps, 50);
    assert_eq!(
        market_pre.accumulated_protocol_fee_shares, 0,
        "no fees accrued yet"
    );

    // ── Standard wallet match (alice ASK, bob BID). ──
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

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
    fixture
        .place_order(
            &alice,
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
            &bob,
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

    // ── Assertion: accumulated_protocol_fee_shares grew strictly. ──
    let market_post = fixture.read_market_fixed().await;
    assert!(
        market_post.accumulated_protocol_fee_shares > 0,
        "origination_bps=50 must produce a non-zero fee accumulator on Fixed match \
         (was {}, now {})",
        market_pre.accumulated_protocol_fee_shares,
        market_post.accumulated_protocol_fee_shares
    );
}
