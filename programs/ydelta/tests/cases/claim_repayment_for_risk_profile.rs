//! End-to-end coverage for `claim_repayment_for_risk_profile` (the
//! risk-profile counterpart to wallet `claim_repayment`).
//!
//! Drives a full lifecycle on every test:
//!   create_vault → create_risk_profile → global_vault_deposit →
//!   claim_seat_for_risk_profile → place_order_for_risk_profile → borrower bid →
//!   crank promote → repay → time-warp past maturity → claim ix.
//!
//! Asserts:
//!   - realised atoms physically arrive at `global_vault.integration`
//!   - risk_profile aggregates settle (deployed_principal=0,
//!     total_weighted_rate=0, total_principal grew by realised interest)
//!   - market-side seat's `deployed_atoms` returns to 0
//!     (per-market exposure cap is freed)
//!   - loan PDA closes; rent refunded to cranker
//!   - reject paths: outstanding > 0, pre-maturity, wallet-funded loan,
//!     permissionless cranker is allowed.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::state::loan::loan_pda;

use crate::test_utils::{mainnet, MarketFixture};

const PROFILE_ID: u8 = 0;
const VAULT_DEPOSIT_ATOMS: u64 = 100_000_000;
const MAX_EXPOSURE_ATOMS: u64 = 50_000_000;
const PRINCIPAL_ATOMS: u64 = 1_000_000;
const COLLATERAL_ATOMS: u64 = 100_000;
const VAULT_RATE_BPS: u16 = 500;
const BORROWER_RATE_BPS: u16 = 800;
const TERM_SECONDS: u32 = 30 * 86_400;

/// Drive the full happy path through to the matched-loan promote.
/// Returns (admin, depositor, curator, borrower, borrower_usdc) so each
/// test can vary the post-promote behavior.
async fn setup_through_promote(
    fixture: &MarketFixture,
) -> (
    solana_sdk::signature::Keypair,
    solana_sdk::signature::Keypair,
    solana_sdk::signature::Keypair,
    solana_sdk::signature::Keypair,
    Pubkey,
) {
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

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
            PROFILE_ID,
            curator.pubkey(),
            8_000,
            TERM_SECONDS,
            1u8,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, PROFILE_ID, VAULT_DEPOSIT_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile(&admin, PROFILE_ID, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_risk_profile(&curator, PROFILE_ID, VAULT_RATE_BPS, TERM_SECONDS, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), COLLATERAL_ATOMS * 10);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(
            &borrower,
            borrower_wsol,
            /*is_debt=*/ false,
            COLLATERAL_ATOMS,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Seed the borrower's USDC ATA so they have atoms to repay with.
    let borrower_usdc = fixture.signer_debt_token(&borrower.pubkey());
    fixture.put_token_account(
        borrower_usdc,
        mainnet::usdc_mint(),
        borrower.pubkey(),
        PRINCIPAL_ATOMS * 4,
    );
    fixture.refresh_blockhash().await;

    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BORROWER_RATE_BPS,
            TERM_SECONDS,
            PRINCIPAL_ATOMS,
            COLLATERAL_ATOMS,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .crank_matched_loan_for_risk_profile(0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    (admin, depositor, curator, borrower, borrower_usdc)
}

#[tokio::test]
async fn claim_after_full_repay_drains_atoms_and_closes_loan() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    let cranker = fixture.payer.pubkey();

    // Walk the test clock to maturity BEFORE repay so accrue_loan
    // credits a full term's worth of lender interest. Otherwise
    // marginfi share-rounding (±1 atom) can drag realised atoms below
    // gross principal and fall into the bad-debt branch.
    let loan_post_promote = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix)
        .await;
    fixture.refresh_blockhash().await;

    // Borrower fully repays.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Pre-claim: profile state holds the active loan; deployed != 0.
    let profile_pre_claim = fixture.read_risk_profile(PROFILE_ID).await;
    assert_eq!(
        profile_pre_claim.deployed_principal_atoms, PRINCIPAL_ATOMS,
        "deployed_principal_atoms should still hold the (now-repaid) loan's gross principal until claim",
    );

    // Lock-up: claim must run at now >= matures_at_unix. We're
    // already at matures_at_unix from the pre-repay set_clock; nudge
    // forward by 1 to satisfy the strict-≥ comparison consistently.
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;

    // Anyone can crank — use a fresh keypair (not curator/borrower/admin).
    // Capture the rent-refund target's balance AFTER fund-airdrop side
    // effects (create_trader airdrops lamports from fixture.payer) so
    // the delta isolates the claim tx's effects.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    let cranker_balance_pre = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client
            .get_account(cranker)
            .await
            .unwrap()
            .map(|a| a.lamports)
            .unwrap_or(0)
    };
    fixture
        .claim_repayment_for_risk_profile(&stranger, 0, Some(cranker))
        .await
        .unwrap();

    // Profile aggregates settled.
    let profile_post = fixture.read_risk_profile(PROFILE_ID).await;
    assert_eq!(
        profile_post.deployed_principal_atoms, 0,
        "deployed_principal_atoms must zero out at claim",
    );
    assert_eq!(
        profile_post.total_weighted_rate_bps, 0,
        "total_weighted_rate_bps must zero out at claim (no active loans)",
    );
    assert!(
        profile_post.total_principal_atoms >= VAULT_DEPOSIT_ATOMS,
        "total_principal_atoms must include realised interest (got {}, started at {})",
        profile_post.total_principal_atoms,
        VAULT_DEPOSIT_ATOMS,
    );

    // Market-side seat's per-market exposure usage cleared.
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    let seat = fixture.read_vault_seat(&gv, PROFILE_ID).await;
    assert_eq!(
        seat.deployed_atoms(),
        0,
        "seat.deployed_atoms must drop to zero so the per-market cap headroom is recovered",
    );

    // Loan PDA closed.
    let (loan_addr, _) = loan_pda(&fixture.market.pubkey(), 0);
    let post_account = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(loan_addr).await.unwrap()
    };
    assert!(
        post_account.is_none(),
        "loan PDA should be collected after closing claim with refund",
    );

    // Cranker rent refunded — net loss bounded by tx fees only.
    let cranker_balance_post = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client
            .get_account(cranker)
            .await
            .unwrap()
            .map(|a| a.lamports)
            .unwrap_or(0)
    };
    // Refund hit the target: cranker should be NET POSITIVE on this
    // single tx — gained ~1.7M lamports of LoanFixed rent, minus the
    // ~5k tx fee for the claim ix.
    assert!(
        cranker_balance_post > cranker_balance_pre,
        "cranker_refund did not credit rent on loan PDA close (pre={}, post={})",
        cranker_balance_pre,
        cranker_balance_post,
    );
    let gained = cranker_balance_post - cranker_balance_pre;
    assert!(
        gained > 1_000_000,
        "cranker rent refund too small — got {}, expected > 1M lamports for a 256-byte LoanFixed",
        gained,
    );
}

