//! Step 8 — end-to-end test for `process_repay`.
//!
//! Flow (mirrors process_matched_loan setup, then repays):
//! 1. Alice (lender) deposits USDC.
//! 2. Bob (borrower)'s collateral seat is seeded directly to skip the
//!    wSOL-deposit overflow; market state is fine.
//! 3. Alice asks; Bob bids matching principal; loan promoted by cranker.
//! 4. Alice ALSO needs USDC to repay (lender's share of the position),
//!    but actually the BORROWER repays. Bob has the principal credit
//!    from cranker, but to repay he needs USDC in his wallet (the loan
//!    paid him atoms, not shares — since he's repaying through marginfi
//!    deposit, he needs SPL atoms). Pre-mint USDC into Bob's ATA.
//! 5. Bob repays full. Assert loan.state == Repaid,
//!    outstanding_debt_atoms == 0, Bob's collateral_withdrawable_shares
//!    grew by amount_to_shares(loan.collateral_atoms), the loan PDA
//!    still exists (lender's claim hasn't run).

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::Bank;
use ydelta::math::{div_scale, to_scaled};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::{
    loan::{LoanState, LoanType},
    OrderType, Side,
};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[allow(dead_code)]
fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

#[tokio::test]
async fn full_repay_marks_loan_repaid_and_credits_collateral_back() {
    let fixture = MarketFixture::new().await;

    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // Alice's USDC funding for the lender side.
    let alice_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        alice_usdc,
        mainnet::usdc_mint(),
        alice.pubkey(),
        100_000_000,
    );
    // Bob's USDC funding for the eventual repay (post-match he'll owe USDC).
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    fixture.refresh_blockhash().await;

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

    // Bob's collateral seat seeded directly (skip wSOL bank overflow).
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    // ─── Match: Alice asks 1M @ 600bps / 30d, Bob bids 1M @ 800bps / 30d.
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

    // Sanity: loan exists, is Active, with the right principal.
    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.state, LoanState::Active as u8);
    assert_eq!(loan_pre.loan_type, LoanType::Fixed as u8);
    assert_eq!(loan_pre.principal_debt_atoms, principal_atoms);
    assert_eq!(loan_pre.outstanding_debt_atoms, principal_atoms);
    assert_eq!(loan_pre.collateral_atoms, collateral_atoms);

    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;
    let bob_coll_pre = bob_seat_pre.collateral_withdrawable_shares;

    // ─── Repay full.
    fixture
        .repay(
            &bob, 0, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true,
        )
        .await
        .unwrap();

    // Loan transitioned to Repaid.
    let loan_post = fixture.read_loan(0).await;
    assert_eq!(loan_post.state, LoanState::Repaid as u8);
    assert_eq!(loan_post.outstanding_debt_atoms, 0);
    // Lender's claimable not zeroed yet — that's `process_claim_repayment`.
    assert!(loan_post.lender_claimable_atoms > 0);

    // Bob's seat: collateral credited back at the loan's place-time
    // snapshot, not the live bank value (byte-symmetric with the
    // place_order encumber).
    let snapshot_fp48 = loan_post.borrower_collateral_share_price_snapshot_fp48;
    let expected_coll_shares: u128 = ((collateral_atoms as u128) << 48) / snapshot_fp48;
    let bob_seat_post = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        bob_seat_post.collateral_withdrawable_shares - bob_coll_pre,
        expected_coll_shares,
        "borrower seat should grow by atoms_to_shares_at_snapshot(collateral, snapshot)"
    );
    let _coll_bank_data: Vec<u8> = fixture.account_data(mainnet::sol_bank()).await;
    // open_borrow_count decremented (was 1 from the matched bid).
    assert_eq!(bob_seat_post.open_borrow_count, 0);
}

#[tokio::test]
async fn partial_repay_leaves_loan_active_and_collateral_locked() {
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

    fixture
        .deposit(&alice, alice_usdc, true, 10_000_000)
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

    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;
    let bob_coll_pre = bob_seat_pre.collateral_withdrawable_shares;

    // Repay half.
    let half: u64 = principal_atoms / 2;
    fixture
        .repay(&bob, 0, bob_usdc, half, /*full_repay=*/ false)
        .await
        .unwrap();

    // Loan stays Active; outstanding decremented exactly by `half`.
    let loan_post = fixture.read_loan(0).await;
    assert_eq!(loan_post.state, LoanState::Active as u8);
    assert_eq!(loan_post.outstanding_debt_atoms, principal_atoms - half);

    // Borrower's collateral should NOT have been credited back yet —
    // partial repay does not release collateral.
    let bob_seat_post = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        bob_seat_post.collateral_withdrawable_shares, bob_coll_pre,
        "collateral must remain locked until full repay"
    );
    assert_eq!(
        bob_seat_post.open_borrow_count, bob_seat_pre.open_borrow_count,
        "open_borrow_count untouched on partial repay"
    );
}
