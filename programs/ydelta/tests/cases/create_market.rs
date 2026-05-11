use crate::test_utils::TestFixture;

#[tokio::test]
async fn create_market_initialises_header() {
    let fixture = TestFixture::new().await;
    let market = fixture.read_market().await;

    assert_eq!(market.fixed.debt_mint, fixture.debt_mint_key());
    assert_eq!(market.fixed.collateral_mint, fixture.collateral_mint_key());
    assert_eq!(market.fixed.order_sequence_number, 0);
    assert_eq!(market.fixed.matched_loan_sequence, 0);
    assert_eq!(market.fixed.position_count, 0);
    // The first call to `expand_market` after init wires one block onto the
    // free list; the head should therefore be non-NIL.
    assert!(market.fixed.has_free_block());
}
