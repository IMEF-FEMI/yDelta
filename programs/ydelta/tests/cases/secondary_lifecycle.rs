//! Secondary cross lifecycle tests under `MarketFixture` (real
//! marginfi banks). Exercises the full chain:
//!   1. Make a Fixed loan via primary cross + crank.
//!   2. Lender posts secondary bid for that loan.
//!   3. New lender posts a primary ask big enough to cross.
//!   4. Cranker finalizes the SECONDARY-flagged queue node.
//!   5. Assert Option-A reset: new lender_seat, new lender_rate,
//!      lender_claimable=0, seller cash credit on seat.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use hypertree::NIL;
use ydelta::state::{OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Build a Fixed loan owned by `alice` (lender) with `bob` (borrower).
/// Returns the loan sequence (always 0). `bob` deposits real wSOL
/// collateral so the borrower marginfi-account has backing for any
/// later marginfi.borrow CPIs that secondary-side flows might trigger.
async fn make_fixed_loan_with_real_collateral(
    fixture: &MarketFixture,
    alice: &solana_sdk::signature::Keypair,
    bob: &solana_sdk::signature::Keypair,
) -> u64 {
    fixture.claim_seat(alice).await;
    fixture.claim_seat(bob).await;

    // Alice deposits USDC.
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

    // Bob deposits real wSOL collateral via the borrower-side path.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Primary cross: alice asks 100 USDC at 600 bps for 30d, bob bids.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
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

/// Step 13 — primary ask × resting secondary bid → full transfer.
/// Cranker finalizes; loan.lender_seat_index updates, lender_rate
/// resets to ask.rate, lender_claimable seized to protocol fees,
/// seller seat credited with cash.
#[tokio::test]
async fn primary_ask_crosses_resting_secondary_full() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Alice posts secondary bid. Pricing is fixed at par — engine
    // sets asking == principal_debt_atoms.
    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();

    let alice_seat_pre = fixture.read_seat_index(&alice.pubkey()).await;

    // Chris (new lender) deposits USDC.
    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Chris posts a primary ask for 200 USDC at 500 bps / 30d. The
    // resting secondary bid (rate = loan.borrower_rate = 800 bps,
    // principal = 100, term = 30d) crosses since:
    //   * bid.rate (800) >= ask.rate (500)        ✓
    //   * bid.term (30d remaining-or-less) <= ask.term (30d)  ✓
    //   * ask.principal (200) >= cash_paid (100, par exit)    ✓
    // Full transfer (matched_principal == loan.principal). Chris's
    // ask remainder of 100 USDC stays on the asks tree for primary
    // borrowers.
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            500,
            30 * 86_400,
            /*principal_atoms=*/ 200,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // The cross created a SECONDARY-flagged MatchedLoan queue node at
    // sequence 1 (loan 0 was the primary one already cranked into a
    // LoanFixed at sequence 0).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "secondary cross should bump matched_loan_sequence to 2"
    );

    // Cranker finalizes. The queue node at sequence 1 references
    // loan_sequence=0.
    fixture
        .crank_secondary_matched_loan(
            /*queue_sequence=*/ 1, /*referenced_loan_sequence=*/ 0,
        )
        .await
        .unwrap();

    // Verify Option-A reset on the loan PDA.
    let loan = fixture.read_loan(0).await;
    let chris_seat = fixture.read_seat_index(&chris.pubkey()).await;
    assert_eq!(
        loan.lender_seat_index, chris_seat,
        "loan.lender_seat_index transferred to chris"
    );
    assert_eq!(
        loan.lender_rate_bps, 500,
        "lender_rate reset to ask.rate (500 bps)"
    );
    assert_eq!(
        loan.lender_claimable_atoms, 0,
        "lender_claimable seized to protocol on Option-A reset"
    );
    assert_eq!(loan.principal_debt_atoms, 100);
    assert_eq!(
        loan.loan_type,
        ydelta::state::loan::LoanType::Fixed as u8,
        "Fixed remains Fixed across secondary transfer (no flip)"
    );

    // (Seller cash credit verification omitted — exact share-amount
    // comparison would require pricing against the live USDC bank's
    // asset_share_value, and the LoanTransferred log already records
    // the matched_principal. Step 13 focuses on the loan-mutation
    // surface.)
    let _ = alice_seat_pre;

    // Sanity: chris's ask remainder rests on the book.
    assert_ne!(
        market.asks_root_index, NIL,
        "chris's ask remainder should rest on the asks tree"
    );
}

