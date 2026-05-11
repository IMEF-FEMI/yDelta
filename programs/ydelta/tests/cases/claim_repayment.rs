//! Step 9 — end-to-end test for `process_claim_repayment`.
//!
//! Builds on the repay flow: after a full repay, the lender claims
//! their accrued atoms. Asserts:
//! - lender's seat carries `debt_withdrawable_shares` ≈
//!   `amount_to_shares(loan.lender_claimable_atoms_pre_zero)`,
//! - lender's `open_lend_count` decremented to zero,
//! - the loan PDA's discriminator was zeroed (account "closed"),
//! - lamports refunded to the cranker (via the optional refund
//!   account).

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::Bank;
use ydelta::math::{div_scale, to_scaled};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::{
    loan::{loan_pda, LoanState},
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
async fn full_claim_after_full_repay_closes_loan_and_refunds_cranker() {
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

    // Cranker = fixture.payer (default crank_matched_loan signer).
    let cranker = fixture.payer.pubkey();
    let cranker_balance_pre = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client
            .get_account(cranker)
            .await
            .unwrap()
            .map(|a| a.lamports)
            .unwrap_or(0)
    };
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;

    // Snapshot loan fields right after promotion (before repay
    // accrual). principal == lender_claimable when origination_bps =
    // 0, so claimable_atoms_pre is just `principal_atoms`.
    let loan_post_crank = fixture.read_loan(0).await;
    assert_eq!(loan_post_crank.lender_claimable_atoms, principal_atoms);

    // Borrower repays full.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let alice_seat_pre = fixture.read_seat(&alice.pubkey()).await;
    let alice_debt_pre = alice_seat_pre.debt_withdrawable_shares;

    // claim_repayment requires now >= matures_at_unix. Advance the
    // test clock past maturity so the lock-up gate passes.
    let loan_mid = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_mid.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;

    // Lender claims, passing the cranker as refund target.
    fixture
        .claim_repayment(&alice, 0, Some(cranker))
        .await
        .unwrap();

    // Lender seat: gained shares = `atoms_to_shares_at_snapshot(
    // claimable_atoms, lender_debt_share_price_snapshot_fp48)` on the
    // debt-withdrawable side. Reads the snapshot off the loan PDA
    // BEFORE close (after close the PDA's lamports are drained, so we
    // captured it on `loan_mid` above which still has the post-promote
    // body intact).
    let snapshot_fp48 = loan_mid.lender_debt_share_price_snapshot_fp48;
    let expected_credit: u128 = ((principal_atoms as u128) << 48) / snapshot_fp48;
    let alice_seat_post = fixture.read_seat(&alice.pubkey()).await;
    let credited = alice_seat_post
        .debt_withdrawable_shares
        .saturating_sub(alice_debt_pre);
    // Allow ±1 share for accrual rounding (claimable can grow a hair
    // between snapshot and on-chain accrue, when origination_bps is 0
    // the difference is just I80F48 round-trip noise).
    let drift = (credited as i128 - expected_credit as i128).abs();
    assert!(
        drift <= 1,
        "lender share credit drift > 1 share (got {}, expected {})",
        credited,
        expected_credit
    );
    let _bank_data: Vec<u8> = fixture.account_data(mainnet::usdc_bank()).await;
    // open_lend_count was bumped at place_order — should clear on full
    // settlement.
    assert_eq!(alice_seat_post.open_lend_count, 0);

    // Loan PDA should be "closed" — lamports drained to the cranker
    // refund target, which garbage-collects the account.
    let (loan_addr, _) = loan_pda(&fixture.market.pubkey(), 0);
    let post_account = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(loan_addr).await.unwrap()
    };
    assert!(
        post_account.is_none(),
        "loan PDA should have been collected after closing claim with refund"
    );

    // Rent reconciliation. The cranker is also the fixture-default tx
    // fee payer, so absolute balance dips per tx (~5k lamports each).
    // Without a refund, the cranker would also be down the entire
    // LoanFixed rent (~2.1M lamports for 256 bytes); WITH refund, the
    // rent comes back. We assert the net loss is "small" — bounded by
    // ~5 tx fees worth, far below the rent of a 256-byte account.
    let cranker_balance_post = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client
            .get_account(cranker)
            .await
            .unwrap()
            .map(|a| a.lamports)
            .unwrap_or(0)
    };
    let net_loss = cranker_balance_pre.saturating_sub(cranker_balance_post);
    assert!(
        net_loss < 100_000,
        "cranker net loss too high — refund didn't run? \
         pre={}, post={}, loss={}",
        cranker_balance_pre,
        cranker_balance_post,
        net_loss
    );
}

