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
use marginfi_mocks::state::MarginfiAccount;
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::loan::{LoanState, LoanType};
use ydelta::state::market_helpers::FLAG_OB_ONLY;
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
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;
    // (Quote-only: no lender seat debt-deposit — the P2Pool borrow's
    // deposit-back CPI inits the lender-side marginfi balance slot.)

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
        market.asks_root_index, NIL,
        "borrower bid is IOC-only — nothing rests on the book"
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

    // OB_ONLY residual was dropped — not P2Pool-borrowed and not
    // rested (a borrower bid is IOC-only).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.asks_root_index, NIL,
        "borrower bid is IOC-only — nothing rests on the book"
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
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await; // borrower

    // Lender side: a vault profile rests an unbounded ask, funded with
    // `deposit_atoms` (40 atoms) idle. The matching engine reserves
    // `MARGINFI_ROUNDING_RESERVE_ATOMS = 1` per profile to absorb
    // marginfi v0.1.8's deposit-share-mint rounding tax — see the
    // doc-comment on that constant. The deposit itself also floors one
    // atom on the share mint, so the cross caps at
    // `deposit_atoms - 1 (deposit floor) - reserve` and the residual
    // rolls to P2Pool.
    use ydelta::state::market_helpers::MARGINFI_ROUNDING_RESERVE_ATOMS;
    let deposit_atoms: u64 = 40;
    let expected_fixed_principal: u64 = deposit_atoms - 1 - MARGINFI_ROUNDING_RESERVE_ATOMS;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            deposit_atoms,
        )
        .await;

    fixture.claim_seat(&bob).await;
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

    // Bob bids for 100 atoms with flags=0 (P2Pool fallback enabled).
    // Matching engine: `expected_fixed_principal` atoms cross the vault
    // ask (idle-capped by the marginfi-rounding reserve) → Fixed
    // MatchedLoan; the remaining `bid - expected_fixed_principal` atoms
    // → P2Pool MatchedLoan + marginfi.borrow CPI.
    let bid_principal: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            bid_principal,
            collateral_atoms,
            0,
        )
        .await
        .unwrap();

    // Two MatchedLoans queued — one Fixed, one P2Pool. The vault ask
    // persists on the book (the matching engine never removes a
    // risk-profile ask — it's unbounded and only the curator removes
    // it); the bid residual went to P2Pool (not rested).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "expected one Fixed + one P2Pool MatchedLoan"
    );
    assert_ne!(
        market.asks_root_index, NIL,
        "the vault risk-profile ask must persist after the cross",
    );

    // Crank both queue nodes → two `LoanFixed` PDAs. The Fixed cross
    // has a vault-profile lender → risk-profile cranker; the P2Pool
    // residual has no vault lender → plain cranker.
    fixture
        .crank_matched_loan_for_risk_profile(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(1).await.unwrap();

    // Loan 0: Fixed, vault profile as lender, sized to the vault's
    // matchable idle (`deposit - MARGINFI_ROUNDING_RESERVE_ATOMS`).
    let loan_fixed = fixture.read_loan(0).await;
    assert_eq!(
        loan_fixed.loan_type,
        LoanType::Fixed as u8,
        "first crank promotes the Fixed cross"
    );
    assert_eq!(
        loan_fixed.principal_debt_atoms, expected_fixed_principal,
        "Fixed loan principal == vault idle minus marginfi rounding reserve"
    );
    assert_eq!(
        loan_fixed.lender_rate_bps, 600,
        "Fixed loan adopts ask rate"
    );

    // Loan 1: P2Pool for the residual, marginfi-backed.
    let residual = bid_principal - expected_fixed_principal;
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

    // Collateral split: Fixed loan got `expected_fixed_principal/bid_principal`
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

/// Read the borrower marginfi-account's live USDC liability shares.
async fn p2pool_liability_shares(fixture: &MarketFixture) -> u128 {
    let data = fixture
        .account_data(fixture.borrower_marginfi_account_pubkey())
        .await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    mfi.find_balance(&mainnet::usdc_bank())
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares))
        .unwrap_or(0)
}

