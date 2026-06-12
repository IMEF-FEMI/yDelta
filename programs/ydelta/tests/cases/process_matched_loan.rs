//! End-to-end test for the `ProcessMatchedLoan` cranker (quote-only).
//!
//! Flow:
//! 1. A vault sub-vault is funded and rests an unbounded ask.
//! 2. Bob (borrower) deposits collateral and crosses the ask with an
//!    IOC bid → the matching engine inserts a `MatchedLoan` node.
//! 3. Bob's seat shouldn't see the credit yet — that's the cranker's
//!    job.
//! 4. The sub-vault cranker (`process_matched_loan` with the
//!    vault-settle accounts) promotes the queued match. Assert the
//!    `LoanFixed` PDA exists with the expected fields, Bob's seat got
//!    credited with `amount_to_shares(net_principal)`, the
//!    `matched_loans_root_index` advances back to NIL, and the
//!    market's `accumulated_protocol_fee_shares` picked up the
//!    origination split (zero — `origination_bps = 0` by default).

use hypertree::NIL;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::{Bank, MarginfiAccount};
use ydelta::math::{div_scale, to_scaled};
use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
use ydelta::program::processor::set_fee_config::SetFeeConfigParams;
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::{
    loan::{loan_pda, LoanFixed, LoanState, LoanType, LOAN_FIXED_SIZE},
    MarketFixed,
};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value).unwrap();
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

async fn lender_asset_shares(fixture: &MarketFixture) -> u128 {
    let data = fixture
        .account_data(fixture.lender_marginfi_account_pubkey())
        .await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    mfi.find_balance(&mainnet::usdc_bank())
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares).unwrap())
        .unwrap_or(0)
}

#[tokio::test]
async fn promote_matched_loan_credits_borrower_and_frees_node() {
    let fixture = MarketFixture::new().await;

    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await; // borrower

    // Lender side: a vault profile funded with 10 USDC, resting an
    // unbounded ask at 600bps / 30d.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    // Bob's collateral-side balance is seeded directly into the seat —
    // matching only reads encumbered shares, so we skip the round-trip
    // through the SOL bank for this test.
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    // Bob bids 1_000_000 atoms @ 8% / 30d, crossing the vault ask.
    let principal_atoms: u64 = 1_000_000;
    let collateral_atoms: u64 = 100_000_000;
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

    // After matching, the market's matched_loans_root_index should point
    // at a single node with sequence 0 (the first matched loan), and
    // matched_loan_sequence has been bumped to 1.
    let market_data = fixture.account_data(fixture.market.pubkey()).await;
    let header_pre: &MarketFixed =
        bytemuck::from_bytes(&market_data[..std::mem::size_of::<MarketFixed>()]);
    assert_ne!(
        header_pre.matched_loans_root_index, NIL,
        "expected a MatchedLoan node to be present pre-crank"
    );
    assert_eq!(
        header_pre.matched_loan_sequence, 1,
        "matched_loan_sequence should advance to 1 after the first match"
    );
    let cranking_sequence: u64 = 0;

    // Bob's seat shouldn't carry any debt withdrawable shares yet — the
    // cranker is what credits him.
    let bob_seat_pre = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(bob_seat_pre.debt_withdrawable_shares, 0);

    // ─── Crank (vault-funded loan → sub-vault cranker).
    fixture
        .crank_matched_loan_for_sub_vault(cranking_sequence)
        .await
        .unwrap();

    // Loan PDA exists with expected values.
    let (loan_addr, loan_bump) = loan_pda(&fixture.market.pubkey(), cranking_sequence);
    let loan_data = fixture.account_data(loan_addr).await;
    assert_eq!(loan_data.len(), LOAN_FIXED_SIZE);
    let loan: &LoanFixed = bytemuck::from_bytes(&loan_data[..std::mem::size_of::<LoanFixed>()]);
    assert_eq!(loan.market, fixture.market.pubkey());
    assert_eq!(loan.matched_loan_sequence, cranking_sequence);
    assert_eq!(loan.bump, loan_bump);
    assert_eq!(loan.state, LoanState::Active as u8);
    assert_eq!(loan.loan_type, LoanType::Fixed as u8);
    assert_eq!(loan.principal_debt_atoms, principal_atoms);
    assert_eq!(loan.outstanding_debt_atoms, principal_atoms); // origination_bps = 0
    assert_eq!(loan.lender_claimable_atoms, principal_atoms);
    // Rate-stamping invariants: lender_rate == ask_rate (600);
    // borrower_rate == max(bid_rate, ask_rate + protocol_fee_bps_floor).
    // protocol_fee_bps_floor defaults to 0 → borrower_rate == 800 (bid_rate).
    assert_eq!(loan.borrower_rate_bps, 800);
    assert_eq!(loan.lender_rate_bps, 600);
    assert!(loan.matures_at_unix > loan.started_at_unix);
    // Term must be exactly the placed term — the cranker's stamp.
    assert_eq!(
        (loan.matures_at_unix - loan.started_at_unix) as u32,
        30 * 86_400,
        "promoted loan term must equal matched_at term_seconds"
    );
    // Conservation identity at promote-time.
    let _ = loan_data;
    fixture
        .assert_loan_conservation_holds(cranking_sequence)
        .await;

    // Bob's seat is now credited with amount_to_shares(net_principal).
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let expected_credit = amount_to_shares_against(&bank_data, principal_atoms);
    let bob_seat_post = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(
        bob_seat_post.debt_withdrawable_shares, expected_credit,
        "borrower seat credited with amount_to_shares(net_principal) = {} (got {})",
        expected_credit, bob_seat_post.debt_withdrawable_shares
    );

    // The MatchedLoan tree is now empty — the only queued node was just
    // promoted and its slot returned to the free list.
    let market_data_post = fixture.account_data(fixture.market.pubkey()).await;
    let header_post: &MarketFixed =
        bytemuck::from_bytes(&market_data_post[..std::mem::size_of::<MarketFixed>()]);
    assert_eq!(
        header_post.matched_loans_root_index, NIL,
        "matched_loans tree should be empty after cranking the only node"
    );

    // origination_bps defaulted to 0, so no fee accrued.
    assert_eq!(header_post.accumulated_protocol_fee_shares, 0);

    // Vault-idle invariant — vault profile's encumbered_in_orders
    // got drained into deployed_principal_atoms at crank time.
    fixture.assert_vault_idle_invariant(1).await;
    let profile = fixture.read_sub_vault(1).await;
    assert_eq!(
        profile.deployed_principal_atoms, principal_atoms,
        "promote must increment deployed_principal_atoms by the loan's principal"
    );
    assert_eq!(
        profile.encumbered_in_orders_atoms, 0,
        "promote must drain encumbered_in_orders to 0"
    );
}

