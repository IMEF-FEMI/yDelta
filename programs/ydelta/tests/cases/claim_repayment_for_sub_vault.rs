//! End-to-end coverage for `claim_repayment_for_sub_vault` (the
//! sub-vault counterpart to wallet `claim_repayment`).
//!
//! Drives a full lifecycle on every test:
//!   create_vault → create_sub_vault → global_vault_deposit →
//!   claim_seat_for_sub_vault → place_order_for_sub_vault → borrower bid →
//!   crank promote → repay → time-warp past maturity → claim ix.
//!
//! Asserts:
//!   - realised atoms physically arrive at `global_vault.integration`
//!   - sub_vault aggregates settle (deployed_principal=0,
//!     total_weighted_rate=0, total_principal grew by realised interest)
//!   - market-side seat's `deployed_atoms` returns to 0
//!     (per-market exposure cap is freed)
//!   - loan PDA closes; rent refunded to cranker
//!   - reject paths: outstanding > 0, pre-maturity, wallet-funded loan,
//!     permissionless cranker is allowed.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::state::Side;

use crate::test_utils::{mainnet, MarketFixture};

const SUB_VAULT_ID: u16 = 1;
const VAULT_DEPOSIT_ATOMS: u64 = 100_000_000;
const PRINCIPAL_ATOMS: u64 = 1_000_000;
// The match-time LTV gate normalizes for the USDC(6-dec) /
// wSOL(9-dec) decimal gap: backing $1 of USDC debt against ~$100-200
// SOL requires tens of millions of wSOL lamports. 50M lamports
// (~0.05 SOL) clears the init-weight requirement comfortably.
const COLLATERAL_ATOMS: u64 = 50_000_000;
const VAULT_RATE_BPS: u16 = 500;
const BORROWER_RATE_BPS: u16 = 3_000;
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
    setup_through_promote_with_fee(fixture, 0).await
}

