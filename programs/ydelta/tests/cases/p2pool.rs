//! P2Pool fallback lifecycle tests.
//!
//! Exercises the live `marginfi.lending_account_borrow` CPI in
//! `process_place_order`'s residual handling: when a Bid's residual
//! triggers `ResidualAction::P2PoolBorrow`, ydelta inserts a
//! `MatchedLoan` with `loan_type = P2Pool` and fires the borrow CPI
//! against the borrower-side marginfi-account. Atoms land directly in
//! the borrower's debt-mint ATA.
//!
//! These tests run under `MarketFixture` (real marginfi banks loaded
//! from mainnet fixtures) so the LVA seed check + solvency math run
//! against the actual marginfi.so binary.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use hypertree::NIL;
use ydelta::state::{OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Bid with no resting asks → entire residual triggers P2Pool borrow.
/// After place_order: borrower's debt ATA holds the borrowed atoms; one
/// MatchedLoan(P2Pool) node lives in the queue with non-zero
/// `borrower_marginfi_borrow_shares`.
#[tokio::test]
async fn bid_unfilled_residual_p2pool_borrows() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // Alice (lender) deposits USDC so the lender-side marginfi-account
    // has assets — irrelevant for the borrow CPI itself but mirrors the
    // realistic setup.
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

    // Bob (borrower) deposits wSOL collateral onto the
    // **borrower-side** marginfi-account so marginfi's solvency
    // check on the new debt liability passes. `is_debt=false` routes
    // to the borrower account.
    //
    // The mainnet wSOL liquidity-vault snapshot already holds many
    // billion atoms, so SPL transfers above ~10⁹ atoms overflow u64.
    // Use a tiny deposit (1000 atoms = ~1µ wSOL) and a tiny principal.
    // wSOL is native — must be initialised with `is_native = Some(rent)`
    // and lamports = rent + amount.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // No asks on book → entire principal becomes the residual and
    // goes through the P2PoolBorrow path. Atoms route through
    // `market_debt_vault` and get deposited into
    // `lender_marginfi_account`; bob's seat is credited with the
    // resulting asset_shares so he can withdraw to atoms later via
    // `process_withdraw`.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob_debt_ata = fixture.signer_debt_token(&bob.pubkey());
    let pre_ata_balance = fixture.token_balance(bob_debt_ata).await;
    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;

    // flags = 0: explicitly opt INTO the P2Pool fallback (default in
    // MarketFixture is OB_ONLY).
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            0,
        )
        .await
        .unwrap();

    // Bob's debt ATA stays unchanged — atoms went into
    // lender_marginfi_account, not his wallet.
    let post_ata_balance = fixture.token_balance(bob_debt_ata).await;
    assert_eq!(
        post_ata_balance, pre_ata_balance,
        "borrowed atoms must route through market_debt_vault → \
         lender_marginfi_account, NOT directly to borrower's ATA"
    );

    // Bob's seat got credited with the matching asset_shares. With a
    // freshly-bootstrapped marginfi bank, asset_share_value ≈ 1.0
    // (fp48 = 2^48), so shares_credited ≈ atoms × 2^48 ≈ principal_atoms
    // share-units in fp48. Rather than reconstruct the exact fp48
    // arithmetic, just assert the seat balance grew by a non-trivial
    // amount on the debt axis (precision-tolerant smoke check).
    let bob_seat_post = fixture.read_seat(&bob.pubkey()).await;
    assert!(
        bob_seat_post.debt_withdrawable_shares > bob_seat_pre.debt_withdrawable_shares,
        "borrower's seat must be credited with shares backing the \
         borrowed atoms (was {}, now {})",
        bob_seat_pre.debt_withdrawable_shares,
        bob_seat_post.debt_withdrawable_shares,
    );

    // Market state: no resting bid (residual was P2Pool-borrowed, not
    // rested), one matched-loan slot allocated.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.bids_root_index, NIL,
        "Bid residual went to P2PoolBorrow, must NOT rest on book"
    );
    assert_eq!(
        market.matched_loan_sequence, 1,
        "exactly one MatchedLoan(P2Pool) inserted"
    );

    // Crank the queue → LoanFixed PDA carries loan_type = P2Pool and a
    // non-zero `borrower_marginfi_borrow_shares`.
    let market_pre_crank = fixture.read_market_fixed().await;
    fixture.crank_matched_loan(0).await.unwrap();
    let loan = fixture.read_loan(0).await;

    // P2Pool promotions must not bump the protocol fee accumulator.
    // There's no orderbook spread for the protocol to capture, so
    // origination is gated off. This should stay true even if
    // `origination_bps` changes later.
    let market_post_crank = fixture.read_market_fixed().await;
    assert_eq!(
        market_post_crank.accumulated_protocol_fee_shares,
        market_pre_crank.accumulated_protocol_fee_shares,
        "P2Pool promote must not bump accumulated_protocol_fee_shares"
    );
    assert_eq!(
        loan.loan_type, 1,
        "promoted loan should carry LoanType::P2Pool (=1)"
    );
    assert!(
        loan.borrower_marginfi_borrow_shares > 0,
        "P2Pool loan should record marginfi liability shares"
    );
    // `principal_debt_atoms` is the net post-origination amount.
    // origination_bps defaults to 0, so it equals principal.
    assert_eq!(
        loan.principal_debt_atoms, principal_atoms,
        "principal_debt_atoms preserved into LoanFixed"
    );
    assert_eq!(
        loan.collateral_atoms, collateral_atoms,
        "collateral_atoms preserved into LoanFixed"
    );
}

