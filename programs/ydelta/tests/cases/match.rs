use solana_sdk::signer::Signer;

use ydelta::state::{OrderType, Side};

use crate::test_utils::TestFixture;

async fn fund_trader(
    fixture: &mut TestFixture,
    debt_amt: u64,
    coll_amt: u64,
) -> solana_sdk::signature::Keypair {
    let trader = fixture.create_trader(0, 0).await;
    fixture.claim_seat(&trader).await.unwrap();

    let debt_ata = fixture
        .create_token_account_and_mint(&trader.pubkey(), &fixture.debt_mint_key(), debt_amt)
        .await;
    let coll_ata = fixture
        .create_token_account_and_mint(&trader.pubkey(), &fixture.collateral_mint_key(), coll_amt)
        .await;
    fixture.refresh_blockhash().await;
    let _ = (debt_ata, coll_ata);
    if debt_amt > 0 {
        fixture
            .seed_seat_shares(&trader.pubkey(), debt_amt as u128, true)
            .await;
    }
    if coll_amt > 0 {
        fixture
            .seed_seat_shares(&trader.pubkey(), coll_amt as u128, false)
            .await;
    }
    trader
}

#[tokio::test]
async fn match_full_fill_drains_both_encumbrances() {
    let mut fixture = TestFixture::new().await;
    let alice = fund_trader(&mut fixture, 1_000, 0).await;
    let bob = fund_trader(&mut fixture, 0, 5_000).await;

    // alice asks 700 @ 600 / 30d
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            700,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // bob bids 700 @ 800 / 30d, collateral 1000
    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            700,
            1_000,
        )
        .await
        .unwrap();

    let alice_seat = fixture.read_seat(&alice.pubkey()).await;
    let bob_seat = fixture.read_seat(&bob.pubkey()).await;
    let market = fixture.read_market().await;

    // Matched atoms leave `*_encumbered` without an immediate re-credit
    // on the seats. Encumbered drops to zero on both sides; the
    // pending-loan amount is parked in `matched_loan_sequence` for the
    // cranker to promote.
    assert_eq!(alice_seat.debt_encumbered_shares, 0);
    assert_eq!(alice_seat.debt_withdrawable_shares, 300); // 1000 - 700
    assert_eq!(bob_seat.collateral_encumbered_shares, 0);
    assert_eq!(bob_seat.collateral_withdrawable_shares, 4_000); // 5000 - 1000
    assert_eq!(market.fixed.matched_loan_sequence, 1);
    assert_eq!(fixture.count_orders(Side::Ask).await, 0);
    assert_eq!(fixture.count_orders(Side::Bid).await, 0);
}

#[tokio::test]
async fn match_partial_fill_resting_maker() {
    let mut fixture = TestFixture::new().await;
    let alice = fund_trader(&mut fixture, 1_000, 0).await;
    let bob = fund_trader(&mut fixture, 0, 5_000).await;

    // alice asks 1000 @ 600 / 30d
    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            600,
            30 * 86_400,
            1_000,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // bob bids 600 @ 800 / 30d, collateral 900
    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            600,
            900,
        )
        .await
        .unwrap();

    let alice_seat = fixture.read_seat(&alice.pubkey()).await;
    let bob_seat = fixture.read_seat(&bob.pubkey()).await;
    let market = fixture.read_market().await;

    assert_eq!(alice_seat.debt_encumbered_shares, 400); // 600 matched, 400 still encumbered
    assert_eq!(bob_seat.collateral_encumbered_shares, 0);
    assert_eq!(market.fixed.matched_loan_sequence, 1);
    assert_eq!(fixture.count_orders(Side::Ask).await, 1);
    assert_eq!(fixture.count_orders(Side::Bid).await, 0);
}

#[tokio::test]
async fn match_account_count_constant_in_n() {
    use solana_program_test::ProgramTestContext;
    use std::cell::RefMut;

    let mut fixture = TestFixture::new().await;

    // Five distinct asks — five distinct makers — for a single bid to sweep.
    let mut askers = Vec::new();
    for i in 0..5u16 {
        let asker = fund_trader(&mut fixture, 100, 0).await;
        fixture.refresh_blockhash().await;
        fixture
            .place_order(
                &asker,
                Side::Ask,
                OrderType::Limit,
                500 + i * 50, // 500, 550, 600, 650, 700
                30 * 86_400,
                100,
                0,
            )
            .await
            .unwrap();
        fixture.refresh_blockhash().await;
        askers.push(asker);
    }

    let bob = fund_trader(&mut fixture, 0, 1_000).await;
    fixture.refresh_blockhash().await;

    // Build the bid ix manually so we can inspect its `account_keys.len()`.
    let debt_bank_lva = fixture.derive_marginfi_lva(fixture.debt_bank);
    let borrower_debt_token = fixture.signer_debt_token(&bob.pubkey()).await;
    let bid_ix =
        ydelta::program::instruction_builders::place_order_instruction::place_order_instruction(
            &fixture.market_key(),
            &bob.pubkey(),
            &fixture.marginfi_group,
            &fixture.debt_bank,
            &fixture.collateral_bank,
            &[fixture.debt_oracle],
            &[fixture.collateral_oracle],
            &fixture.debt_liquidity_vault,
            &debt_bank_lva,
            &borrower_debt_token,
            &fixture.debt_mint.pubkey(),
            &spl_token::id(),
            &marginfi_mocks::ID,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            500,
            750,
            0,
            // OB_ONLY — synth-bank native fixture can't satisfy
            // marginfi.borrow's LVA seed check, so the residual must go to
            // Drop rather than P2PoolBorrow. Tests that exercise P2Pool
            // live under MarketFixture (real marginfi banks).
            ydelta::state::market_helpers::FLAG_OB_ONLY,
            None,
            None, // borrower_ltv_bps default
        );
    // Matching tx carries marginfi/oracle accounts for LTV-at-match
    // + P2Pool + the P2Pool deposit-back accounts + the GlobalVault
    // PDA for match-time vault encumbrance. Constant in N (resting
    // orders crossed).
    // 21 = 3 (payer/market/system) + 10 (marginfi/oracle) + 2
    //   (borrower debt ATA + token program for the P2Pool borrow
    //   CPI destination)
    //   + 2 (signer's UserAccount PDA + system_program for
    //   auto-create) + 2 (lender_marginfi_account +
    //   market_debt_vault for P2Pool deposit-back)
    //   + 1 (vault PDA for match-time vault state updates when
    //   crossing vault-owned makers)
    //   + 1 (global_config).
    assert_eq!(bid_ix.accounts.len(), 21);

    let blockhash = fixture.context.borrow().last_blockhash;
    let payer = fixture.payer_keypair();
    let bob_kp = bob.insecure_clone();
    let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
        &[bid_ix],
        Some(&payer.pubkey()),
        &[&payer, &bob_kp],
        blockhash,
    );
    {
        let client: RefMut<ProgramTestContext> = fixture.context.borrow_mut();
        client.banks_client.process_transaction(tx).await.unwrap();
    }

    let market = fixture.read_market().await;
    // All five asks crossed (5 × 100 = 500 principal exhausted).
    assert_eq!(market.fixed.matched_loan_sequence, 5);
    assert_eq!(fixture.count_orders(Side::Ask).await, 0);
}

