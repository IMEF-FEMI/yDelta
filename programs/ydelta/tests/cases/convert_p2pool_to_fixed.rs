//! `ConvertP2PoolToFixed` (tag 39) — borrower-initiated refinance of a
//! P2Pool loan against compatible primary asks on the orderbook.
//!
//! Happy path: open a P2Pool loan, promote it via the cranker, post a
//! primary ask that fully covers the P2Pool principal, invoke
//! `convert_p2pool_to_fixed`, then crank the resulting MatchedLoan and
//! verify:
//!   - the original P2Pool LoanFixed PDA is closed (data zeroed)
//!   - a fresh Fixed LoanFixed PDA at the new sequence carries the
//!     ask's rate as both lender_rate_bps and borrower_rate_bps
//!   - lender_seat_index points at the ask maker's seat
//!   - the matched ask is removed from the asks tree (full fill)
//!   - the borrower's marginfi liability is zeroed

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use hypertree::NIL;
use ydelta::state::{loan::LoanType, OrderType, Side};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn p2pool_converts_to_fixed_against_matching_ask() {
    let fixture = MarketFixture::new().await;
    let alice = fixture.create_trader().await; // ask maker / new lender
    let bob = fixture.create_trader().await; // P2Pool borrower
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    // Lender side: alice deposits enough USDC to back both the P2Pool
    // deposit-back AND the primary ask she'll later place.
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

    // Borrower side: bob deposits wSOL collateral so marginfi.borrow's
    // solvency check passes. Tiny size to avoid u64 overflow with the
    // mainnet liquidity-vault snapshot.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Open P2Pool: bob bids with no asks on book; flags = 0 opts
    // INTO the P2Pool fallback. ──
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
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
    fixture.refresh_blockhash().await;

    // Promote the P2Pool MatchedLoan to a LoanFixed PDA.
    fixture.crank_matched_loan(0).await.unwrap();
    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.loan_type, LoanType::P2Pool as u8);
    assert!(
        loan_pre.borrower_marginfi_borrow_shares > 0,
        "P2Pool loan should carry non-zero marginfi liability shares"
    );
    let loan_borrower_seat_index = loan_pre.borrower_seat_index;
    let loan_principal = loan_pre.principal_debt_atoms;

    // Alice places a primary Ask sized to match the P2Pool principal
    // exactly. Term must accommodate the remaining loan term.
    let ask_rate_bps: u16 = 700;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            ask_rate_bps,
            30 * 86_400,
            loan_principal,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Convert. ──
    // Snapshot the P2Pool PDA's `created_by` so we can pass it as
    // `cranker_refund` and observe the rent flowing back on close.
    let p2pool_created_by = loan_pre.created_by;
    let market_pre_convert = fixture.read_market_fixed().await;
    let next_matched_loan_sequence = market_pre_convert.matched_loan_sequence;

    fixture.refresh_blockhash().await;
    fixture
        .convert_p2pool_to_fixed(
            &bob,
            /*loan_sequence=*/ 0,
            /*max_acceptable_rate_bps=*/ 1_000, // any value ≥ ask_rate
            Some(p2pool_created_by),
        )
        .await
        .unwrap();

    // ── Assertions. ──
    // 1. The original P2Pool LoanFixed PDA is closed. Since we passed
    //    the matching `cranker_refund`, lamports drained to 0 and the
    //    Solana runtime garbage-collected the account — `get_account`
    //    returns None. (If the refund recipient hadn't matched
    //    `created_by`, the account would still exist with data zeroed
    //    but lamports stranded; we don't exercise that branch here.)
    let (p2pool_pda, _) =
        ydelta::state::loan::loan_pda(&fixture.market.pubkey(), /*sequence=*/ 0);
    {
        let client = fixture.context.borrow_mut();
        let acct = client.banks_client.get_account(p2pool_pda).await.unwrap();
        assert!(
            acct.is_none(),
            "P2Pool PDA should be closed (account GC'd) on full conversion"
        );
    }

    // 2. A fresh Fixed MatchedLoan was emitted at the next sequence.
    //    Crank it to promote into a LoanFixed PDA at that sequence and
    //    inspect the result.
    fixture
        .crank_matched_loan(next_matched_loan_sequence)
        .await
        .unwrap();
    let new_loan = fixture.read_loan(next_matched_loan_sequence).await;
    assert_eq!(
        new_loan.loan_type,
        LoanType::Fixed as u8,
        "refinanced loan is Fixed"
    );
    assert_eq!(
        new_loan.lender_rate_bps, ask_rate_bps,
        "lender_rate adopts the ask's rate"
    );
    assert_eq!(
        new_loan.borrower_rate_bps, ask_rate_bps,
        "borrower_rate collapses to the ask's rate"
    );
    assert_eq!(
        new_loan.principal_debt_atoms, loan_principal,
        "fresh Fixed loan principal == matched amount"
    );
    assert_eq!(
        new_loan.borrower_seat_index, loan_borrower_seat_index,
        "borrower seat carries forward from the P2Pool"
    );
    assert_eq!(
        new_loan.borrower_marginfi_borrow_shares, 0,
        "Fixed loan body never carries marginfi shares"
    );

    // 3. The matched ask was full-filled, so the asks tree is empty.
    let market_post_convert = fixture.read_market_fixed().await;
    assert_eq!(
        market_post_convert.asks_root_index, NIL,
        "ask matched in full → asks tree empty"
    );

    // 4. The borrower's marginfi liability is reduced to atom-level
    //    dust — refinance retired effectively all of it via the
    //    consolidated repay CPI. Sub-atom dust remains because
    //    marginfi accrues `liability_share_value` mid-tx (between our
    //    pre-CPI read of `live_outstanding_atoms` and the actual
    //    `marginfi.repay_atoms` invocation), so atoms→shares floor
    //    rounding leaves a residual smaller than 1 atom of liability.
    //
    //    Threshold: 2 shares in fp48 (= `2 << 48` raw I80F48 bits).
    //    At `liability_share_value ≈ 1`, that's < 2 atoms of liability
    //    per refinance — economically irrelevant but not exactly zero.
    let borrower_marginfi_pk =
        ydelta::validation::get_borrower_integration_account_address(&fixture.market.pubkey()).0;
    let mfi_data = fixture.account_data(borrower_marginfi_pk).await;
    let mfi = marginfi_mocks::state::MarginfiAccount::try_from_account_data(&mfi_data).unwrap();
    let usdc_bank_pk = mainnet::usdc_bank();
    let liability_shares_i80f48 = mfi
        .find_balance(&usdc_bank_pk)
        .map(|b| b.liability_shares.to_i128_bits())
        .unwrap_or(0);
    let dust_threshold_fp48: i128 = 2_i128 << 48;
    assert!(
        liability_shares_i80f48 >= 0 && liability_shares_i80f48 < dust_threshold_fp48,
        "borrower's marginfi liability_shares should be near-zero after full \
         conversion (got {} fp48 bits, threshold {})",
        liability_shares_i80f48,
        dust_threshold_fp48
    );
}