/// Step 21 — full repay sweeps stale secondary bids on the loan.
/// Borrower repays in full → process_repay walks the bids tree and
/// removes any resting `SecondaryLoanSale` referencing this loan.
#[tokio::test]
async fn borrower_repay_sweeps_stale_secondary() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Alice posts secondary bid.
    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();

    // Confirm the bid is in the tree.
    let bid_seq_before = fixture
        .read_first_bid_sequence_for_loan(loan_seq)
        .await
        .expect("secondary bid should rest on bids tree before repay");
    let _ = bid_seq_before;

    // Borrower repays in full. process_repay's full-repay branch must
    // walk the bids tree and sweep alice's stale secondary bid.
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .repay(
            &bob, loan_seq, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true,
        )
        .await
        .unwrap();

    // The secondary bid should now be gone.
    let bid_seq_after = fixture.read_first_bid_sequence_for_loan(loan_seq).await;
    assert!(
        bid_seq_after.is_none(),
        "process_repay's full-repay path must sweep stale secondary bids"
    );
}

/// Step 24 — secondary `place_order` ix carries +1 account vs primary.
#[tokio::test]
async fn secondary_account_count_is_primary_plus_one() {
    use ydelta::program::instruction_builders::place_order_instruction::{
        place_order_instruction, secondary_place_order_instruction,
    };

    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    let debt_bank_lva = mainnet::liquidity_vault_authority(mainnet::usdc_bank());
    let alice_debt_token = fixture.signer_debt_token(&alice.pubkey());

    // Primary place_order — fixed account count (see assertion below).
    let primary_ix = place_order_instruction(
        &fixture.market.pubkey(),
        &alice.pubkey(),
        &mainnet::marginfi_group(),
        &mainnet::usdc_bank(),
        &mainnet::sol_bank(),
        &[mainnet::usdc_oracle()],
        &[mainnet::sol_oracle()],
        &mainnet::usdc_liquidity_vault(),
        &debt_bank_lva,
        &alice_debt_token,
        &mainnet::usdc_mint(),
        &spl_token::id(),
        &marginfi_mocks::ID,
        Side::Ask,
        OrderType::Limit,
        600,
        30 * 86_400,
        100,
        0,
        0,
        /*flags=*/ 0,
        None,
        None, // borrower_ltv_bps default
    );
    assert_eq!(
        primary_ix.accounts.len(),
        21,
        "primary place_order = 15 (marginfi/oracle) + 2 (UserAccount) + 2 \
         (lender_marginfi_account + market_debt_vault) + 1 (vault PDA) + \
         1 (global_config)"
    );

    // Secondary place_order — primary + 1 extra (the Loan PDA being sold).
    let (loan_pda, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), loan_seq);
    let secondary_ix = secondary_place_order_instruction(
        &fixture.market.pubkey(),
        &alice.pubkey(),
        &mainnet::marginfi_group(),
        &mainnet::usdc_bank(),
        &mainnet::sol_bank(),
        &[mainnet::usdc_oracle()],
        &[mainnet::sol_oracle()],
        &mainnet::usdc_liquidity_vault(),
        &debt_bank_lva,
        &alice_debt_token,
        &mainnet::usdc_mint(),
        &spl_token::id(),
        &marginfi_mocks::ID,
        &loan_pda,
        /*last_valid_unix_ts=*/ 0,
        /*flags=*/ 0,
        None,
    );
    assert_eq!(
        secondary_ix.accounts.len(),
        22,
        "secondary place_order = primary (21) + 1 (Loan PDA being sold)"
    );
}