/// Same as [`setup_through_promote`] but stamps `curator_fee_bps` on the
/// Pool sub-vault at creation (v1 D3b — the fee lives on the sub-vault).
async fn setup_through_promote_with_fee(
    fixture: &MarketFixture,
    curator_fee_bps: u16,
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
    // v1 D4: VAULT_RATE_BPS is the sub-vault's SPREAD over the live
    // bank APR, set at creation (placement is parameterless).
    fixture
        .create_pool_sub_vault_full(
            curator.pubkey(),
            /*spread_bps=*/ VAULT_RATE_BPS,
            /*max_ltv_bps=*/ 8_000,
            /*liquidation_ltv_bps=*/ 9_000,
            TERM_SECONDS,
            curator_fee_bps,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, SUB_VAULT_ID, VAULT_DEPOSIT_ATOMS)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    // No claim-seat step — the vault market-seat is auto-created on the
    // curator's first place_order_for_sub_vault. The ask is
    // unbounded; the cross is capped by the sub_vault's idle balance.
    fixture
        .place_order_for_sub_vault(&curator, SUB_VAULT_ID, VAULT_RATE_BPS, TERM_SECONDS, 0)
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
        .crank_matched_loan_for_sub_vault(0)
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

    // Walk to maturity BEFORE repay so accrue_loan credits the full
    // term's worth of lender interest. Snapshot matures_at now since
    // the loan PDA closes during the repay below.
    let loan_post_promote = fixture.read_loan(0).await;
    let matures_at_unix = loan_post_promote.matures_at_unix;
    fixture.set_clock_unix_timestamp(matures_at_unix).await;
    fixture.refresh_blockhash().await;

    // Cranker (who paid for the LoanFixed rent at promotion-time) is
    // `loan.created_by` — the test fixture's payer. Snapshot the balance
    // BEFORE the repay tx that closes the PDA + refunds.
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

    // Borrower fully repays. This is now the close event — loan PDA gone,
    // sub-vault decrements applied, rent refunded to cranker.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    assert!(
        fixture.loan_account_is_closed(0).await,
        "full repay must close the loan PDA in-place",
    );
    // Cranker rent refunded by the repay tx (the close-and-refund
    // happens in repay now, not claim). Note: borrower paid the tx fee
    // for repay, not the cranker — so the cranker's balance should
    // strictly grow by the rent without offsetting fees.
    let cranker_balance_post_repay = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client
            .get_account(cranker)
            .await
            .unwrap()
            .map(|a| a.lamports)
            .unwrap_or(0)
    };
    let gained = cranker_balance_post_repay - cranker_balance_pre;
    assert!(
        gained > 1_000_000,
        "cranker rent refund too small after repay close — got {}, expected > 1M lamports for a LoanFixed PDA",
        gained,
    );

    // SubVault decrements applied at repay-close: deployed_principal == 0,
    // total_weighted_rate_bps == 0, total_principal grew by realised interest.
    let sub_vault_post_repay = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault_post_repay.deployed_principal_atoms, 0,
        "deployed_principal_atoms must zero out at repay-close (no longer waits for claim)",
    );
    assert_eq!(
        sub_vault_post_repay.total_weighted_rate_bps, 0,
        "total_weighted_rate_bps must zero out at repay-close (no active loans)",
    );
    assert!(
        sub_vault_post_repay.total_principal_atoms >= VAULT_DEPOSIT_ATOMS,
        "total_principal_atoms must include realised interest (got {}, started at {})",
        sub_vault_post_repay.total_principal_atoms,
        VAULT_DEPOSIT_ATOMS,
    );
    // pending_claim_atoms holds the atoms in-transit (lender_marginfi →
    // pending sweep by curator).
    assert!(
        sub_vault_post_repay.pending_claim_atoms > 0,
        "repay-close must mark atoms as pending sweep — got {}",
        sub_vault_post_repay.pending_claim_atoms,
    );

    // Now the curator's claim — pure seat sweeper. Anyone can call.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, SUB_VAULT_ID)
        .await
        .unwrap();

    // Claim moved atoms from lender_marginfi_account to the vault's
    // integration account; pending_claim_atoms decremented.
    let sub_vault_post_claim = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert!(
        sub_vault_post_claim.pending_claim_atoms < sub_vault_post_repay.pending_claim_atoms,
        "claim must shrink pending_claim_atoms (pre={}, post={})",
        sub_vault_post_repay.pending_claim_atoms,
        sub_vault_post_claim.pending_claim_atoms,
    );
    // Vault-idle invariant must still hold.
    fixture.assert_vault_idle_invariant(SUB_VAULT_ID).await;
}

/// Burning the LAST vault share while a loan is still
/// deployed must be rejected. Otherwise the final-burn reconciliation
/// would zero `total_principal_atoms` while `deployed_principal_atoms`
/// is still non-zero — orphaning real atoms and breaking the invariant
/// `total_principal ≥ deployed + encumbered`.
#[tokio::test]
async fn last_share_burn_rejected_while_loan_deployed() {
    let fixture = MarketFixture::new().await;
    let (_admin, depositor, _curator, _borrower, _borrower_usdc) =
        setup_through_promote(&fixture).await;

    // A loan is deployed (setup_through_promote cranks the match).
    let sub_vault = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault.deployed_principal_atoms, PRINCIPAL_ATOMS,
        "test invariant: a loan must be deployed before this check"
    );
    // Genesis 1:1 mint — the sole depositor holds every share.
    assert_eq!(
        sub_vault.total_shares,
        (VAULT_DEPOSIT_ATOMS - 1) as u128,
        "test invariant: depositor holds the entire share supply"
    );

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());

    // Attempt to burn ALL shares — the last-share burn. Must reject
    // with the exact VaultInsufficientIdleAtoms variant.
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_withdraw(
            &depositor,
            depositor_token,
            SUB_VAULT_ID,
            sub_vault.total_shares,
        )
        .await;
    crate::assert_custom_error!(
        result,
        ydelta::program::YdeltaError::VaultInsufficientIdleAtoms
    );

    // The sub_vault state must be untouched — the rejection reverts the tx.
    let sub_vault_after = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault_after.total_shares, sub_vault.total_shares,
        "rejected last-share burn must not mutate total_shares",
    );
    assert_eq!(
        sub_vault_after.total_principal_atoms, sub_vault.total_principal_atoms,
        "rejected last-share burn must not orphan deployed principal",
    );

    // A PARTIAL withdraw within the sub_vault's idle still works — only the
    // LAST share is gated. Idle = total_principal − deployed = ~99M atoms.
    fixture.refresh_blockhash().await;
    let partial = fixture
        .global_vault_withdraw(&depositor, depositor_token, SUB_VAULT_ID, 1_000_000_u128)
        .await;
    assert!(
        partial.is_ok(),
        "a partial withdraw within idle must still succeed; got {:?}",
        partial,
    );
}