#[tokio::test]
async fn promote_matched_loan_keeps_borrower_and_fee_claims_backed() {
    let fixture = MarketFixture::new().await;
    let mut fee_params = SetFeeConfigParams::default();
    fee_params.origination_bps = Some(100);
    let cfg_ix = set_fee_config_instruction(
        &fixture.market.pubkey(),
        &fixture.payer.pubkey(),
        fee_params,
    );
    let payer_kp = fixture.payer.insecure_clone();
    fixture.process(cfg_ix, &[&payer_kp]).await.unwrap();

    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await;

    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_333_337;
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            100_000_000,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .crank_matched_loan_for_sub_vault(0)
        .await
        .unwrap();

    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let gross_shares = amount_to_shares_against(&bank_data, principal_atoms);
    let market_shares = lender_asset_shares(&fixture).await;
    let bob_seat = fixture.read_seat(&bob.pubkey()).await;
    let market_data = fixture.account_data(fixture.market.pubkey()).await;
    let header: &MarketFixed =
        bytemuck::from_bytes(&market_data[..std::mem::size_of::<MarketFixed>()]);
    let claimed_shares = bob_seat
        .debt_withdrawable_shares
        .checked_add(header.accumulated_protocol_fee_shares)
        .unwrap();

    assert_eq!(
        market_shares, gross_shares,
        "market lender integration should be funded with exactly the matched principal's share value"
    );
    assert!(
        header.accumulated_protocol_fee_shares > 0,
        "non-zero origination_bps should accrue protocol fee shares"
    );
    assert!(
        claimed_shares <= market_shares,
        "borrower + protocol share claims ({}) must stay backed by market integration shares ({})",
        claimed_shares,
        market_shares
    );
}

#[tokio::test]
async fn curator_fee_snapshot_is_match_time_not_promotion_time() {
    let fixture = MarketFixture::new().await;

    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await;

    // (1) v1 D3b: the curator fee lives on the SUB-VAULT, set at
    // creation (10%) and immutable — `UpdateSubVault` has no fee field,
    // so the old market-admin front-run window is closed at the type
    // level. This test pins the remaining half of the guarantee: the
    // fee is snapshotted onto the MatchedLoan at MATCH time and carried
    // to the LoanFixed at promotion.
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_pool_sub_vault_full(
            curator.pubkey(),
            /*spread_bps=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*liquidation_ltv_bps=*/ 9_000,
            30 * 86_400,
            /*curator_fee_bps=*/ 1_000,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        20_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_sub_vault(&curator, 1, 600, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    // (2) Borrower bid crosses the ask; the MatchedLoan node must stamp
    // the sub-vault's 1_000 bps at match time.
    let principal_atoms: u64 = 1_000_000;
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            100_000_000,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // (3) Cranker promotes the queued match; the LoanFixed must carry
    // the match-time snapshot.
    fixture
        .crank_matched_loan_for_sub_vault(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.curator_fee_bps_snapshot, 1_000,
        "loan must carry the sub-vault's match-time curator_fee_bps; got {}",
        loan.curator_fee_bps_snapshot,
    );
}