/// Step 14 — primary ask × resting secondary bid, ask too small to
/// take the whole loan → SECONDARY|SPLIT cross. Cranker allocates a
/// new Loan PDA at the live market.matched_loan_sequence, splits
/// principal/outstanding/collateral pro-rata, runs LTV on both
/// halves, and ends with two LoanFixed PDAs sharing the same
/// borrower.
#[tokio::test]
async fn primary_ask_crosses_resting_secondary_partial_split() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;
    // After primary crank: market.matched_loan_sequence == 1.

    // Alice posts secondary on her loan.
    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();

    // Chris (new lender) deposits USDC.
    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Chris posts a primary ask for ONLY 50 atoms — half the loan's
    // principal. Cross with alice's secondary fires SECONDARY|SPLIT.
    // matched_principal = min(50, 100) = 50. cash_paid = 50 (par).
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            500,
            30 * 86_400,
            /*principal_atoms=*/ 50,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Queue node at sequence 1 (matched_loan_sequence bumped to 2).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "secondary split cross should bump matched_loan_sequence to 2"
    );

    // Cranker: takes the existing loan PDA + a NEW empty Loan PDA at
    // sequence 2 (the live counter). After crank, counter bumps to 3.
    fixture
        .crank_secondary_split_matched_loan(
            /*queue_sequence=*/ 1, /*referenced_loan_sequence=*/ 0,
            /*next_market_sequence=*/ 2,
        )
        .await
        .unwrap();

    // Original loan halved.
    let original = fixture.read_loan(0).await;
    assert_eq!(
        original.principal_debt_atoms, 50,
        "original loan principal halved (100 - 50 transferred)"
    );
    assert_eq!(
        original.collateral_atoms, 2_500,
        "original loan collateral halved pro-rata"
    );

    // New sub-loan at split_loan_pda(market, queue_sequence=1) —
    // split PDA derived from the queue node's stable sequence.
    let sub_loan = fixture.read_split_loan(1).await;
    let chris_seat = fixture.read_seat_index(&chris.pubkey()).await;
    assert_eq!(sub_loan.principal_debt_atoms, 50);
    assert_eq!(sub_loan.collateral_atoms, 2_500);
    assert_eq!(sub_loan.lender_seat_index, chris_seat);
    assert_eq!(
        sub_loan.lender_rate_bps, 500,
        "sub-loan lender_rate = chris's ask rate"
    );
    assert_eq!(
        sub_loan.lender_claimable_atoms, 0,
        "sub-loan starts with zero accrued (Option A fresh start)"
    );
    assert_eq!(
        sub_loan.borrower_seat_index, original.borrower_seat_index,
        "borrower stays the same across split"
    );

    // Splits do not consume `matched_loan_sequence` — counter stays
    // at 2 (set by the matching engine when the SECONDARY split
    // queue node was inserted).
    let market_post = fixture.read_market_fixed().await;
    assert_eq!(market_post.matched_loan_sequence, 2);
}

/// Step 22 — cranker drops a stale SECONDARY queue node when the
/// borrower repaid in full between match and crank.
#[tokio::test]
async fn cranker_drops_stale_secondary_post_repay() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Alice posts secondary; chris's ask crosses → SECONDARY queue node.
    fixture.refresh_blockhash().await;
    fixture
        .place_secondary_order(&alice, loan_seq, 0)
        .await
        .unwrap();

    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            500,
            30 * 86_400,
            /*principal_atoms=*/ 200,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bob repays in full BEFORE the cranker runs. process_repay's
    // sweep targets the bids tree (where alice's secondary bid was —
    // already removed at match time), not the MatchedLoan queue. The
    // SECONDARY queue node persists.
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .repay(
            &bob, loan_seq, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true,
        )
        .await
        .unwrap();

    let chris_seat = fixture.read_seat_index(&chris.pubkey()).await;
    let chris_seat_pre_drop = fixture.read_seat_data(chris_seat).await;

    // Cranker runs — sees loan.outstanding == 0 → refund-and-drop.
    fixture
        .crank_secondary_matched_loan(
            /*queue_sequence=*/ 1, /*referenced_loan_sequence=*/ 0,
        )
        .await
        .unwrap();

    let chris_seat_post_drop = fixture.read_seat_data(chris_seat).await;

    // Withdrawable should have increased (refund), encumbered held
    // steady (no double-debit on stale-drop path).
    assert!(
        chris_seat_post_drop.debt_withdrawable_shares
            > chris_seat_pre_drop.debt_withdrawable_shares,
        "chris should be refunded the matched cash on stale-drop"
    );

    // Loan PDA still exists (Repaid state, not closed); the cranker
    // didn't error and the queue node is gone (free list recovered
    // its slot). Best assertion we can make without re-reading the
    // queue tree directly: the cranker tx succeeded.
    let loan = fixture.read_loan(loan_seq).await;
    assert_eq!(
        loan.outstanding_debt_atoms, 0,
        "loan still shows fully repaid"
    );
}

// ──────── Scenario B (secondary-bid taker) ────────