/// Under the repay/claim split there is NO maturity time gate on claim
/// (it's a stateless seat sweeper). This test pins that the close happens
/// at repay time regardless of where we are in the loan's term — early
/// repay closes the PDA immediately, and a subsequent claim sweeps the
/// seat shares to the vault.
#[tokio::test]
async fn claim_allowed_pre_maturity_after_early_repay() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    // Snapshot matures_at BEFORE repay — under the repay/claim split,
    // full repay closes the PDA.
    let matures_at = fixture.read_loan(0).await.matures_at_unix;

    // Repay in full WITHOUT advancing to maturity. The repay tx is the
    // close event now; loan PDA is gone immediately, well inside the term.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    assert!(
        fixture.loan_account_is_closed(0).await,
        "early full repay must close the loan PDA immediately, regardless of term position",
    );

    // Pin the clock PRE-maturity to prove there is no time gate on claim.
    fixture.set_clock_unix_timestamp(matures_at - 100).await;
    fixture.refresh_blockhash().await;

    // Claim sweeps the seat shares to the vault. Permissionless caller.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, SUB_VAULT_ID)
        .await
        .unwrap();
}

/// Under the repay/claim split, claim is a stateless seat sweeper. It
/// no longer reads any loan PDA, has no outstanding-balance gate, and
/// has no time gate. Calling it when no atoms have accumulated on the
/// seat must be a NO-OP (returns Ok, no state change) — this lets keeper
/// bots poll the ix without worrying about specific timing.
///
/// Pre-fix-equivalent: the old ix rejected with InvalidArgument when
/// outstanding > 0. That gate is now meaningless (the loan PDA isn't
/// even passed in).
#[tokio::test]
async fn claim_with_empty_seat_is_noop() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, _borrower, _borrower_usdc) =
        setup_through_promote(&fixture).await;

    // No repay — borrower hasn't paid back, so no shares accumulated on
    // the lender seat. SubVault state pre-claim:
    let sub_vault_pre = fixture.read_sub_vault(SUB_VAULT_ID).await;

    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    // Must succeed (no-op), not error.
    fixture
        .claim_repayment_for_sub_vault(&stranger, SUB_VAULT_ID)
        .await
        .expect("claim with empty seat must be a no-op, not error");

    // SubVault state untouched.
    let sub_vault_post = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault_post.deployed_principal_atoms, sub_vault_pre.deployed_principal_atoms,
        "no-op claim must not change deployed_principal_atoms",
    );
    assert_eq!(
        sub_vault_post.pending_claim_atoms, sub_vault_pre.pending_claim_atoms,
        "no-op claim must not change pending_claim_atoms",
    );
    assert_eq!(
        sub_vault_post.total_principal_atoms, sub_vault_pre.total_principal_atoms,
        "no-op claim must not change total_principal_atoms",
    );
}

