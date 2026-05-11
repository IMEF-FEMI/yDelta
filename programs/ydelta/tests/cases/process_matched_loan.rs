//! Step 7 — end-to-end test for the `ProcessMatchedLoan` cranker.
//!
//! Flow:
//! 1. Two traders deposit (Alice → debt, Bob → collateral).
//! 2. Alice rests an ask; Bob's bid crosses + matches.
//! 3. The matching engine inserts a `MatchedLoan` node into the
//!    market's queue. Borrower (Bob)'s seat shouldn't see the credit
//!    yet — that's the cranker's job.
//! 4. Anyone (here, the fixture payer) calls `process_matched_loan`.
//!    Assert the `LoanFixed` PDA exists with the expected fields,
//!    Bob's seat got credited with `amount_to_shares(net_principal)`,
//!    the `matched_loans_root_index` advances back to NIL, and the
//!    market's `accumulated_protocol_fee_shares` picked up the
//!    origination split (zero in this test — `origination_bps = 0`
//!    by default).

use hypertree::NIL;
use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::Bank;
use ydelta::math::{div_scale, to_scaled};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::{
    loan::{loan_pda, LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE},
    MarketFixed, OrderType, Side,
};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

#[tokio::test]
async fn promote_matched_loan_credits_borrower_and_frees_node() {
    let fixture = MarketFixture::new().await;

    // ─── Alice = lender (debt-side / USDC), Bob = borrower (collateral / wSOL) ───
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

    // Alice deposits 10 USDC of debt-side liquidity through the real
    // marginfi adapter — that's the side `process_matched_loan` reads
    // (debt bank) for `amount_to_shares`.
    let alice_deposit_atoms: u64 = 10_000_000;
    fixture
        .deposit(
            &alice,
            alice_usdc,
            /*is_debt=*/ true,
            alice_deposit_atoms,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bob's collateral-side balance is seeded directly into the seat —
    // matching only reads encumbered shares, so we skip the round-trip
    // through the SOL bank for this test.
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    // ─── Build a crossable book: Alice asks 1_000_000 atoms @ 6% / 30d.
    // (Asks lend the debt-side, encumbering debt-side shares.) Bob bids
    // the same principal @ 8% / 30d, posting the necessary collateral.
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

    // After matching, the market's matched_loans_root_index should point
    // at a single node with sequence 0 (the first matched loan), and
    // matched_loan_sequence has been bumped to 1.
    let market_data = fixture.account_data(fixture.market.pubkey()).await;
    let header_pre: &MarketFixed =
        bytemuck::from_bytes(&market_data[..std::mem::size_of::<MarketFixed>()]);
    assert_ne!(
        header_pre.matched_loans_root_index, NIL,
        "expected a MatchedLoan node to be present pre-crank"
    );
    assert_eq!(
        header_pre.matched_loan_sequence, 1,
        "matched_loan_sequence should advance to 1 after the first match"
    );
    let cranking_sequence: u64 = 0;

    // Bob's seat shouldn't carry any debt withdrawable shares yet — the
    // cranker is what credits him.
    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(bob_seat_pre.debt_withdrawable_shares, 0);

    // ─── Crank.
    fixture.crank_matched_loan(cranking_sequence).await.unwrap();

    // Loan PDA exists with expected values.
    let (loan_addr, loan_bump) = loan_pda(&fixture.market.pubkey(), cranking_sequence);
    let loan_data = fixture.account_data(loan_addr).await;
    assert_eq!(loan_data.len(), LOAN_FIXED_SIZE);
    let loan: &LoanFixed = bytemuck::from_bytes(&loan_data[..std::mem::size_of::<LoanFixed>()]);
    assert_eq!(loan.market, fixture.market.pubkey());
    assert_eq!(loan.matched_loan_sequence, cranking_sequence);
    assert_eq!(loan.bump, loan_bump);
    assert_eq!(loan.state, LoanState::Active as u8);
    assert_eq!(loan.loan_type, LoanType::Fixed as u8);
    assert_eq!(loan.principal_debt_atoms, principal_atoms);
    assert_eq!(loan.outstanding_debt_atoms, principal_atoms); // origination_bps = 0
    assert_eq!(loan.lender_claimable_atoms, principal_atoms);
    assert_eq!(loan.borrower_rate_bps, 800);
    assert_eq!(loan.lender_rate_bps, 600);
    assert!(loan.matures_at_unix > loan.started_at_unix);

    // Bob's seat is now credited with amount_to_shares(net_principal).
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let expected_credit = amount_to_shares_against(&bank_data, principal_atoms);
    let bob_seat_post = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        bob_seat_post.debt_withdrawable_shares, expected_credit,
        "borrower seat credited with amount_to_shares(net_principal) = {} (got {})",
        expected_credit, bob_seat_post.debt_withdrawable_shares
    );

    // The MatchedLoan tree is now empty — the only queued node was just
    // promoted and its slot returned to the free list.
    let market_data_post = fixture.account_data(fixture.market.pubkey()).await;
    let header_post: &MarketFixed =
        bytemuck::from_bytes(&market_data_post[..std::mem::size_of::<MarketFixed>()]);
    assert_eq!(
        header_post.matched_loans_root_index, NIL,
        "matched_loans tree should be empty after cranking the only node"
    );

    // origination_bps defaulted to 0, so no fee accrued.
    assert_eq!(header_post.accumulated_protocol_fee_shares, 0);
}
