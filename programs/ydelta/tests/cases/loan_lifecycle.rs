//! End-to-end smoke + multi-loan isolation for the quote-only model.
//!
//! `lifecycle_smoke` runs the entire happy path top-to-bottom in one
//! test (create_vault → fund profile → rest ask → borrower bid →
//! crank → repay → claim) and asserts that the vault profile recovers
//! its principal (plus realised lender interest) and the loan PDA is
//! closed.
//!
//! `two_independent_loans` exercises cross-loan isolation: one funded
//! vault profile rests a single unbounded ask, two borrowers each
//! cross it into a distinct Fixed loan, both cranked + repaid +
//! claimed independently. Tests that the matched-loan tree, the Loan
//! PDAs, and the seat bookkeeping all stay disjoint per loan.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::state::loan::LoanState;

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn lifecycle_smoke_create_match_crank_repay_claim() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await;

    // Lender side: a vault profile funded with 10 USDC, resting an
    // unbounded ask at 600 bps / 30d.
    let lender_deposit_atoms: u64 = 10_000_000;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            lender_deposit_atoms,
        )
        .await;

    fixture.claim_seat(&bob).await;
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
    // Borrower IOC bid crosses the vault ask.
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .crank_matched_loan_for_sub_vault(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Conservation must hold at promote-time.
    fixture.assert_loan_conservation_holds(0).await;
    // Vault-idle invariant must hold post-promote.
    fixture.assert_vault_idle_invariant(1).await;

    // Borrower repays full. Per the repay/claim split this closes the
    // loan PDA in-place and applies all per-loan sub-vault decrements;
    // claim is now a pure seat→vault sweep with no time gate.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    // Conservation is now a no-op on closed loans (atoms migrated to seat).
    fixture.assert_loan_conservation_holds(0).await;
    fixture.refresh_blockhash().await;

    let profile_pre_claim = fixture.read_sub_vault(1).await;
    let pre_drift =
        (profile_pre_claim.total_principal_atoms as i128 - lender_deposit_atoms as i128).abs();
    assert!(
        pre_drift <= 4,
        "profile total_principal {} drifted > 4 atoms from the deposit {}",
        profile_pre_claim.total_principal_atoms,
        lender_deposit_atoms,
    );

    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, 1)
        .await
        .unwrap();

    // Vault profile recovered its principal plus realised lender
    // interest, and the active-loan tracking zeroed out.
    let profile_post = fixture.read_sub_vault(1).await;
    assert!(
        profile_post.total_principal_atoms + 2 >= lender_deposit_atoms,
        "profile total_principal {} fell below initial deposit {} after lifecycle",
        profile_post.total_principal_atoms,
        lender_deposit_atoms,
    );
    assert_eq!(
        profile_post.deployed_principal_atoms, 0,
        "all loans repaid → deployed_principal must be 0",
    );
    assert_eq!(
        profile_post.encumbered_in_orders_atoms, 0,
        "encumbered_in_orders must zero out after the loan is claimed",
    );
    // Vault-idle invariant post-claim.
    fixture.assert_vault_idle_invariant(1).await;

    // Loan PDA is gone.
    let (loan_addr, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), 0);
    let post_account = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(loan_addr).await.unwrap()
    };
    assert!(post_account.is_none(), "loan PDA should be collected");

    // Borrower seat at zero open positions.
    let bob_seat = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(bob_seat.open_borrow_count, 0);
}

#[tokio::test]
async fn two_independent_loans_remain_disjoint() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let carol = fixture.create_trader().await;

    // One vault profile funds both loans (unbounded ask, 20 USDC idle).
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 20_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    fixture.claim_seat(&carol).await;
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    let carol_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        carol_usdc,
        mainnet::usdc_mint(),
        carol.pubkey(),
        100_000_000,
    );
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture
        .seed_seat_shares(&carol.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    let principal: u64 = 1_000_000;
    let coll: u64 = 100_000_000;

    // Loan 0: bob's IOC bid crosses the vault ask.
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal,
            coll,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Loan 1: carol's IOC bid crosses the (still-resting) vault ask.
    // Advance the slot so the second bid gets a distinct blockhash.
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
        .place_order_with_flags(
            &carol,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal,
            coll,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Two queued matches.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 2);

    // Crank both.
    fixture
        .crank_matched_loan_for_sub_vault(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .crank_matched_loan_for_sub_vault(1)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Both loans exist, are Active, with distinct borrowers.
    let loan0 = fixture.read_loan(0).await;
    let loan1 = fixture.read_loan(1).await;
    assert_eq!(loan0.state, LoanState::Active as u8);
    assert_eq!(loan1.state, LoanState::Active as u8);
    assert_ne!(
        loan0.borrower_seat_index, loan1.borrower_seat_index,
        "borrower seat indices must differ between independent loans"
    );
    // Conservation must hold on both independent loans.
    fixture.assert_loan_conservation_holds(0).await;
    fixture.assert_loan_conservation_holds(1).await;
    // Both loans share the same lender (vault profile 0); their
    // principals must sum to the profile's deployed_principal_atoms.
    let profile_mid = fixture.read_sub_vault(1).await;
    assert_eq!(
        profile_mid.deployed_principal_atoms,
        loan0.principal_debt_atoms + loan1.principal_debt_atoms,
        "deployed_principal_atoms must equal Σ active loans' principals"
    );
    // Vault-idle invariant must hold mid-lifecycle.
    fixture.assert_vault_idle_invariant(1).await;

    // Bob repays only loan 0, claims loan 0 → loan 1 untouched.
    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let max_matures = loan0.matures_at_unix.max(loan1.matures_at_unix);
    fixture.set_clock_unix_timestamp(max_matures + 1).await;
    fixture.refresh_blockhash().await;
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, 1)
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

    // Carol repays + the loan-1 claim runs independently.
    fixture
        .repay(&carol, 1, carol_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        // Under the repay/claim split, both loans share the vault's
        // single sub-vault seat (sub_vault_id=0). The new claim is a
        // seat sweep, not per-loan; the previous-call form
        // .claim_repayment_for_sub_vault(&stranger, 1, …) is now
        // expressed as a second sweep on the same profile.
        .claim_repayment_for_sub_vault(&stranger, 1)
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

    // Profile fully settled.
    let profile_final = fixture.read_sub_vault(1).await;
    assert_eq!(profile_final.deployed_principal_atoms, 0);
}