/// Open a P2Pool fallback loan (no resting asks) and crank it into a
/// `LoanFixed`. Returns `(borrower, borrower_usdc_ata)` — the ATA is
/// pre-funded so the borrower can repay.
async fn open_p2pool_loan_for_repay(
    fixture: &MarketFixture,
    principal_atoms: u64,
    collateral_atoms: u64,
) -> (solana_sdk::signature::Keypair, Pubkey) {
    // (Quote-only: lenders fund via vaults, not seat debt-deposits — direct
    // debt deposits into a market seat are rejected. The P2Pool borrow's
    // deposit-back CPI inits the lender-side balance slot on first use.)
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;

    // Collateral must clear marginfi's risk-engine health check on the
    // P2Pool borrow. The working `bid_unfilled_residual` test uses a
    // ~50:1 collateral:principal atom ratio (wSOL 9-dec vs USDC 6-dec);
    // mirror that so the `LendingAccountBorrow` CPI is not rejected as
    // undercollateralized.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 2_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, collateral_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // No asks → full residual goes to the P2Pool fallback.
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;

    // Fund a fresh USDC ATA for the repay. The borrowed atoms landed in
    // the lender-side marginfi account, not bob's wallet.
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    fixture.refresh_blockhash().await;
    (bob, bob_usdc)
}

/// Open a P2Pool fallback loan and fund a third-party keeper with debt
/// / collateral token accounts suitable for liquidation or matured
/// settlement.
async fn open_p2pool_loan_for_keeper(
    fixture: &MarketFixture,
    principal_atoms: u64,
    collateral_atoms: u64,
) -> (solana_sdk::signature::Keypair, Pubkey, Pubkey) {
    let _ = open_p2pool_loan_for_repay(fixture, principal_atoms, collateral_atoms).await;
    let keeper = fixture.create_trader().await;
    let keeper_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        keeper_usdc,
        mainnet::usdc_mint(),
        keeper.pubkey(),
        100_000_000,
    );
    let keeper_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(keeper_wsol, keeper.pubkey(), 0);
    fixture.refresh_blockhash().await;
    (keeper, keeper_usdc, keeper_wsol)
}

async fn loan_account_is_closed(fixture: &MarketFixture, sequence: u64) -> bool {
    let (loan_addr, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), sequence);
    let ctx = fixture.context.borrow_mut();
    ctx.banks_client
        .get_account(loan_addr)
        .await
        .unwrap()
        .is_none()
}

/// P2Pool voluntary repay (partial): repaying less than the live
/// liability leaves the loan `Active` with a non-zero residual.
#[tokio::test]
async fn p2pool_partial_repay_leaves_loan_active() {
    let fixture = MarketFixture::new().await;
    let principal_atoms: u64 = 1_000;
    let (bob, bob_usdc) = open_p2pool_loan_for_repay(&fixture, principal_atoms, 60_000).await;

    let liability_pre = p2pool_liability_shares(&fixture).await;
    assert!(liability_pre > 0, "P2Pool loan must carry a live liability");

    // Repay 400 of the ~1_000-atom liability.
    fixture
        .repay(
            &bob, 0, bob_usdc, /*repay_atoms=*/ 400, /*full_repay=*/ false,
        )
        .await
        .unwrap();

    // Loan stays Active; the PDA is intact.
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.state,
        LoanState::Active as u8,
        "partial P2Pool repay must leave the loan Active"
    );
    assert_eq!(loan.loan_type, LoanType::P2Pool as u8);

    // A residual marginfi liability remains, smaller than before.
    let liability_post = p2pool_liability_shares(&fixture).await;
    assert!(
        liability_post > 0,
        "partial repay must leave a non-zero residual liability"
    );
    assert!(
        liability_post < liability_pre,
        "partial repay must shrink the liability (was {}, now {})",
        liability_pre,
        liability_post
    );
    // `outstanding_debt_atoms` mirror tracks the post-CPI live residual.
    assert!(loan.outstanding_debt_atoms > 0);
    // Loan conservation must hold across the partial P2Pool repay event.
    fixture.assert_loan_conservation_holds(0).await;
}