/// Multi-cross + partial residual: walk the asks tree, cross every
/// compatible wallet ask up to the live marginfi liability, and confirm
/// the unfilled portion stays on the original P2Pool body. Two asks
/// (sized to undershoot the loan) combine into 2 Fixed MatchedLoans;
/// the P2Pool LoanFixed PDA is NOT closed and its `principal_debt_atoms`
/// / `collateral_atoms` are reduced in-place by the matched share.
///
/// Two asks (not three) keeps the program-test fixture's open-fd
/// footprint inside macOS's default 256-fd cap; the multi-cross path is
/// already exercised by 2-ask coverage (the matching loop is the same
/// for N ≥ 2).
#[tokio::test]
async fn convert_p2pool_multi_cross_partial_residual_stays() {
    let fixture = MarketFixture::new().await;
    let bob = fixture.create_trader().await; // P2Pool borrower
    fixture.claim_seat(&bob).await;

    // 2 wallet ask makers, same rate → FIFO order in tree.
    let alice = fixture.create_trader().await;
    let carol = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&carol).await;

    for (i, kp) in [&alice, &carol].iter().enumerate() {
        let usdc = Pubkey::new_unique();
        fixture.put_token_account(usdc, mainnet::usdc_mint(), kp.pubkey(), 100_000_000);
        fixture.refresh_blockhash().await;
        // Different deposit sizes so dedup-by-signature won't drop
        // identical txns. Each ask maker deposits enough USDC to back
        // their ask.
        fixture
            .deposit(
                kp,
                usdc,
                /*is_debt=*/ true,
                1_000_000 + i as u64 * 1_000,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;
    }

    // Bob's wSOL collateral. 5000 atoms backs the 100-atom P2Pool bid
    // (same ratio as the original convert test).
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Open the P2Pool loan: bob bids 100 atoms with no asks resting.
    let loan_principal: u64 = 100;
    let loan_collateral: u64 = 5_000;
    fixture
        .place_order_with_flags(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            loan_principal,
            loan_collateral,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(0).await.unwrap();
    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.loan_type, LoanType::P2Pool as u8);
    let loan_borrower_seat = loan_pre.borrower_seat_index;

    // ── Two asks at distinct rates, each covering 30 atoms = 60 total
    //    (< 100 loan principal). After convert: 2 Fixed crosses, 40
    //    atoms residual stays as P2Pool. Distinct rates avoid the
    //    program-test signature-dedup gotcha for identical-shape txns
    //    without needing `warp_to_slot` (which is fd-heavy).
    let cross_size: u64 = 30;
    let alice_rate_bps: u16 = 600;
    let carol_rate_bps: u16 = 700;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            alice_rate_bps,
            30 * 86_400,
            cross_size,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &carol,
            Side::Ask,
            OrderType::Limit,
            carol_rate_bps,
            30 * 86_400,
            cross_size,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Snapshot pre-convert market state so we can assert the new
    // MatchedLoan emissions.
    let market_pre = fixture.read_market_fixed().await;
    let pre_seq = market_pre.matched_loan_sequence;

    fixture
        .convert_p2pool_to_fixed(
            &bob, /*loan_sequence=*/ 0,
            /*max_acceptable_rate_bps=*/ 1_000, // > ask_rate; admits all
            None,  // partial path → no PDA close, no refund needed
        )
        .await
        .unwrap();

    // ── Assertions ──
    // 1. Two new MatchedLoans were emitted (one per cross).
    let market_post = fixture.read_market_fixed().await;
    assert_eq!(
        market_post.matched_loan_sequence - pre_seq,
        2,
        "exactly 2 Fixed MatchedLoans must be emitted (one per ask cross)"
    );
    // All three asks fully consumed → asks tree empty.
    assert_eq!(
        market_post.asks_root_index, NIL,
        "all asks must be drained — each ask was sized to be fully crossed"
    );

    // 2. The P2Pool LoanFixed PDA still exists (partial path doesn't
    //    close it) and its body shrank in-place.
    let (p2pool_pda, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), 0);
    {
        let ctx = fixture.context.borrow_mut();
        let acct = ctx.banks_client.get_account(p2pool_pda).await.unwrap();
        assert!(
            acct.is_some(),
            "P2Pool PDA must NOT be closed on partial conversion"
        );
    }

    let loan_post = fixture.read_loan(0).await;
    assert_eq!(
        loan_post.loan_type,
        LoanType::P2Pool as u8,
        "loan_type must stay P2Pool on partial conversion"
    );
    let total_crossed: u64 = cross_size * 2;
    let expected_residual: u64 = loan_principal - total_crossed;
    assert_eq!(
        loan_post.principal_debt_atoms, expected_residual,
        "P2Pool body must reflect the unfilled portion as remaining principal"
    );
    // Collateral peeled off proportionally to crossed principal.
    // Per-cross collateral = loan_collateral × cross_principal / loan_principal.
    // Residual collateral = loan_collateral × residual / loan_principal.
    // Allow ±3 atoms of integer-division slop across 3 crosses.
    let expected_residual_collateral =
        (loan_collateral as u128 * expected_residual as u128 / loan_principal as u128) as u64;
    let collateral_drift = if loan_post.collateral_atoms > expected_residual_collateral {
        loan_post.collateral_atoms - expected_residual_collateral
    } else {
        expected_residual_collateral - loan_post.collateral_atoms
    };
    assert!(
        collateral_drift <= 3,
        "P2Pool collateral_atoms must be ≈ residual proportion of original \
         (got {}, expected {} ± 3)",
        loan_post.collateral_atoms,
        expected_residual_collateral
    );
    assert_eq!(
        loan_post.borrower_seat_index, loan_borrower_seat,
        "borrower seat must carry forward unchanged"
    );

    // (We deliberately stop at the MatchedLoan-emission assertion above
    // rather than cranking the new Fixed loans inside this test.
    // Cranking them would add 2 more program-test transactions, and
    // the macOS process-level fd cap (256 by default) is already
    // pinching with the convert ix's CPI fanout. The promote→Fixed
    // path is already covered end-to-end by `process_matched_loan`
    // tests; what's unique here is the multi-cross emission, which is
    // proven by the matched_loan_sequence delta + body-shrink
    // assertions above.)
}
