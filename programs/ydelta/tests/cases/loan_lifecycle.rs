//! Step 10 — end-to-end smoke + multi-loan isolation.
//!
//! `lifecycle_smoke` runs the entire happy path top-to-bottom in one
//! test (create_market → deposits → match → crank → repay → claim)
//! and asserts the share/atom-conservation invariant: post-flow the
//! lender has back roughly what they deposited (within ±1 share),
//! the loan PDA is closed, and protocol fees are zero
//! (origination_bps = 0 by default).
//!
//! `two_independent_loans` exercises cross-loan isolation: two
//! disjoint matches in one fixture, both cranked + repaid + claimed
//! independently. Tests that the matched-loan tree, the Loan PDAs,
//! and the seat bookkeeping all stay disjoint per loan.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::Bank;
use ydelta::math::{div_scale, to_scaled};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::{loan::LoanState, OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

#[tokio::test]
async fn lifecycle_smoke_create_match_crank_repay_claim() {
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
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    fixture.refresh_blockhash().await;

    let lender_deposit_atoms: u64 = 10_000_000;
    fixture
        .deposit(
            &alice,
            alice_usdc,
            /*is_debt=*/ true,
            lender_deposit_atoms,
        )
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
    fixture.refresh_blockhash().await;

    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Advance to maturity for the fixed-term lock-up gate.
    let loan_pre_claim = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_pre_claim.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment(&alice, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();

    // Sanity: lender ended with at least their initial deposit
    // shares worth on the withdrawable side (the loan principal came
    // back through the claim path). Strict share conservation is
    // loose because encumbrance/match bookkeeping carries
    // atom-as-share semantics under the placeholder fp48 share-price.
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let initial_shares = amount_to_shares_against(&bank_data, lender_deposit_atoms);
    let alice_seat = fixture.read_seat(&alice.pubkey()).await;
    assert!(
        alice_seat.debt_withdrawable_shares >= initial_shares.saturating_sub(1u128 << 48),
        "lender withdrawable={} fell below initial deposit {} after lifecycle",
        alice_seat.debt_withdrawable_shares,
        initial_shares
    );

    // Loan PDA is gone.
    let (loan_addr, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), 0);
    let post_account = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(loan_addr).await.unwrap()
    };
    assert!(post_account.is_none(), "loan PDA should be collected");

    // Both seats are at zero open positions.
    let bob_seat = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(alice_seat.open_lend_count, 0);
    assert_eq!(bob_seat.open_borrow_count, 0);

    // No protocol fee accrued (origination_bps = 0 by default).
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.accumulated_protocol_fee_shares, 0);
}

#[tokio::test]
async fn two_independent_loans_remain_disjoint() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let carol = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;
    fixture.claim_seat(&carol).await;

    let alice_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        alice_usdc,
        mainnet::usdc_mint(),
        alice.pubkey(),
        100_000_000,
    );
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    let carol_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        carol_usdc,
        mainnet::usdc_mint(),
        carol.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;

    fixture
        .deposit(&alice, alice_usdc, /*is_debt=*/ true, 20_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture
        .seed_seat_shares(&carol.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    let principal: u64 = 1_000_000;
    let coll: u64 = 100_000_000;

    // Loan 0: alice ↔ bob.
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            principal,
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
            principal,
            coll,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Loan 1: alice ↔ carol.
    //
    // `solana-program-test` deduplicates transactions by signature.
    // Alice's first and second ASK have identical instruction bytes
    // (same signer, same params); back-to-back `refresh_blockhash`
    // calls within the same slot return the same blockhash, so the
    // second tx ends up with the same signature as the first and is
    // silently dropped. Advance the slot to force a new blockhash
    // before alice's second placement.
    {
        let cur = fixture
            .context
            .borrow()
            .banks_client
            .get_root_slot()
            .await
            .unwrap();
        fixture.context.borrow_mut().warp_to_slot(cur + 1).unwrap();
    }
    fixture.refresh_blockhash().await;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            principal,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .place_order(
            &carol,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal,
            coll,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Two queued matches.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 2);

    // Crank both.
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(1).await.unwrap();
    fixture.refresh_blockhash().await;

    // Both loans exist, are Active, with the right borrowers.
    let loan0 = fixture.read_loan(0).await;
    let loan1 = fixture.read_loan(1).await;
    assert_eq!(loan0.state, LoanState::Active as u8);
    assert_eq!(loan1.state, LoanState::Active as u8);
    assert_ne!(
        loan0.borrower_seat_index, loan1.borrower_seat_index,
        "borrower seat indices must differ between independent loans"
    );

    // Bob repays only loan 0, claims loan 0 → loan 1 untouched.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Advance past loan 0's maturity for the lock-up gate. Both
    // loans share the same term in this test so a single advance
    // covers both claims below.
    let max_matures = loan0.matures_at_unix.max(loan1.matures_at_unix);
    fixture.set_clock_unix_timestamp(max_matures + 1).await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment(&alice, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Loan 1 still Active.
    let loan1_after = fixture.read_loan(1).await;
    assert_eq!(loan1_after.state, LoanState::Active as u8);
    assert_eq!(
        loan1_after.outstanding_debt_atoms,
        loan1.outstanding_debt_atoms
    );

    // Carol can repay + Alice claim independently.
    fixture
        .repay(&carol, 1, carol_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment(&alice, 1, Some(fixture.payer.pubkey()))
        .await
        .unwrap();

    // Both loan PDAs gone.
    for seq in [0u64, 1u64] {
        let (loan_addr, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), seq);
        let acc = {
            let ctx = fixture.context.borrow_mut();
            ctx.banks_client.get_account(loan_addr).await.unwrap()
        };
        assert!(acc.is_none(), "loan {} should be collected", seq);
    }
}