/// P2Pool voluntary repay (full): `full_repay` drives the marginfi
/// liability to exactly zero and closes the loan PDA — the close is
/// gated on the POST-CPI live liability being zero.
#[tokio::test]
async fn p2pool_full_repay_zeroes_liability_and_closes_pda() {
    let fixture = MarketFixture::new().await;
    let principal_atoms: u64 = 1_000;
    let (bob, bob_usdc) = open_p2pool_loan_for_repay(&fixture, principal_atoms, 60_000).await;

    let liability_pre = p2pool_liability_shares(&fixture).await;
    assert!(liability_pre > 0);

    // Full repay: repays exactly the live liability re-derived from
    // `liability_shares` at the live bank price.
    fixture
        .repay(
            &bob, 0, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true,
        )
        .await
        .unwrap();

    // Liability fully retired.
    let liability_post = p2pool_liability_shares(&fixture).await;
    assert_eq!(
        liability_post, 0,
        "full P2Pool repay must drive the marginfi liability to exactly 0"
    );

    assert!(
        loan_account_is_closed(&fixture, 0).await,
        "full P2Pool repay must close the loan PDA only when the \
         post-CPI live liability is zero"
    );
}

/// Multiple P2Pool loans on ONE market share a single per-market
/// borrower marginfi account, so their liabilities commingle. Each loan
/// must close on ITS OWN slice retiring — not on the shared account-total
/// hitting zero. Regression guard for the bug where, with ≥2 P2Pool
/// loans, only the loan repaid with `repay_all` could close and the rest
/// were stranded `Active` (a follow-up repay tripped the "liability is 0"
/// guard, orphaning them forever). Also pins the open-loan counter:
/// `open_borrow_count` ticks up per loan opened and down per loan closed.
#[tokio::test]
async fn p2pool_multiple_loans_each_close_independently() {
    let fixture = MarketFixture::new().await;
    let principal_atoms: u64 = 1_000;
    let collateral_each: u64 = 60_000;
    let n: u64 = 3;

    // One borrower, one seat, collateral for all N loans deposited up
    // front (each bid encumbers `collateral_each` from withdrawable).
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), n * collateral_each + 1_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, n * collateral_each)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Open N P2Pool loans (no resting asks → full residual borrows),
    // each cranked into a `LoanFixed` PDA at its sequence.
    for seq in 0..n {
        fixture
            .place_order_with_flags(
                &bob,
                Side::Bid,
                OrderType::Limit,
                800,
                30 * 86_400,
                principal_atoms,
                collateral_each,
                /*flags=*/ 0,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;
        fixture.crank_matched_loan(seq).await.unwrap();
        fixture.refresh_blockhash().await;
    }

    // Fund the repay ATA (borrowed atoms went to the lender side, not bob).
    let bob_usdc = Pubkey::new_unique();
    fixture.put_token_account(bob_usdc, mainnet::usdc_mint(), bob.pubkey(), 100_000_000);
    fixture.refresh_blockhash().await;

    // All N live: shared liability > 0, count == N, each loan Active.
    assert!(p2pool_liability_shares(&fixture).await > 0);
    assert_eq!(
        fixture.read_seat(&bob.pubkey()).await.open_borrow_count,
        n as u32,
        "N P2Pool loans open → open_borrow_count == N"
    );
    for seq in 0..n {
        assert_eq!(fixture.read_loan(seq).await.state, LoanState::Active as u8);
        assert!(!loan_account_is_closed(&fixture, seq).await);
    }

    // Repay each in turn. Each closes ITS OWN PDA; siblings stay Active
    // until repaid; the count ticks down one per close. (Under the old
    // account-total close gate only the final `repay_all` loan closed.)
    for seq in 0..n {
        fixture
            .repay(&bob, seq, bob_usdc, /*repay_atoms=*/ 0, /*full_repay=*/ true)
            .await
            .unwrap();
        fixture.refresh_blockhash().await;

        assert!(
            loan_account_is_closed(&fixture, seq).await,
            "P2Pool loan {seq} must close on its own slice retiring"
        );
        for later in (seq + 1)..n {
            assert_eq!(
                fixture.read_loan(later).await.state,
                LoanState::Active as u8,
                "sibling loan {later} must stay Active until repaid"
            );
            assert!(!loan_account_is_closed(&fixture, later).await);
        }
        assert_eq!(
            fixture.read_seat(&bob.pubkey()).await.open_borrow_count,
            (n - seq - 1) as u32,
            "open_borrow_count decrements once per closed loan"
        );
    }

    // The last loan's `repay_all` zeroes the shared liability — no orphan
    // dust — and every collateral share is released back to withdrawable.
    assert_eq!(
        p2pool_liability_shares(&fixture).await,
        0,
        "the last loan's repay_all must zero the shared liability — no orphaned residual"
    );
    let seat = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(seat.open_borrow_count, 0);
    assert_eq!(
        seat.collateral_encumbered_shares, 0,
        "all collateral released to withdrawable after every loan closed"
    );
}