#[tokio::test]
async fn match_picks_best_ask_first_when_taker_partial() {
    // Three asks at 500, 600, 700 (each 100 atoms). A 100-atom bid at
    // 800 should match exactly the 500-rate ask (best price for the
    // borrower) and leave the 600/700 asks resting. This catches the
    // case where the matching engine walks the wrong end of the rate
    // tree first.
    let mut fixture = TestFixture::new().await;

    let mut askers = Vec::new();
    for (i, rate) in [500u16, 600, 700].iter().enumerate() {
        let asker = fund_trader(&mut fixture, 100, 0).await;
        fixture.refresh_blockhash().await;
        fixture
            .place_order(
                &asker,
                Side::Ask,
                OrderType::Limit,
                *rate,
                30 * 86_400,
                100,
                0,
            )
            .await
            .unwrap_or_else(|_| panic!("ask #{i} at {rate}bps should rest"));
        fixture.refresh_blockhash().await;
        askers.push(asker);
    }

    let bob = fund_trader(&mut fixture, 0, 1_000).await;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            100,
            200,
        )
        .await
        .unwrap();

    // Exactly one match landed.
    let market = fixture.read_market().await;
    assert_eq!(market.fixed.matched_loan_sequence, 1);
    // Two asks still resting — the matched ask was the 500-rate one.
    assert_eq!(fixture.count_orders(Side::Ask).await, 2);

    // Confirm by checking the 500-rate asker's seat: their debt
    // encumbrance fully drained (matched), the others' didn't.
    let alice_seat = fixture.read_seat(&askers[0].pubkey()).await;
    let bob_seat_for_600 = fixture.read_seat(&askers[1].pubkey()).await;
    let bob_seat_for_700 = fixture.read_seat(&askers[2].pubkey()).await;
    assert_eq!(
        alice_seat.debt_encumbered_shares, 0,
        "best-priced ask (500bps) should have been picked first"
    );
    assert_eq!(bob_seat_for_600.debt_encumbered_shares, 100);
    assert_eq!(bob_seat_for_700.debt_encumbered_shares, 100);
}

#[tokio::test]
async fn match_skips_term_incompatible_best_and_walks_to_compatible() {
    // Best ask has too-short term for the bid, but a worse-rate ask
    // accepts longer terms. Matching should walk past the term-
    // incompatible best maker rather than bailing out.
    let mut fixture = TestFixture::new().await;

    // ask A: 500bps / 7-day term (best rate, but too short)
    let alice = fund_trader(&mut fixture, 100, 0).await;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(&alice, Side::Ask, OrderType::Limit, 500, 7 * 86_400, 100, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ask B: 700bps / 60-day term (worse rate but compatible term)
    let charlie = fund_trader(&mut fixture, 100, 0).await;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &charlie,
            Side::Ask,
            OrderType::Limit,
            700,
            60 * 86_400,
            100,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // bid: 800bps / 30-day term. bid_term=30d > ask A's 7d → skip A.
    // bid_term=30d <= ask B's 60d → match B.
    let bob = fund_trader(&mut fixture, 0, 1_000).await;
    fixture.refresh_blockhash().await;
    fixture
        .place_order(
            &bob,
            Side::Bid,
            OrderType::Limit,
            800,
            30 * 86_400,
            100,
            200,
        )
        .await
        .unwrap();

    // Exactly one match landed (against ask B).
    let market = fixture.read_market().await;
    assert_eq!(market.fixed.matched_loan_sequence, 1);
    // ask A still rests (term-incompatible).
    let alice_seat = fixture.read_seat(&alice.pubkey()).await;
    assert_eq!(
        alice_seat.debt_encumbered_shares, 100,
        "term-incompatible best ask should be untouched"
    );
    // ask B was matched.
    let charlie_seat = fixture.read_seat(&charlie.pubkey()).await;
    assert_eq!(
        charlie_seat.debt_encumbered_shares, 0,
        "compatible second-best ask should have matched"
    );
    assert_eq!(fixture.count_orders(Side::Ask).await, 1);
}