/// Vault yield realization for a depositor: deposit X atoms, run a loan
/// to maturity + repay + claim_repayment_for_sub_vault, then withdraw
/// all of the depositor's shares. Asserts the depositor receives MORE
/// atoms than they deposited (lender interest from the matched loan
/// raised the share-price), AND the on-chain `total_assets_atoms` grew
/// by at least the expected lender interest.
#[tokio::test]
async fn vault_depositor_share_price_grows_after_repaid_loan() {
    let fixture = MarketFixture::new().await;
    let (_admin, depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    // Snapshot pre-claim sub_vault state. Depositor went in at genesis
    // share-price = 1.0 (1 share = 1 atom), so their deposit minted
    // exactly VAULT_DEPOSIT_ATOMS shares.
    let sub_vault_pre = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault_pre.total_shares,
        (VAULT_DEPOSIT_ATOMS - 1) as u128,
        "test invariant: genesis SP=1.0, shares == atoms after first deposit"
    );

    // Walk the test clock to maturity so the borrower's repay triggers
    // a full term's `accrue_loan` — otherwise the share-price gain is
    // sub-atom and the round-trip can return slightly less than the
    // deposit due to marginfi share-floor rounding.
    // Snapshot matures_at BEFORE repay (PDA closes during repay under
    // the repay/claim split).
    let matures_at = fixture.read_loan(0).await.matures_at_unix;
    fixture.set_clock_unix_timestamp(matures_at).await;
    fixture.refresh_blockhash().await;

    // Borrower repays full → closes loan PDA, applies sub-vault
    // decrements + total_principal/total_assets reconciliation.
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Claim is now a stateless seat sweep (no time gate). Stranger
    // cranks. This moves the atoms from lender_marginfi_account to the
    // vault's integration_account so depositors can withdraw them.
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, SUB_VAULT_ID)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // SubVault NAV: total_assets must have grown by ≥ lender interest.
    // Expected lender interest (simple, single-segment):
    //   principal × VAULT_RATE × term / (10000 × SECONDS_PER_YEAR)
    //   = 1_000_000 × 500 × 2_592_000 / (10_000 × 31_536_000)
    //   ≈ 4_109 atoms.
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let expected_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128
            * (VAULT_RATE_BPS + mainnet::USDC_LIVE_LENDING_APR_BPS) as u128
            * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    assert!(
        expected_lender_interest > 1_000,
        "test parameters too small to observe yield"
    );

    let sub_vault_post_claim = fixture.read_sub_vault(SUB_VAULT_ID).await;
    // total_principal_atoms now carries BOTH:
    //   - realized lender interest from the matched loan
    //   - physically accrued supply yield on the idle pool
    //
    // The realized loan interest must still be present inside that gain.
    let total_principal_gain = sub_vault_post_claim
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
        sub_vault_post_claim.total_shares,
        (VAULT_DEPOSIT_ATOMS - 1) as u128,
        "total_shares must not change between deposit and yield realization"
    );

    // Share price grew strictly: total_assets / total_shares > 1.0.
    // Since total_shares == VAULT_DEPOSIT_ATOMS (1:1 at genesis), this
    // reduces to total_assets > VAULT_DEPOSIT_ATOMS.
    assert!(
        sub_vault_post_claim.total_assets_atoms > VAULT_DEPOSIT_ATOMS,
        "vault share price must have grown strictly above 1.0 (assets={} vs shares={})",
        sub_vault_post_claim.total_assets_atoms,
        sub_vault_post_claim.total_shares
    );

    // Full-shares withdrawal must now succeed: idle-side supply yield is
    // part of the sub_vault's withdrawable basis, not just its displayed
    // NAV.
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    let balance_pre = fixture.token_balance(depositor_token).await;
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(
            &depositor,
            depositor_token,
            SUB_VAULT_ID,
            sub_vault_post_claim.total_shares,
        )
        .await
        .unwrap();
    let balance_post = fixture.token_balance(depositor_token).await;
    let received = balance_post.saturating_sub(balance_pre);
    assert!(
        received > VAULT_DEPOSIT_ATOMS,
        "full-shares withdraw should redeem the accrued yield too (received {}, deposit {})",
        received,
        VAULT_DEPOSIT_ATOMS
    );
}