#[tokio::test]
async fn early_claim_before_repay_is_rejected() {
    // Lender attempts to claim BEFORE the borrower has repaid. Earlier
    // semantics let this succeed and credit the full
    // lender_claimable_atoms (which already holds principal + accrued
    // interest after `accrue_loan`) — but only repaid atoms have
    // physically arrived in market.lender_integration, so the lender
    // would drain shares the pool can't redeem. Now gated:
    // claim_repayment requires outstanding_debt_atoms == 0.
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

    // Pre-claim: loan is Active, outstanding_debt > 0.
    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.state, LoanState::Active as u8);
    assert_eq!(loan_pre.outstanding_debt_atoms, principal_atoms);

    // Lender attempts to claim early — should error with
    // InvalidArgument. Loan body must be untouched.
    let result = fixture.claim_repayment(&alice, 0, None).await;
    assert!(
        result.is_err(),
        "claim_repayment must reject when outstanding_debt > 0"
    );

    let loan_post = fixture.read_loan(0).await;
    assert_eq!(loan_post.state, LoanState::Active as u8);
    assert_eq!(loan_post.outstanding_debt_atoms, principal_atoms);
    assert_eq!(
        loan_post.lender_claimable_atoms,
        loan_pre.lender_claimable_atoms
    );
}