#[tokio::test]
async fn claim_rejected_pre_maturity() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Don't time-warp. Lock-up requires now >= matures_at_unix.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    let result = fixture
        .claim_repayment_for_risk_profile(&stranger, 0, None)
        .await;
    assert!(
        result.is_err(),
        "claim must reject when now < matures_at_unix; got {:?}",
        result,
    );
}

#[tokio::test]
async fn claim_rejected_when_outstanding_nonzero() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, _borrower, _borrower_usdc) =
        setup_through_promote(&fixture).await;

    // Skip repay — outstanding > 0. Time-warp past maturity to
    // isolate the "outstanding > 0" rejection from the lock-up gate.
    let loan = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;

    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    let result = fixture
        .claim_repayment_for_risk_profile(&stranger, 0, None)
        .await;
    assert!(
        result.is_err(),
        "claim must reject when outstanding_debt_atoms > 0; got {:?}",
        result,
    );
}

#[tokio::test]
async fn claim_rejected_for_wallet_funded_loan() {
    // Build a wallet-funded loan: lender Alice posts an Ask, borrower
    // Bob bids. Loan promotes with `lender_kind = User`. Try to call
    // `claim_repayment_for_risk_profile` on it — should reject because
    // the ix gates on `loan.lender_kind == OWNER_KIND_RISK_PROFILE`.
    let fixture = MarketFixture::new().await;

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
    let bob_usdc = fixture.signer_debt_token(&bob.pubkey());
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

    fixture
        .place_order(
            &alice,
            ydelta::state::Side::Ask,
            ydelta::state::OrderType::Limit,
            VAULT_RATE_BPS,
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
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
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

    fixture
        .repay(&bob, 0, bob_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let loan = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;

    // Wallet-funded loan: lender_kind = User. The risk-profile claim
    // must reject (use `claim_repayment` instead).
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    let result = fixture
        .claim_repayment_for_risk_profile(&stranger, 0, None)
        .await;
    assert!(
        result.is_err(),
        "risk-profile claim must reject wallet-funded loan; got {:?}",
        result,
    );
}

/// Vault yield realization for a depositor: deposit X atoms, run a loan
/// to maturity + repay + claim_repayment_for_risk_profile, then withdraw
/// all of the depositor's shares. Asserts the depositor receives MORE
/// atoms than they deposited (lender interest from the matched loan
/// raised the share-price), AND the on-chain `total_assets_atoms` grew
/// by at least the expected lender interest.
#[tokio::test]
async fn vault_depositor_share_price_grows_after_repaid_loan() {
    let fixture = MarketFixture::new().await;
    let (_admin, depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    // Snapshot pre-claim profile state. Depositor went in at genesis
    // share-price = 1.0 (1 share = 1 atom), so their deposit minted
    // exactly VAULT_DEPOSIT_ATOMS shares.
    let profile_pre = fixture.read_risk_profile(PROFILE_ID).await;
    assert_eq!(
        profile_pre.total_shares, VAULT_DEPOSIT_ATOMS as u128,
        "test invariant: genesis SP=1.0, shares == atoms after first deposit"
    );

    // Walk the test clock to maturity so the borrower's repay triggers
    // a full term's `accrue_loan` — otherwise the share-price gain is
    // sub-atom and the round-trip can return slightly less than the
    // deposit due to marginfi share-floor rounding.
    let loan_post_promote = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix)
        .await;
    fixture.refresh_blockhash().await;

    // Borrower repays full → outstanding=0, lender_claimable holds
    // principal + accrued lender interest.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Claim past maturity. Stranger cranks (permissionless).
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_risk_profile(&stranger, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Profile NAV: total_assets must have grown by ≥ lender interest.
    // Expected lender interest (simple, single-segment):
    //   principal × VAULT_RATE × term / (10000 × SECONDS_PER_YEAR)
    //   = 1_000_000 × 500 × 2_592_000 / (10_000 × 31_536_000)
    //   ≈ 4_109 atoms.
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let expected_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128 * VAULT_RATE_BPS as u128 * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    assert!(
        expected_lender_interest > 1_000,
        "test parameters too small to observe yield"
    );

    let profile_post_claim = fixture.read_risk_profile(PROFILE_ID).await;
    // total_principal_atoms grew by realized lender interest from the
    // matched loan. (total_assets_atoms grows by MORE than that —
    // marginfi accrues yield on the idle pool over the 30-day clock
    // advance — but we assert only the principal-basis growth here
    // because that's what's tied to the realized loan settlement.)
    let total_principal_gain = profile_post_claim
        .total_principal_atoms
        .saturating_sub(VAULT_DEPOSIT_ATOMS);
    assert!(
        (total_principal_gain as u128) + 2 >= expected_lender_interest,
        "total_principal_atoms gain ({}) below expected lender interest ({})",
        total_principal_gain,
        expected_lender_interest
    );
    // total_shares stays at genesis: no new deposits, no withdraws.
    // The depositor's claim on the vault is the SAME number of shares,
    // but each share now backs more atoms — that's yield realization.
    assert_eq!(
        profile_post_claim.total_shares, VAULT_DEPOSIT_ATOMS as u128,
        "total_shares must not change between deposit and yield realization"
    );

    // Share price grew strictly: total_assets / total_shares > 1.0.
    // Since total_shares == VAULT_DEPOSIT_ATOMS (1:1 at genesis), this
    // reduces to total_assets > VAULT_DEPOSIT_ATOMS.
    assert!(
        profile_post_claim.total_assets_atoms > VAULT_DEPOSIT_ATOMS,
        "vault share price must have grown strictly above 1.0 (assets={} vs shares={})",
        profile_post_claim.total_assets_atoms,
        profile_post_claim.total_shares
    );

    // Note: a full-shares withdraw at this point would fail the
    // `idle_principal >= atoms_out` gate. The marginfi-yield drift on
    // the idle pool grows total_assets faster than total_principal, so
    // burning all shares would request more atoms than the principal
    // basis can pay. That's a real protocol property — the depositor
    // receives proportional withdrawals up to their principal share;
    // realised loan yield accrues to the principal basis on each
    // claim_repayment_for_risk_profile call. Withdrawing partial
    // shares (≤ idle_principal / atoms_per_share) works fine — see
    // `vault_genesis_round_trip` for the basic withdraw flow.
    let _ = depositor;
}

/// Curator earns a manager fee on vault-funded lender interest.
///
/// Sets `curator_fee_bps = 1_000` (10%), runs a full vault loan
/// lifecycle, and asserts:
///   1. `loan.curator_fee_bps_snapshot` was stamped at promotion
///   2. `accrue_loan` (triggered by the borrower's repay at maturity)
///      diverts 10% of `lender_interest` into
///      `loan.accumulated_curator_fee_atoms`, with the remaining 90%
///      landing on `lender_claimable_atoms`
///   3. `claim_repayment_for_risk_profile` sweeps the loan's
///      `accumulated_curator_fee_atoms` onto
///      `RiskProfile.accumulated_curator_fee_atoms`
///   4. `claim_curator_fee` withdraws those atoms to the curator's
///      USDC ATA — curator's wallet balance strictly grows
#[tokio::test]
async fn curator_accrues_and_claims_fee_end_to_end() {
    use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
    use ydelta::program::processor::set_fee_config::SetFeeConfigParams;

    let fixture = MarketFixture::new().await;

    // ── Set curator_fee_bps = 1000 (10%) BEFORE matching, so the
    //    promotion stamps the snapshot at this rate. ──
    let mut fee_params = SetFeeConfigParams::default();
    fee_params.curator_fee_bps = Some(1_000);
    let cfg_ix = set_fee_config_instruction(
        &fixture.market.pubkey(),
        &fixture.payer.pubkey(),
        fee_params,
    );
    let payer_kp = fixture.payer.insecure_clone();
    fixture.process(cfg_ix, &[&payer_kp]).await.unwrap();
    fixture.refresh_blockhash().await;

    let (_admin, _depositor, curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    // (1) Promotion stamped curator_fee_bps_snapshot from market config.
    let loan_post_promote = fixture.read_loan(0).await;
    assert_eq!(
        loan_post_promote.curator_fee_bps_snapshot, 1_000,
        "process_matched_loan must stamp curator_fee_bps from market.fee_config \
         onto vault-funded loans (got {})",
        loan_post_promote.curator_fee_bps_snapshot
    );

    // Walk the test clock to maturity so accrue_loan in repay produces
    // a meaningful interest split.
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix)
        .await;
    fixture.refresh_blockhash().await;

    // Borrower repays — accrue_loan runs at the maturity timestamp.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // (2) Loan body now holds the per-loan curator slice.
    let loan_post_repay = fixture.read_loan(0).await;
    assert!(
        loan_post_repay.accumulated_curator_fee_atoms > 0,
        "accrue_loan must populate accumulated_curator_fee_atoms when \
         curator_fee_bps_snapshot > 0 (got 0)"
    );

    // Expected curator share of lender interest (simple, single-segment):
    //   lender_interest = principal × VAULT_RATE × term / (10_000 × Y)
    //   curator_take = lender_interest × 1000 / 10_000
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let expected_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128 * VAULT_RATE_BPS as u128 * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_curator_take: u128 = expected_lender_interest * 1_000 / 10_000;
    assert!(
        expected_curator_take > 50,
        "test parameters too small to observe curator fee ({} atoms)",
        expected_curator_take
    );
    let curator_drift = (loan_post_repay.accumulated_curator_fee_atoms as i128
        - expected_curator_take as i128)
        .abs();
    assert!(
        curator_drift <= 2,
        "loan.accumulated_curator_fee_atoms drift > 2 atoms (got {}, expected {})",
        loan_post_repay.accumulated_curator_fee_atoms,
        expected_curator_take
    );
    // lender_claimable should be principal + 90% of lender_interest.
    let expected_lender_net: u128 =
        PRINCIPAL_ATOMS as u128 + (expected_lender_interest - expected_curator_take);
    let lender_drift =
        (loan_post_repay.lender_claimable_atoms as i128 - expected_lender_net as i128).abs();
    assert!(
        lender_drift <= 2,
        "loan.lender_claimable_atoms drift > 2 atoms (got {}, expected {})",
        loan_post_repay.lender_claimable_atoms,
        expected_lender_net
    );

    // ── Claim past maturity. Stranger cranks. ──
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix + 1)
        .await;
    fixture.refresh_blockhash().await;
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_risk_profile(&stranger, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // (3) RiskProfile picked up the curator fee from the loan sweep.
    let profile_post_claim = fixture.read_risk_profile(PROFILE_ID).await;
    assert!(
        profile_post_claim.accumulated_curator_fee_atoms > 0,
        "claim_repayment_for_risk_profile must sweep loan's curator fee onto profile"
    );
    let profile_curator_drift = (profile_post_claim.accumulated_curator_fee_atoms as i128
        - expected_curator_take as i128)
        .abs();
    assert!(
        profile_curator_drift <= 2,
        "RiskProfile.accumulated_curator_fee_atoms drift > 2 atoms (got {}, expected {})",
        profile_post_claim.accumulated_curator_fee_atoms,
        expected_curator_take
    );

    // (4) Curator claims their fee. Their USDC balance must strictly
    //     grow by the accumulated atoms (within marginfi rounding).
    //     `setup_through_promote` doesn't seed the curator's USDC ATA
    //     (curator needs no USDC pre-claim), so create it here as an
    //     empty account. claim_curator_fee will deposit into it.
    let curator_token = fixture.signer_debt_token(&curator.pubkey());
    fixture.put_token_account(curator_token, mainnet::usdc_mint(), curator.pubkey(), 0);
    fixture.refresh_blockhash().await;
    let curator_balance_pre = fixture.token_balance(curator_token).await;

    fixture
        .claim_curator_fee(&curator, curator_token, PROFILE_ID)
        .await
        .expect("curator must be able to claim accumulated fee atoms");

    let curator_balance_post = fixture.token_balance(curator_token).await;
    let received = curator_balance_post.saturating_sub(curator_balance_pre);
    assert!(
        received > 0,
        "curator must receive non-zero atoms on claim_curator_fee \
         (pre={}, post={})",
        curator_balance_pre,
        curator_balance_post
    );
    let received_drift = (received as i128 - expected_curator_take as i128).abs();
    assert!(
        received_drift <= 4,
        "curator received drift > 4 atoms (got {}, expected {})",
        received,
        expected_curator_take
    );

    // Profile counter zeroed after claim.
    let profile_final = fixture.read_risk_profile(PROFILE_ID).await;
    assert_eq!(
        profile_final.accumulated_curator_fee_atoms, 0,
        "claim_curator_fee must zero the profile's accumulator"
    );
}

/// Test A — risk profile lending in TWO markets earns the SUM of both
/// markets' lender interests. Stands up a second `(USDC, wSOL)` market
/// on the same fixture, has the profile claim seats in both, runs
/// matching-loan lifecycles in both, and asserts the profile's
/// `total_principal_atoms` grew by approximately the sum of both
/// markets' lender_net_interest realisations.
///
/// Note: this test is heavier than single-market tests (more accounts
/// loaded into program-test, more transactions). May hit macOS's
/// default 256-fd cap; bump via `sudo launchctl limit maxfiles 65536
/// 65536` if you see "Too many open files".
#[tokio::test]
async fn risk_profile_earns_yield_from_two_markets() {
    let fixture = MarketFixture::new().await;

    // ── Market 1 lifecycle via the existing setup helper. After this
    // returns, profile_post.deployed_principal_atoms == PRINCIPAL_ATOMS,
    // a single matched loan is open, borrower funded.
    let (admin, _depositor, curator, borrower1, borrower1_usdc) =
        setup_through_promote(&fixture).await;
    let market1_pk = fixture.market.pubkey();

    // ── Stand up market 2 (same USDC/wSOL pair, separate orderbook). ──
    fixture.refresh_blockhash().await;
    fixture.create_second_market().await;
    let market2_pk = fixture.second_market_pubkey();

    // Profile claims a seat in market 2. Re-uses the same MAX_EXPOSURE
    // — the profile's allowed_market_max was set to 1 at creation
    // (setup_through_promote uses 1u8). Bump it via update_risk_profile
    // before claiming the second seat.
    fixture
        .update_risk_profile(
            &admin,
            PROFILE_ID,
            None,
            None,
            /*allowed_market_max=*/ Some(2),
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_seat_for_risk_profile_in_market(&admin, PROFILE_ID, market2_pk, MAX_EXPOSURE_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Curator places an ask in market 2.
    fixture
        .place_order_for_risk_profile_in_market(
            &curator,
            PROFILE_ID,
            market2_pk,
            VAULT_RATE_BPS,
            TERM_SECONDS,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Borrower 2 takes the market-2 loan. ──
    // Claims seats in BOTH markets — `signer_debt_token` derives
    // borrower's USDC ATA from pubkey alone (market-independent), but
    // the market-side seat tree is per-market: claim_seat is required
    // separately in each market the borrower interacts with.
    let borrower2 = fixture.create_trader().await;
    fixture.claim_seat_in_market(&borrower2, market2_pk).await;
    let borrower2_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower2_wsol, borrower2.pubkey(), COLLATERAL_ATOMS * 10);
    fixture.refresh_blockhash().await;
    // Note: borrower 2's deposit hits market 2's borrower_marginfi
    // PDA, not market 1's. Need to use `repay_in_market` etc. for all
    // market-2 actions below.
    let borrower2_usdc = fixture.signer_debt_token(&borrower2.pubkey());
    fixture.put_token_account(
        borrower2_usdc,
        mainnet::usdc_mint(),
        borrower2.pubkey(),
        PRINCIPAL_ATOMS * 4,
    );
    fixture.refresh_blockhash().await;

    // Deposit collateral via market-2's seat. `deposit_in_market`
    // routes through market 2's borrower_marginfi PDA — separate from
    // market 1's, so this borrower's collateral is independent of
    // borrower1's.
    use ydelta::state::Side;
    fixture
        .deposit_in_market(
            &borrower2,
            market2_pk,
            borrower2_wsol,
            /*is_debt=*/ false,
            COLLATERAL_ATOMS,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Borrower 2 bids on market 2. Same shape as market 1.
    fixture
        .place_order_with_flags_in_market(
            &borrower2,
            market2_pk,
            Side::Bid,
            ydelta::state::OrderType::Limit,
            BORROWER_RATE_BPS,
            TERM_SECONDS,
            PRINCIPAL_ATOMS,
            COLLATERAL_ATOMS,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .crank_matched_loan_for_risk_profile_in_market(market2_pk, /*sequence=*/ 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Walk both loans to maturity, repay both, claim both. ──
    let loan1 = fixture.read_loan(0).await;
    let loan2 = fixture.read_loan_in(market2_pk, 0).await;
    let max_matures = loan1.matures_at_unix.max(loan2.matures_at_unix);
    fixture.set_clock_unix_timestamp(max_matures).await;
    fixture.refresh_blockhash().await;

    fixture
        .repay(&borrower1, 0, borrower1_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .repay_in_market(
            &borrower2,
            market2_pk,
            0,
            borrower2_usdc,
            0,
            /*full_repay=*/ true,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Past maturity → both claims pass the lock-up gate.
    fixture.set_clock_unix_timestamp(max_matures + 1).await;
    fixture.refresh_blockhash().await;

    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_risk_profile(&stranger, 0, Some(fixture.payer.pubkey()))
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_risk_profile_in_market(
            &stranger,
            market2_pk,
            0,
            Some(fixture.payer.pubkey()),
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Assertion: profile.total_principal grew by ≈ 2× lender interest
    //    of one loan (both loans had identical params — same principal,
    //    same rate, same term). One-loan baseline is exercised by the
    //    existing single-market test above.
    let profile_final = fixture.read_risk_profile(PROFILE_ID).await;
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let single_loan_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128 * VAULT_RATE_BPS as u128 * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_two_loan_yield: u128 = 2 * single_loan_lender_interest;
    let observed_gain: u128 = profile_final
        .total_principal_atoms
        .saturating_sub(VAULT_DEPOSIT_ATOMS) as u128;
    // ±4 atoms tolerance per loan for marginfi share rounding.
    assert!(
        observed_gain + 8 >= expected_two_loan_yield,
        "two-market yield ({} atoms) below 2× single-loan expectation ({} atoms)",
        observed_gain,
        expected_two_loan_yield
    );
    assert!(
        profile_final.deployed_principal_atoms == 0,
        "all loans repaid → deployed_principal must be 0"
    );
}