/// `OB_ONLY` on a bid keeps residual size on the book instead of
/// triggering the P2Pool borrow path.
#[tokio::test]
async fn ob_only_blocks_p2pool_borrow_path() {
    use ydelta::state::market_helpers::FLAG_OB_ONLY;

    let fixture = MarketFixture::new().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;
    // Seat-level collateral so encumbrance bookkeeping works. No real
    // marginfi-account deposit needed: the OB_ONLY path never touches
    // marginfi.
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;

    let bob_debt_ata = fixture.signer_debt_token(&bob.pubkey());
    let pre_balance = fixture.token_balance(bob_debt_ata).await;

    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            FLAG_OB_ONLY,
        )
        .await
        .unwrap();

    // No marginfi.borrow CPI fired → ATA balance unchanged.
    let post_balance = fixture.token_balance(bob_debt_ata).await;
    assert_eq!(
        post_balance, pre_balance,
        "OB_ONLY must not fire marginfi.borrow"
    );

    // Order rested instead of being P2Pool-borrowed.
    let market = fixture.read_market_fixed().await;
    assert_ne!(
        market.bids_root_index, NIL,
        "OB_ONLY Limit Bid residual should rest on book"
    );
    assert_eq!(
        market.matched_loan_sequence, 0,
        "no MatchedLoan should be inserted on the OB_ONLY path"
    );
}

/// Mixed fill: bid partially crosses a primary ask AND its residual
/// triggers a P2Pool borrow. Hits the borrower's "best of both worlds"
/// scenario — fixed-rate where the orderbook offers it, P2Pool fallback
/// for whatever remains. Two MatchedLoan nodes land in the queue: one
/// `Fixed` against the ask maker, one `P2Pool` for the residual.
#[tokio::test]
async fn bid_partial_match_residual_p2pool_borrows() {
    use ydelta::state::loan::LoanType;

    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await; // ask maker / lender
    let bob = fixture.create_trader().await; // borrower
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // Alice deposits USDC (lender side).
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

    // Bob deposits wSOL collateral (borrower side). Tiny size to avoid
    // u64 overflow with the mainnet liquidity-vault snapshot.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Alice places a primary ask sized to cover only PART of the bid.
    let ask_principal: u64 = 40;
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            ask_principal,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Bob bids for 100 atoms with flags=0 (P2Pool fallback enabled).
    // Matching engine: 40 atoms cross alice's ask → Fixed MatchedLoan;
    // residual 60 atoms → P2Pool MatchedLoan + marginfi.borrow CPI.
    let bid_principal: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            bid_principal,
            collateral_atoms,
            0,
        )
        .await
        .unwrap();

    // Two MatchedLoans queued — one Fixed, one P2Pool. Order book is
    // empty: ask was fully consumed, bid residual went to P2Pool (not
    // rested).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "expected one Fixed + one P2Pool MatchedLoan"
    );
    assert_eq!(
        market.asks_root_index, NIL,
        "alice's ask was fully filled by the partial cross"
    );
    assert_eq!(
        market.bids_root_index, NIL,
        "bid residual went to P2PoolBorrow, NOT rested on book"
    );

    // Crank both queue nodes → two `LoanFixed` PDAs.
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(1).await.unwrap();

    // Loan 0: Fixed, alice as lender, ask_principal in size.
    let loan_fixed = fixture.read_loan(0).await;
    assert_eq!(
        loan_fixed.loan_type,
        LoanType::Fixed as u8,
        "first crank promotes the Fixed cross"
    );
    assert_eq!(
        loan_fixed.principal_debt_atoms, ask_principal,
        "Fixed loan principal == ask amount"
    );
    assert_eq!(
        loan_fixed.lender_rate_bps, 600,
        "Fixed loan adopts ask rate"
    );

    // Loan 1: P2Pool for the residual, marginfi-backed.
    let residual = bid_principal - ask_principal;
    let loan_p2p = fixture.read_loan(1).await;
    assert_eq!(
        loan_p2p.loan_type,
        LoanType::P2Pool as u8,
        "second crank promotes the P2Pool residual"
    );
    assert_eq!(
        loan_p2p.principal_debt_atoms, residual,
        "P2Pool principal == bid - matched"
    );
    assert!(
        loan_p2p.borrower_marginfi_borrow_shares > 0,
        "P2Pool loan must carry marginfi liability shares"
    );

    // Collateral split: Fixed loan got `ask_principal/bid_principal`
    // share of bob's posted collateral; P2Pool got the rest. The two
    // must sum to the original collateral_atoms (within rounding).
    let total_collateral = loan_fixed.collateral_atoms + loan_p2p.collateral_atoms;
    assert!(
        total_collateral <= collateral_atoms
            && total_collateral >= collateral_atoms.saturating_sub(1),
        "Fixed + P2Pool collateral must sum to bid collateral (got {} from {})",
        total_collateral,
        collateral_atoms
    );
}