/// Full matured settlement on a P2Pool loan must retire the marginfi
/// liability to exactly zero and close/refund the loan PDA.
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn p2pool_full_matured_settlement_zeroes_liability_and_closes_pda() {
    let fixture = MarketFixture::new().await;
    let principal_atoms: u64 = 1_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        open_p2pool_loan_for_keeper(&fixture, principal_atoms, 60_000).await;

    let loan_pre = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_pre.matures_at_unix + 2 * 86_400)
        .await;
    fixture.refresh_blockhash().await;
    fixture.refresh_oracle_freshness().await;

    fixture
        .settle_matured_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .unwrap();

    assert_eq!(p2pool_liability_shares(&fixture).await, 0);
    assert!(loan_account_is_closed(&fixture, 0).await);
}

/// Full LTV liquidation on a P2Pool loan must retire the marginfi
/// liability to exactly zero and close/refund the loan PDA.
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn p2pool_full_liquidation_zeroes_liability_and_closes_pda() {
    let fixture = MarketFixture::new().await;
    let principal_atoms: u64 = 1_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        open_p2pool_loan_for_keeper(&fixture, principal_atoms, 60_000).await;

    fixture
        .set_swb_oracle_price_atoms(mainnet::sol_oracle(), 1_000_000_000_000_000_i128)
        .await;
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
    fixture.refresh_oracle_freshness().await;

    fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .unwrap();

    assert_eq!(p2pool_liability_shares(&fixture).await, 0);
    assert!(loan_account_is_closed(&fixture, 0).await);
}

/// CONFIRMS the collateral-commitment bug: a P2Pool borrow must keep the
/// borrower's collateral committed (it backs the live marginfi liability),
/// NOT return it to `withdrawable`. The residual arm currently calls
/// `unencumber_for_order`, wrongly freeing the collateral — so the seat shows
/// it as withdrawable while marginfi's solvency check still blocks it.
///
/// Expected (post-fix): after the borrow, the locked collateral leaves
/// `withdrawable` and shows in `encumbered`. This test FAILS on the current
/// (buggy) code, demonstrating the issue is real.
#[tokio::test]
async fn p2pool_borrow_keeps_collateral_committed() {
    let fixture = MarketFixture::new().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;

    // Bob deposits 10_000 wSOL collateral onto the borrower marginfi account.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let pre = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(pre.collateral_encumbered_shares, 0, "nothing encumbered before the bid");
    let pre_withdrawable = pre.collateral_withdrawable_shares;

    // No asks → the full residual borrows from marginfi (P2Pool). The bid
    // locks 5_000 collateral, which now backs that live liability.
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            /*principal_atoms=*/ 100,
            /*collateral_atoms=*/ 5_000,
            /*flags=*/ 0,
        )
        .await
        .unwrap();

    let post = fixture.read_seat(&bob.pubkey()).await;
    assert!(
        post.collateral_encumbered_shares > 0,
        "P2Pool borrow must keep the backing collateral committed (encumbered), \
         got 0 — collateral was wrongly returned to withdrawable",
    );
    assert!(
        post.collateral_withdrawable_shares < pre_withdrawable,
        "withdrawable collateral must drop by the amount backing the loan \
         (pre={}, post={})",
        pre_withdrawable,
        post.collateral_withdrawable_shares,
    );
}
