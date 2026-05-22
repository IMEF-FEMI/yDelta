//! End-to-end test for `process_repay` in the quote-only model.
//!
//! Flow:
//! 1. A vault risk profile is funded and rests an unbounded ask
//!    (`provide_vault_liquidity`).
//! 2. The borrower's collateral seat is seeded directly to skip the
//!    wSOL-deposit overflow; market state is fine.
//! 3. The borrower places an IOC bid that crosses the vault ask; the
//!    cranker promotes the matched loan.
//! 4. The borrower repays (needs SPL USDC atoms in their wallet).
//! 5. Asserts loan.state == Repaid, outstanding_debt_atoms == 0, the
//!    borrower's collateral_withdrawable_shares grew by
//!    amount_to_shares(loan.collateral_atoms), the loan PDA still
//!    exists (the lender's claim hasn't run).

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::Bank;
use ydelta::math::{div_scale, to_scaled};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::loan::{LoanState, LoanType};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[allow(dead_code)]
fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

const PRINCIPAL_ATOMS: u64 = 1_000_000;
const COLLATERAL_ATOMS: u64 = 100_000_000;
const TERM_SECONDS: u32 = 30 * 86_400;

/// Stand up a funded vault profile (lender side) + a borrower whose
/// IOC bid has crossed the vault ask into a promoted Fixed loan.
/// Returns `(borrower, borrower_usdc)`.
async fn match_one_loan(fixture: &MarketFixture) -> (solana_sdk::signature::Keypair, Pubkey) {
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    // Lender side: vault profile rests an unbounded ask at 600bps/30d.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            TERM_SECONDS,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&borrower).await;
    // Borrower's collateral seat seeded directly (skip wSOL bank
    // overflow). Borrower's USDC ATA seeded for the eventual repay.
    fixture
        .seed_seat_shares(&borrower.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    let borrower_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        borrower_usdc,
        mainnet::usdc_mint(),
        borrower.pubkey(),
        100_000_000,
    );
    fixture.refresh_blockhash().await;

    // Borrower IOC bid @ 800bps crosses the vault ask.
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            TERM_SECONDS,
            PRINCIPAL_ATOMS,
            COLLATERAL_ATOMS,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Vault-funded loan → promote via the risk-profile cranker.
    fixture
        .crank_matched_loan_for_risk_profile(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    (borrower, borrower_usdc)
}

#[tokio::test]
async fn full_repay_marks_loan_repaid_and_credits_collateral_back() {
    let fixture = MarketFixture::new().await;
    let (bob, bob_usdc) = match_one_loan(&fixture).await;

    // Sanity: loan exists, is Active, with the right principal.
    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.state, LoanState::Active as u8);
    assert_eq!(loan_pre.loan_type, LoanType::Fixed as u8);
    assert_eq!(loan_pre.principal_debt_atoms, PRINCIPAL_ATOMS);
    assert_eq!(loan_pre.outstanding_debt_atoms, PRINCIPAL_ATOMS);
    assert_eq!(loan_pre.collateral_atoms, COLLATERAL_ATOMS);
    // Rate matching: lender_rate == ask_rate (600), borrower_rate ==
    // max(bid_rate, ask_rate + protocol_fee_bps_floor). The floor
    // defaults to 0 on a fresh market, so borrower_rate == 800.
    assert_eq!(loan_pre.lender_rate_bps, 600);
    assert_eq!(loan_pre.borrower_rate_bps, 800);
    // Conservation holds at promotion.
    fixture.assert_loan_conservation_holds(0).await;

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
    // Lender's claimable not zeroed yet — that's claim_repayment_for_risk_profile.
    assert!(loan_post.lender_claimable_atoms > 0);
    // Conservation identity must still hold after the full repay event.
    fixture.assert_loan_conservation_holds(0).await;
    // Full repay retires at least the gross principal — `principal_retired_atoms`
    // tracks cumulative atoms repaid (principal + accrued interest).
    assert!(
        loan_post.principal_retired_atoms >= loan_post.principal_debt_atoms,
        "full repay must retire >= gross principal (got retired={}, principal={})",
        loan_post.principal_retired_atoms, loan_post.principal_debt_atoms,
    );

    // Bob's seat: collateral credited back at the loan's place-time
    // snapshot, not the live bank value (byte-symmetric with the
    // place_order encumber).
    let snapshot_fp48 = loan_post.borrower_collateral_share_price_snapshot_fp48;
    let expected_coll_shares: u128 = ((COLLATERAL_ATOMS as u128) << 48) / snapshot_fp48;
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

/// Legacy-shape safety: a Fixed loan whose collateral sits in the seat's
/// `withdrawable` bucket with `encumbered == 0` — exactly how loans opened
/// before the match-time collateral encumber existed look on-chain — must
/// still repay cleanly. The close-time `release_loan_collateral` is
/// min-capped, so here it is a no-op: the collateral is NEITHER
/// double-credited NOR stranded, and the loan still closes. This is the
/// property the 6pGq grandfather (mainnet legacy loans) relied on.
#[tokio::test]
async fn legacy_collateral_in_withdrawable_repays_without_double_credit() {
    let fixture = MarketFixture::new().await;
    let (bob, bob_usdc) = match_one_loan(&fixture).await;

    // Forge the legacy shape: move the loan's encumbered collateral into
    // withdrawable (encumbered → 0), as if it had never been encumbered.
    fixture.legacy_collateral_to_withdrawable(&bob.pubkey()).await;
    let seat_pre = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        seat_pre.collateral_encumbered_shares, 0,
        "legacy shape: nothing encumbered"
    );
    let withdrawable_pre = seat_pre.collateral_withdrawable_shares;
    assert!(withdrawable_pre > 0, "legacy collateral sits in withdrawable");
    assert_eq!(seat_pre.open_borrow_count, 1, "loan still open pre-repay");

    // Repay full under the new code.
    fixture
        .repay(&bob, 0, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true)
        .await
        .unwrap();

    let loan_post = fixture.read_loan(0).await;
    assert_eq!(
        loan_post.state,
        LoanState::Repaid as u8,
        "loan must close despite the legacy collateral shape"
    );
    assert_eq!(loan_post.outstanding_debt_atoms, 0);

    let seat_post = fixture.read_seat(&bob.pubkey()).await;
    // No double-credit: withdrawable is UNCHANGED — the min-capped release
    // moved nothing because encumbered was 0 (collateral already there).
    assert_eq!(
        seat_post.collateral_withdrawable_shares, withdrawable_pre,
        "legacy repay must NOT double-credit collateral (release no-ops when encumbered==0)"
    );
    // Not stranded, and the open-loan counter still winds down.
    assert_eq!(seat_post.collateral_encumbered_shares, 0);
    assert_eq!(seat_post.open_borrow_count, 0);
}

#[tokio::test]
async fn partial_repay_leaves_loan_active_and_collateral_locked() {
    let fixture = MarketFixture::new().await;
    let (bob, bob_usdc) = match_one_loan(&fixture).await;

    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;
    let bob_coll_pre = bob_seat_pre.collateral_withdrawable_shares;

    // Repay half.
    let half: u64 = PRINCIPAL_ATOMS / 2;
    fixture
        .repay(&bob, 0, bob_usdc, half, /*full_repay=*/ false)
        .await
        .unwrap();

    // Loan stays Active; outstanding decremented exactly by `half`.
    let loan_post = fixture.read_loan(0).await;
    assert_eq!(loan_post.state, LoanState::Active as u8);
    assert_eq!(loan_post.outstanding_debt_atoms, PRINCIPAL_ATOMS - half);
    // Conservation identity must hold across the partial repay event.
    fixture.assert_loan_conservation_holds(0).await;
    // Partial repay retires exactly `half` atoms of principal.
    assert_eq!(
        loan_post.principal_retired_atoms, half,
        "partial repay must retire exactly the repaid atoms (got retired={}, expected={})",
        loan_post.principal_retired_atoms, half,
    );

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
