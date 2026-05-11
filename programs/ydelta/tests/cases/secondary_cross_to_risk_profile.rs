//! End-to-end coverage for SecondaryLoanSale crossing a risk-profile
//! primary ask. Verifies:
//!   - the matching engine admits the cross (no longer skips
//!     vault-funded asks),
//!   - the risk-profile gate runs at match time (idle pool +
//!     per-market exposure cap; bookkeeping bumps inline under the
//!     vault account borrow),
//!   - the cranker's secondary-finalize path migrates the buyer's cash
//!     atoms via `do_vault_settle` (vault.integration → market.lender)
//!     and stamps the loan body's lender_kind / lender_global_vault /
//!     lender_profile_id / lender_rate_bps on full transfer,
//!   - the seller is paid out as withdrawable shares on their seat,
//!   - risk-profile aggregates settle (deployed += cash_paid,
//!     encumbered_in_orders -= cash_paid, total_weighted_rate +=
//!     cash_paid × ask_rate).

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::state::claimed_seat::OWNER_KIND_RISK_PROFILE;
use ydelta::state::{OrderType, Side};

use crate::test_utils::{mainnet, MarketFixture};

const VAULT_RATE_BPS: u16 = 500; // ask rate the new lender (vault) earns
const BORROWER_RATE_BPS: u16 = 800; // rate borrower keeps paying
const ALICE_RATE_BPS: u16 = 600; // initial wallet lender's ask
const TERM_SECONDS: u32 = 30 * 86_400;
const PRINCIPAL_ATOMS: u64 = 100;
const COLLATERAL_ATOMS: u64 = 5_000;
const VAULT_DEPOSIT_ATOMS: u64 = 10_000_000;
const MAX_EXPOSURE_ATOMS: u64 = 1_000_000;

#[tokio::test]
async fn secondary_bid_crosses_risk_profile_ask_full_transfer() {
    let fixture = MarketFixture::new().await;

    // ─── Build a wallet primary loan: alice (lender) + bob (borrower).
    let alice = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&alice).await;
    fixture.claim_seat(&bob).await;

    let alice_usdc = fixture.signer_debt_token(&alice.pubkey());
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

    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .place_order(
            &alice,
            Side::Ask,
            OrderType::Limit,
            ALICE_RATE_BPS,
            TERM_SECONDS,
            PRINCIPAL_ATOMS,
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
            BORROWER_RATE_BPS,
            TERM_SECONDS,
            PRINCIPAL_ATOMS,
            COLLATERAL_ATOMS,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;

    // ─── Stand up a risk-profile that will buy the loan.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            /*profile_id=*/ 0,
            curator.pubkey(),
            /*max_ltv_bps=*/ 8_000,
            TERM_SECONDS,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(
            &depositor,
            depositor_token,
            /*profile_id=*/ 0,
            VAULT_DEPOSIT_ATOMS,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, /*profile_id=*/ 0, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // Curator places a primary ask at VAULT_RATE_BPS — below the loan's
    // BORROWER_RATE_BPS, so the secondary cross gate
    // (`ask_rate ≤ borrower_rate`) admits it.
    fixture
        .place_order_for_risk_profile(
            &curator,
            /*profile_id=*/ 0,
            VAULT_RATE_BPS,
            TERM_SECONDS,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ─── Seller (alice) posts a SecondaryLoanSale. The matcher walks
    // the asks tree, finds the risk-profile ask at the top, crosses
    // for the loan's full principal at par. No residual rests.
    fixture
        .place_secondary_order(&alice, /*loan_seq=*/ 0, /*last_valid_unix_ts=*/ 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Pre-promote: the matching engine bumped encumbered_in_orders
    // (open-ended ask backed by idle pool) but did not yet move atoms
    // — the cranker's `do_vault_settle` does that.
    let profile_pre_promote = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile_pre_promote.encumbered_in_orders_atoms, PRINCIPAL_ATOMS,
        "match-time gate must encumber the matched principal"
    );
    assert_eq!(
        profile_pre_promote.deployed_principal_atoms, 0,
        "deployed_principal_atoms must stay 0 until cranker promote"
    );

    // ─── Cranker finalises. The cranker is the test payer; queue
    // sequence is 1 (loan was sequence 0, secondary cross is 1).
    fixture
        .crank_secondary_matched_loan_for_risk_profile_buyer(
            /*queue_sequence=*/ 1, /*referenced_loan_sequence=*/ 0,
        )
        .await
        .unwrap();

    // Loan body re-stamped to the risk-profile new lender.
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.lender_kind, OWNER_KIND_RISK_PROFILE,
        "loan.lender_kind must flip to OWNER_KIND_RISK_PROFILE post-transfer",
    );
    assert_eq!(
        loan.lender_rate_bps, VAULT_RATE_BPS,
        "loan.lender_rate_bps must update to the risk-profile ask rate",
    );
    assert_eq!(
        loan.lender_profile_id, 0,
        "loan.lender_profile_id must stamp the buying profile_id",
    );
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    assert_eq!(
        loan.lender_global_vault, gv,
        "loan.lender_global_vault must point to the GlobalVault PDA",
    );
    assert_eq!(
        loan.lender_claimable_atoms, 0,
        "secondary full transfer resets lender_claimable to 0 (Option-A)",
    );

    // Risk-profile aggregates: deployed bumped by gross cash_paid;
    // encumbered drained back; total_weighted_rate carries the new
    // loan's contribution at VAULT_RATE_BPS.
    let profile_post = fixture.read_risk_profile(0).await;
    assert_eq!(
        profile_post.deployed_principal_atoms, PRINCIPAL_ATOMS,
        "deployed_principal_atoms must include the just-acquired loan",
    );
    assert_eq!(
        profile_post.encumbered_in_orders_atoms, 0,
        "encumbered_in_orders_atoms must drop to 0 once the cranker promotes",
    );
    assert_eq!(
        profile_post.total_weighted_rate_bps,
        (PRINCIPAL_ATOMS as u128) * (VAULT_RATE_BPS as u128),
        "total_weighted_rate_bps must reflect the new loan at the risk-profile ask rate",
    );

    // Per-market seat usage saturated by the matched amount.
    let seat = fixture.read_vault_seat(&gv, /*profile_id=*/ 0).await;
    assert_eq!(
        seat.deployed_atoms(),
        PRINCIPAL_ATOMS,
        "seat.deployed_atoms must reflect the risk_profile's per-market exposure",
    );

    // Seller (alice) was paid out as debt_withdrawable_shares: the
    // matching engine + cranker credit cash_paid_shares onto her seat.
    let alice_seat = fixture.read_seat(&alice.pubkey()).await;
    assert!(
        alice_seat.debt_withdrawable_shares > 0,
        "alice must receive cash_paid as withdrawable shares",
    );
}