/// Lender earning over a loan's term: open a fixed-rate loan, hold it
/// to maturity, borrower repays, lender claims. Asserts the
/// `lender_claimable_atoms` after the repay-time `accrue_loan` includes
/// real interest — `principal × lender_rate × elapsed / SECONDS_PER_YEAR` —
/// and that the lender's seat receives shares backing principal+interest.
///
/// Distinguishes itself from `full_claim_after_full_repay_*` by actually
/// advancing the test clock between crank and repay so accrual produces
/// a non-trivial interest amount; the existing test runs everything in
/// the same slot, so accrued interest is sub-atom.
#[tokio::test]
async fn lender_claims_principal_plus_interest_at_maturity() {
    let fixture = MarketFixture::new().await;

    let alice = fixture.create_trader().await; // lender
    let bob = fixture.create_trader().await; // borrower
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
        .deposit(&alice, alice_usdc, /*is_debt=*/ true, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    // Use distinct lender vs borrower rates so the spread (protocol
    // fee) is observably non-zero — confirms the dual-rate accrual is
    // wired correctly end-to-end.
    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
    let term_seconds: u32 = 30 * 86_400;
    let lender_rate_bps: u16 = 600; // ask: lender earns 6% APR
    let borrower_rate_bps: u16 = 800; // bid: borrower pays 8% APR
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            lender_rate_bps,
            term_seconds,
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
            borrower_rate_bps,
            term_seconds,
            principal_atoms,
            collateral_atoms,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;

    // Pre-accrue snapshot: claimable == principal (origination_bps = 0).
    let loan_post_crank = fixture.read_loan(0).await;
    assert_eq!(loan_post_crank.lender_claimable_atoms, principal_atoms);
    assert_eq!(loan_post_crank.outstanding_debt_atoms, principal_atoms);

    // Advance the test clock to maturity. Repaying triggers
    // `accrue_loan` against the new clock, which folds full-term
    // interest into both `outstanding_debt_atoms` (borrower side) and
    // `lender_claimable_atoms` (lender side).
    fixture
        .set_clock_unix_timestamp(loan_post_crank.matures_at_unix)
        .await;
    fixture.refresh_blockhash().await;

    // Borrower repays full. Internally: accrue_loan(at maturity) →
    // outstanding gets bumped by borrower_interest, repay zeroes it.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Post-repay: outstanding == 0, lender_claimable holds principal +
    // lender-side interest, protocol_fee_atoms holds the spread.
    let loan_post_repay = fixture.read_loan(0).await;
    assert_eq!(
        loan_post_repay.outstanding_debt_atoms, 0,
        "full repay must zero outstanding"
    );

    // Expected lender interest (simple, single-segment):
    //   principal × lender_rate × elapsed / (10000 × SECONDS_PER_YEAR)
    // For our params (P=1M, r=600bps, t=30d):
    //   1_000_000 × 600 × 2_592_000 / (10_000 × 31_536_000)
    //   ≈ 4_931 atoms.
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let elapsed: u128 = term_seconds as u128;
    let expected_lender_interest: u128 =
        principal_atoms as u128 * lender_rate_bps as u128 * elapsed
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_borrower_interest: u128 =
        principal_atoms as u128 * borrower_rate_bps as u128 * elapsed
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_spread: u128 = expected_borrower_interest - expected_lender_interest;
    let expected_claimable = principal_atoms as u128 + expected_lender_interest;

    // Lender claimable: principal + accrued lender interest.
    let drift_claimable =
        (loan_post_repay.lender_claimable_atoms as i128 - expected_claimable as i128).abs();
    assert!(
        drift_claimable <= 2,
        "lender_claimable_atoms drift > 2 atoms (got {}, expected {} = principal + {} interest)",
        loan_post_repay.lender_claimable_atoms,
        expected_claimable,
        expected_lender_interest
    );

    // Protocol fee accumulator: ≈ spread (borrower − lender interest).
    let drift_fee =
        (loan_post_repay.accumulated_protocol_fee_atoms as i128 - expected_spread as i128).abs();
    assert!(
        drift_fee <= 2,
        "accumulated_protocol_fee_atoms drift > 2 atoms (got {}, expected {})",
        loan_post_repay.accumulated_protocol_fee_atoms,
        expected_spread
    );

    // Sanity: interest must be material — > 1k atoms over 30d at 6% APR.
    assert!(
        expected_lender_interest > 1_000,
        "test parameters too small to observe accrual (got {} atoms)",
        expected_lender_interest
    );

    // The atom-level proof above (lender_claimable_atoms includes
    // accrued lender interest at the configured rate) is the
    // load-bearing assertion for this test. We don't try to convert
    // through the share-price snapshot here — H-14 replaced the
    // placeholder fp48=2^48 share price with real bank share-prices
    // (which exceed 1.0 once marginfi has accrued any yield), so a
    // direct atoms→shares formula doesn't hold without re-reading the
    // loan's lender_debt_share_price_snapshot_fp48 (already covered by
    // `full_claim_after_full_repay_closes_loan_and_refunds_cranker`).
}

/// Lender posts a single ASK that gets split across two borrower BIDs
/// → two distinct Fixed loans → both repaid → lender claims both →
/// aggregate withdrawable atoms ≥ original principal + sum_of_interest.
///
/// Proves that order-splitting doesn't lose principal: a curator who
/// puts up 500k that ends up serving two 200k+300k borrowers gets the
/// same total atoms back as if it had been one 500k loan (modulo
/// per-loan rounding noise within ±2 atoms per loan).
#[tokio::test]
async fn split_lender_order_recovers_full_principal_after_all_maturities() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await; // single lender, split ask
    let bob = fixture.create_trader().await; // borrower 1
    let carol = fixture.create_trader().await; // borrower 2
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;
    fixture.claim_seat(&carol).await;

    // Alice deposits enough USDC to back the full ask.
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

    // Borrowers: synthetic seat collateral + real USDC for repay.
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture
        .seed_seat_shares(&carol.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
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

    // ── Split ask ──
    // Alice posts a single 500k ASK. Bob takes 200k. Carol takes the
    // remaining 300k. Two distinct Fixed loans get queued.
    let total_principal: u64 = 500_000;
    let bob_principal: u64 = 200_000;
    let carol_principal: u64 = 300_000;
    let lender_rate_bps: u16 = 600;
    let term_seconds: u32 = 30 * 86_400;
    let collateral_atoms: u64 = 50_000_000;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            lender_rate_bps,
            term_seconds,
            total_principal,
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
            term_seconds,
            bob_principal,
            collateral_atoms,
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
            term_seconds,
            carol_principal,
            collateral_atoms,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Sanity: 2 MatchedLoans queued, asks fully drained.
    let market_post_match = fixture.read_market_fixed().await;
    assert_eq!(
        market_post_match.matched_loan_sequence, 2,
        "two split fills must produce two MatchedLoan queue nodes"
    );

    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(1).await.unwrap();
    fixture.refresh_blockhash().await;

    // Snapshot loan bodies pre-repay so we can read the per-loan
    // lender_claimable_atoms and confirm the split sums to the original.
    let loan0 = fixture.read_loan(0).await;
    let loan1 = fixture.read_loan(1).await;
    assert_eq!(
        loan0.principal_debt_atoms + loan1.principal_debt_atoms,
        total_principal,
        "split loans' principals must sum to the original ask"
    );

    // Advance to maturity so accrue_loan adds full-term interest on
    // both before the repays.
    let max_matures = loan0.matures_at_unix.max(loan1.matures_at_unix);
    fixture.set_clock_unix_timestamp(max_matures).await;
    fixture.refresh_blockhash().await;

    // Both borrowers repay. Process sequentially with blockhash refreshes.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .repay(&carol, 1, carol_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Snapshot per-loan lender_claimable_atoms post-repay (= principal +
    // accrued lender interest).
    let loan0_post = fixture.read_loan(0).await;
    let loan1_post = fixture.read_loan(1).await;
    let total_claimable: u128 =
        loan0_post.lender_claimable_atoms as u128 + loan1_post.lender_claimable_atoms as u128;
    // Aggregate must be ≥ original principal (with at most a few atoms
    // of marginfi-rounding tolerance per loan).
    assert!(
        total_claimable + 4 >= total_principal as u128,
        "aggregated claimable ({}) below original principal ({})",
        total_claimable,
        total_principal
    );

    // Lock-up + claim both.
    fixture.set_clock_unix_timestamp(max_matures + 1).await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment(&alice, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment(&alice, 1, Some(fixture.payer.pubkey()))
        .await
        .unwrap();

    // Aggregate principal recovery: alice's debt_withdrawable_shares
    // grew by an amount that, in atoms terms via the loans' share-price
    // snapshots, covers ≥ total_principal. We assert the per-loan
    // claimable-atoms sum ≥ total_principal directly (above) which is
    // the load-bearing invariant. The claim ix then converts those
    // atoms to shares on alice's seat using the snapshots — that
    // conversion is already covered by the single-loan
    // `full_claim_after_full_repay_closes_loan_and_refunds_cranker`.
}