/// At loan close `claim_repayment_for_sub_vault` must reconcile
/// `total_assets_atoms` to REALIZED loan economics.
///
/// While the loan is open, `accrue_sub_vault` continuously credits an
/// ESTIMATED loan yield into `total_assets_atoms`; the realized lender
/// interest only lands in `total_principal_atoms` at claim. Supply yield
/// on idle atoms is a separate stream and is stripped out below via the
/// cumulative supply index. After removing that supply component, the
/// claim true-up (`total_assets += realized − estimated`) must reconcile
/// the loan-rate component of both `total_assets` and `total_principal`
/// back to realized cash. Without it, `total_assets` carries the stale
/// estimate while `total_principal` carries the realized figure, and
/// depositors exiting between default and claim redeem phantom yield.
#[tokio::test]
async fn claim_reconciles_total_assets_to_realized_interest() {
    let fixture = MarketFixture::new().await;
    let (_admin, _depositor, _curator, borrower, borrower_usdc) =
        setup_through_promote(&fixture).await;

    let loan_post_promote = fixture.read_loan(0).await;
    fixture
        .set_clock_unix_timestamp(loan_post_promote.matures_at_unix)
        .await;
    fixture.refresh_blockhash().await;
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Claim a full extra term past maturity: the loan's weighted-rate
    // contribution lingers in `total_weighted_net_rate_bps` until the
    // claim unwinds it, so `accrue_sub_vault` over-credits the
    // ESTIMATED loan yield (≈ 2 terms' worth) into `total_assets`. The
    // borrower only ever owed ONE term. The claim true-up must reconcile
    // `total_assets` back to the realized one-term figure.
    let claim_at = loan_post_promote.matures_at_unix + TERM_SECONDS as i64;
    fixture.set_clock_unix_timestamp(claim_at).await;
    fixture.refresh_blockhash().await;
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, 1)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let realized_interest: u128 =
        PRINCIPAL_ATOMS as u128
            * (VAULT_RATE_BPS + mainnet::USDC_LIVE_LENDING_APR_BPS) as u128
            * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);

    let sub_vault_post = fixture.read_sub_vault(SUB_VAULT_ID).await;

    // total_principal grew from two streams:
    //   - supply yield on idle atoms
    //   - realized lender interest from the loan close
    //
    // Strip the supply-yield component to isolate the loan-close effect.
    let principal_gain = sub_vault_post
        .total_principal_atoms
        .saturating_sub(VAULT_DEPOSIT_ATOMS);
    const INDEX_SCALE: u128 = 1u128 << 48;
    let supply_yield_atoms: u128 =
        sub_vault_post.cumulative_supply_yield_index_scaled * sub_vault_post.total_shares / INDEX_SCALE;
    let principal_loan_component: i128 = principal_gain as i128 - supply_yield_atoms as i128;
    assert!(
        (principal_loan_component - realized_interest as i128).abs() <= 16,
        "loan component of total_principal growth ({}) must match realized interest (~{}) \
         after removing supply yield ({})",
        principal_loan_component,
        realized_interest,
        supply_yield_atoms,
    );

    // `total_assets_atoms` grew from two independent streams:
    //   - supply yield (marginfi share-value drift on idle atoms)
    //   - loan-rate yield (this loan's contribution)
    // Supply yield is genuine NAV growth and is tracked separately by
    // `cumulative_supply_yield_index_scaled`. The supply-yield atoms
    // credited = index × total_shares / 2^48 (the index accumulates
    // `idle_yield × 2^48 / total_shares`, and total_shares is constant
    // here — no deposits/withdraws). Subtracting the supply-yield
    // component isolates the loan-rate contribution to `total_assets`.
    //
    // After the claim true-up, that loan-rate contribution must equal the
    // REALIZED one-term interest — NOT the ~2-term estimate
    // `accrue_sub_vault` credited while the loan lingered open past
    // maturity. Without reconciliation the loan-rate component would be
    // ~2× realized.
    let supply_yield_atoms: u128 =
        sub_vault_post.cumulative_supply_yield_index_scaled * sub_vault_post.total_shares / INDEX_SCALE;
    let assets_gain: u128 =
        (sub_vault_post.total_assets_atoms as u128).saturating_sub(VAULT_DEPOSIT_ATOMS as u128);
    let loan_rate_component: i128 = assets_gain as i128 - supply_yield_atoms as i128;
    // The loan-rate component of total_assets growth must track realized
    // interest within marginfi share-rounding + per-call index flooring.
    assert!(
        (loan_rate_component - realized_interest as i128).abs() <= 16,
        "loan-rate component of total_assets growth ({}) must be \
         reconciled to realized interest ({}) — not the stale ~2-term \
         estimate (assets_gain {}, supply_yield {})",
        loan_rate_component,
        realized_interest,
        assets_gain,
        supply_yield_atoms,
    );
    // The loan was claimed a full extra term late; the un-reconciled
    // estimate would have been ~2× realized — assert we're well under it.
    assert!(
        (loan_rate_component as u128) < realized_interest + realized_interest / 2,
        "loan-rate component ({}) still carries the stale over-estimate \
         (realized {})",
        loan_rate_component,
        realized_interest,
    );
    // And yield realization is real: total_assets did grow.
    assert!(
        sub_vault_post.total_assets_atoms > VAULT_DEPOSIT_ATOMS,
        "total_assets must reflect realized yield growth",
    );
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
///   3. `claim_repayment_for_sub_vault` sweeps the loan's
///      `accumulated_curator_fee_atoms` onto
///      `SubVault.accumulated_curator_fee_atoms`
///   4. `claim_curator_fee` withdraws those atoms to the curator's
///      USDC ATA — curator's wallet balance strictly grows
#[tokio::test]
async fn curator_accrues_and_claims_fee_end_to_end() {
    let fixture = MarketFixture::new().await;

    // ── v1 D3b: curator_fee_bps = 1000 (10%) is stamped on the Pool
    //    sub-vault at creation; promotion snapshots it onto the loan. ──
    let (_admin, _depositor, curator, borrower, borrower_usdc) =
        setup_through_promote_with_fee(&fixture, 1_000).await;

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

    // Borrower repays — accrue_loan runs at the maturity timestamp and
    // closes the loan PDA, applying the per-loan curator fee
    // accumulator directly to the sub_vault (per repay/claim split).
    fixture
        .repay(&borrower, 0, borrower_usdc, 0, /*full_repay=*/ true)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // (2) SubVault holds the curator slice — applied at repay-close, NOT
    // at claim time. Computed expectation:
    //   lender_interest = principal × VAULT_RATE × term / (10_000 × Y)
    //   curator_take    = lender_interest × 1000 / 10_000
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let expected_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128
            * (VAULT_RATE_BPS + mainnet::USDC_LIVE_LENDING_APR_BPS) as u128
            * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_curator_take: u128 = expected_lender_interest * 1_000 / 10_000;
    assert!(
        expected_curator_take > 50,
        "test parameters too small to observe curator fee ({} atoms)",
        expected_curator_take
    );
    let sub_vault_post_repay = fixture.read_sub_vault(SUB_VAULT_ID).await;
    let sub_vault_curator_drift = (sub_vault_post_repay.accumulated_curator_fee_atoms as i128
        - expected_curator_take as i128)
        .abs();
    assert!(
        sub_vault_curator_drift <= 2,
        "SubVault.accumulated_curator_fee_atoms drift > 2 atoms after \
         repay-close (got {}, expected {})",
        sub_vault_post_repay.accumulated_curator_fee_atoms,
        expected_curator_take
    );

    // ── Claim sweeps the seat to vault. No time gate; no loan PDA. ──
    let stranger = fixture.create_trader().await;
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault(&stranger, SUB_VAULT_ID)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // (3) Curator fee accumulator unchanged across the claim (already
    // applied at repay) — claim is purely a token movement.
    let sub_vault_post_claim = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert_eq!(
        sub_vault_post_claim.accumulated_curator_fee_atoms,
        sub_vault_post_repay.accumulated_curator_fee_atoms,
        "claim is a pure seat sweep; must NOT mutate accumulated_curator_fee_atoms",
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
        .claim_curator_fee(&curator, curator_token, SUB_VAULT_ID)
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

    // SubVault accumulator after claim. `claim_curator_fee` decrements the
    // accumulator by the marginfi-reported `actual_atoms`
    // (`accumulator' = fee_atoms.saturating_sub(actual_atoms)`), NOT a
    // blanket zero. When marginfi's withdraw returns the full requested
    // amount the accumulator lands at 0; when the ±1-atom drift gate
    // makes marginfi return one atom short, that 1-atom un-realised
    // remainder stays on the accumulator (claimable on a later call)
    // rather than being silently lost. Allow that sub-atom residual.
    let sub_vault_final = fixture.read_sub_vault(SUB_VAULT_ID).await;
    assert!(
        sub_vault_final.accumulated_curator_fee_atoms <= 1,
        "claim_curator_fee must drain the accumulator to <= a 1-atom \
         marginfi-drift residual; got {}",
        sub_vault_final.accumulated_curator_fee_atoms
    );
}

/// Test A — sub-vault lending in TWO markets earns the SUM of both
/// markets' lender interests. Stands up a second `(USDC, wSOL)` market
/// on the same fixture, has the sub_vault claim seats in both, runs
/// matching-loan lifecycles in both, and asserts the sub_vault's
/// `total_principal_atoms` grew by approximately the sum of both
/// markets' lender_net_interest realisations.
///
/// Note: this test is heavier than single-market tests (more accounts
/// loaded into program-test, more transactions). May hit macOS's
/// default 256-fd cap; bump via `sudo launchctl limit maxfiles 65536
/// 65536` if you see "Too many open files".
#[tokio::test]
async fn sub_vault_earns_yield_from_two_markets() {
    let fixture = MarketFixture::new().await;

    // ── Market 1 lifecycle via the existing setup helper. After this
    // returns, sub_vault_post.deployed_principal_atoms == PRINCIPAL_ATOMS,
    // a single matched loan is open, borrower funded.
    let (admin, _depositor, curator, borrower1, borrower1_usdc) =
        setup_through_promote(&fixture).await;
    let _market1_pk = fixture.market.pubkey();

    // ── Stand up market 2 (same USDC/wSOL pair, separate orderbook). ──
    fixture.refresh_blockhash().await;
    fixture.create_second_market().await;
    let market2_pk = fixture.second_market_pubkey();

    // Curator places an ask in market 2. No claim-seat step and no
    // market cap — the quote-only model auto-creates the vault seat on
    // the first place_order_for_sub_vault, and a sub_vault may quote
    // any market sharing the vault's mint.
    let _ = admin;
    fixture
        .place_order_for_sub_vault_in_market(
            &curator,
            SUB_VAULT_ID,
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
        .crank_matched_loan_for_sub_vault_in_market(market2_pk, /*sequence=*/ 0)
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
        .claim_repayment_for_sub_vault(&stranger, 1)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .claim_repayment_for_sub_vault_in_market(&stranger, market2_pk, 1)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // ── Assertion: sub_vault.total_principal grew by ≈ 2× lender interest
    //    of one loan (both loans had identical params — same principal,
    //    same rate, same term). One-loan baseline is exercised by the
    //    existing single-market test above.
    let sub_vault_final = fixture.read_sub_vault(SUB_VAULT_ID).await;
    const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;
    let single_loan_lender_interest: u128 =
        PRINCIPAL_ATOMS as u128
            * (VAULT_RATE_BPS + mainnet::USDC_LIVE_LENDING_APR_BPS) as u128
            * TERM_SECONDS as u128
            / (10_000u128 * SECONDS_PER_YEAR);
    let expected_two_loan_yield: u128 = 2 * single_loan_lender_interest;
    let observed_gain: u128 = sub_vault_final
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
        sub_vault_final.deployed_principal_atoms == 0,
        "all loans repaid → deployed_principal must be 0"
    );
    assert_eq!(
        sub_vault_final.encumbered_in_orders_atoms, 0,
        "encumbered_in_orders must zero out after both loans claim"
    );
    // Vault-idle invariant post-claim of both loans.
    fixture.assert_vault_idle_invariant(SUB_VAULT_ID).await;
}