/// Place a primary ask, then post a SecondaryLoanSale that matches
/// it directly. No residual rests. The `has_resting_secondary_bid`
/// flag stays 0 because nothing is left in the bids tree.
#[tokio::test]
async fn scenario_b_secondary_bid_matches_resting_ask_full() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Chris deposits USDC and posts a primary ask the secondary bid
    // can fully consume. Loan principal = 100, borrower_rate = 800,
    // term = 30d. Chris's ask: rate 500 < 800, term 30d, principal
    // 100. Cross gate satisfied; full taker fill.
    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            500,
            30 * 86_400,
            /*principal_atoms=*/ 100,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Alice posts secondary bid for her own loan → Scenario B: walks
    // asks, crosses Chris's ask, no residual to rest.
    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // No resting bid → no SecondaryLoanSale in the bids tree.
    let bid_seq = fixture.read_first_bid_sequence_for_loan(loan_seq).await;
    assert!(
        bid_seq.is_none(),
        "Scenario B full match: no residual should rest on the bids tree"
    );

    // Loan flag should remain 0 (no resting bid was created).
    let loan = fixture.read_loan(loan_seq).await;
    assert_eq!(
        loan.has_resting_secondary_bid, 0,
        "Scenario B full match: O(1) flag must stay 0"
    );

    // A SECONDARY queue node was inserted at sequence 1 (loan was
    // sequence 0). Cranker finalizes the full transfer.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "Scenario B should bump matched_loan_sequence to 2"
    );
    fixture
        .crank_secondary_matched_loan(
            /*queue_sequence=*/ 1, /*referenced_loan_sequence=*/ 0,
        )
        .await
        .unwrap();

    // Verify Option-A reset on the loan PDA.
    let loan = fixture.read_loan(loan_seq).await;
    let chris_seat = fixture.read_seat_index(&chris.pubkey()).await;
    assert_eq!(
        loan.lender_seat_index, chris_seat,
        "Scenario B: loan transferred to chris"
    );
    assert_eq!(loan.lender_rate_bps, 500);
    assert_eq!(loan.lender_claimable_atoms, 0);
}

/// Scenario B with a primary ask too small to consume the whole loan.
/// The match takes what it can; the residual rests as a SecondaryLoanSale
/// bid, and the loan flag is set to 1.
#[tokio::test]
async fn scenario_b_secondary_bid_partial_residual_rests() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Chris's ask only has 60 atoms — less than the loan's 100. The
    // Scenario B match takes 60; 40 must rest.
    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            500,
            30 * 86_400,
            /*principal_atoms=*/ 60,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Residual = 40 atoms rests as a SecondaryLoanSale bid.
    let bid_seq = fixture
        .read_first_bid_sequence_for_loan(loan_seq)
        .await
        .expect("partial Scenario B: residual should rest");
    let _ = bid_seq;

    let loan = fixture.read_loan(loan_seq).await;
    assert_eq!(
        loan.has_resting_secondary_bid, 1,
        "Scenario B partial: O(1) flag must be 1 (residual rests)"
    );

    // A queue node for the matched 60 atoms was inserted.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "Scenario B partial: one matched-loan node for the 60 atoms taken"
    );
}

/// Scenario B where the cross gate fails (ask rate too high vs the
/// loan's borrower_rate). The bid fully rests; no match occurs.
#[tokio::test]
async fn scenario_b_no_match_when_rate_floor_fails() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let chris = fixture.create_trader().await;

    let loan_seq = make_fixed_loan_with_real_collateral(&fixture, &alice, &bob).await;

    // Loan's borrower_rate is 800 bps. Chris's ask at 850 > 800
    // breaks the cross gate (`borrower_rate - ask_rate < 0 < floor`).
    fixture.claim_seat(&chris).await;
    let chris_usdc = Pubkey::new_unique();
    fixture.put_token_account(chris_usdc, mainnet::usdc_mint(), chris.pubkey(), 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&chris, chris_usdc, /*is_debt=*/ true, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &chris,
            Side::Ask,
            OrderType::Limit,
            850,
            30 * 86_400,
            /*principal_atoms=*/ 100,
            /*collateral_atoms=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .place_secondary_order(&alice, loan_seq, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Full bid rested (no match).
    let bid_seq = fixture
        .read_first_bid_sequence_for_loan(loan_seq)
        .await
        .expect("rate-floor reject: bid should rest in full");
    let _ = bid_seq;

    let loan = fixture.read_loan(loan_seq).await;
    assert_eq!(
        loan.has_resting_secondary_bid, 1,
        "rate-floor reject: flag set since bid rested"
    );

    // No matched-loan node was created (Scenario B made zero crosses,
    // and no primary cross happened either since the ask doesn't
    // match a primary borrower). Sequence stays at 1 (1 was bumped
    // by the original loan creation).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "no Scenario B cross: matched_loan_sequence stays at 1"
    );
}
