//! `convert_p2pool_to_fixed` integration tests.
//!
//! Flow exercised:
//!  1. A borrower's IOC bid finds no resting asks → the whole principal
//!     falls through to the P2Pool fallback (`marginfi.borrow`), and the
//!     cranker promotes a `LoanFixed` with `loan_type == P2Pool`.
//!  2. A vault risk profile is funded and rests an unbounded ask.
//!  3. `convert_p2pool_to_fixed` crosses that vault ask, withdrawing
//!     principal from the vault, repaying the borrower's marginfi
//!     liability, and queuing a Fixed `MatchedLoan`.
//!
//! Asserts the P2Pool PDA is closed only on a full conversion that
//! drives the live marginfi liability to exactly zero.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::{Bank, MarginfiAccount};
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::loan::{LoanState, LoanType};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Read the borrower marginfi-account's live USDC liability shares.
async fn borrower_liability_shares(fixture: &MarketFixture) -> u128 {
    let data = fixture
        .account_data(fixture.borrower_marginfi_account_pubkey())
        .await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    mfi.find_balance(&mainnet::usdc_bank())
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares))
        .unwrap_or(0)
}

/// Read the borrower marginfi-account's live USDC liability in ATOMS —
/// `liability_shares × liability_share_value`, floored, the same
/// conversion marginfi applies internally.
async fn borrower_liability_atoms(fixture: &MarketFixture) -> u128 {
    let shares = borrower_liability_shares(fixture).await;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let bank = Bank::try_from_account_data(&bank_data).unwrap();
    let lsv = wrapped_i80f48_to_u128(bank.liability_share_value);
    ydelta::math::from_scaled_floor(ydelta::math::mul_scale(shares, lsv).unwrap())
}

/// Borrow `principal` atoms via the P2Pool fallback and crank it into a
/// `LoanFixed`. Returns the borrower keypair.
async fn open_p2pool_loan(
    fixture: &MarketFixture,
    principal_atoms: u64,
    collateral_atoms: u64,
) -> solana_sdk::signature::Keypair {
    // (Quote-only: lenders fund via vaults, not seat debt-deposits — the
    // P2Pool borrow's deposit-back CPI inits the lender-side balance slot.)
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;

    // Borrower deposits wSOL collateral onto the borrower-side marginfi
    // account so the borrow CPI's solvency check passes. Tiny size to
    // dodge the mainnet liquidity-vault u64 overflow.
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), 100_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, 10_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // No asks on book → entire principal becomes the P2Pool residual.
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

    // Crank the P2Pool MatchedLoan → LoanFixed PDA (sequence 0).
    fixture.crank_matched_loan(0).await.unwrap();
    fixture.refresh_blockhash().await;

    let loan = fixture.read_loan(0).await;
    assert_eq!(loan.loan_type, LoanType::P2Pool as u8);
    bob
}

/// Full conversion: a vault ask with ample idle absorbs the entire
/// P2Pool liability. The P2Pool PDA must be closed (data zeroed) and the
/// borrower's marginfi liability driven to exactly zero.
///
/// A *full* P2Pool→Fixed refinance retires the entire variable liability.
/// The per-market lender deposit-back (≈ the borrowed principal) is ~1
/// atom short of the live liability after marginfi's share-floor, so the
/// repay-rounding shortfall is topped up from the crossed vault's idle
/// (the new fixed lender funds the remainder) — see the funding block in
/// `convert_p2pool_to_fixed`.
#[tokio::test]
async fn full_conversion_closes_p2pool_pda() {
    let fixture = MarketFixture::new().await;

    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    let liability_pre = borrower_liability_shares(&fixture).await;
    assert!(liability_pre > 0, "P2Pool loan must carry a live liability");

    // Vault profile rests an unbounded ask with plenty of idle (10_000
    // atoms) at 600 bps / 30d — comfortably covers the 100-atom debt.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Convert: borrower accepts asks up to 1_000 bps. The 600 bps vault
    // ask crosses; the whole live liability is refinanced.
    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 1_000)
        .await
        .unwrap();

    // The crossed ask produced a Fixed MatchedLoan at sequence 1.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "one P2Pool (seq 0) + one converted Fixed (seq 1) MatchedLoan"
    );

    // The whole marginfi liability has been retired. A FULL refinance
    // uses a `repay_all` CPI, so the live liability lands at exactly
    // zero — the borrower's variable debt is cleanly destroyed and
    // never under-retired by share-floor dust.
    let liability_post = borrower_liability_shares(&fixture).await;
    assert_eq!(
        liability_post, 0,
        "a full refinance must drive the live marginfi liability to zero"
    );
    // Zero residual liability → the P2Pool PDA is closed. The closed
    // account is either removed entirely or left zeroed; both are a
    // valid "closed" outcome.
    let (loan_addr, _) = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), 0);
    let loan_account = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(loan_addr).await.unwrap()
    };
    match loan_account {
        None => { /* account removed — closed */ }
        Some(acct) => assert!(
            acct.data.iter().all(|b| *b == 0),
            "zero residual liability must close the P2Pool loan PDA (data zeroed)"
        ),
    }
}

/// Partial conversion: the vault profile has less idle than the P2Pool
/// debt, so only part of the liability is refinanced. The P2Pool PDA
/// must stay open and `Active` with a non-zero residual liability.
#[tokio::test]
async fn partial_conversion_keeps_p2pool_active() {
    let fixture = MarketFixture::new().await;

    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    let liability_pre = borrower_liability_shares(&fixture).await;
    assert!(liability_pre > 0);

    // Vault profile rests an ask with only 40 atoms idle — less than the
    // ~100-atom P2Pool debt, so the conversion is necessarily partial.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 40,
        )
        .await;
    fixture.refresh_blockhash().await;

    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 1_000)
        .await
        .unwrap();

    // P2Pool PDA still alive: discriminator intact, loan Active.
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.state,
        LoanState::Active as u8,
        "partial conversion must leave the P2Pool loan Active"
    );
    assert_eq!(
        loan.loan_type,
        LoanType::P2Pool as u8,
        "partial conversion keeps loan_type == P2Pool"
    );

    // A residual marginfi liability remains.
    let liability_post = borrower_liability_shares(&fixture).await;
    assert!(
        liability_post > 0,
        "partial conversion must leave a non-zero residual liability"
    );

    // One converted Fixed MatchedLoan was queued.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "one P2Pool (seq 0) + one converted Fixed (seq 1) MatchedLoan"
    );

    // The converted Fixed MatchedLoan, once cranked, carries the
    // ask-rate lender rate. Cross sized at the 40-atom profile idle.
    fixture
        .crank_matched_loan_for_risk_profile(1)
        .await
        .unwrap();
    let converted = fixture.read_loan(1).await;
    assert_eq!(converted.loan_type, LoanType::Fixed as u8);
    assert_eq!(
        converted.lender_rate_bps, 600,
        "converted Fixed loan adopts the crossed ask's rate"
    );
    // Converted principal is capped by the crossed profile's idle minus
    // the matching engine's per-profile marginfi-rounding reserve. The
    // vault was funded with 40 atoms; the marginfi deposit share-floor
    // credits 39 idle, and the matcher reserves
    // `MARGINFI_ROUNDING_RESERVE_ATOMS` on top.
    use ydelta::state::market_helpers::MARGINFI_ROUNDING_RESERVE_ATOMS;
    const VAULT_DEPOSIT_ATOMS: u64 = 40;
    let floored_idle = VAULT_DEPOSIT_ATOMS - 1; // marginfi deposit share-floor
    assert_eq!(
        converted.principal_debt_atoms,
        floored_idle - MARGINFI_ROUNDING_RESERVE_ATOMS,
        "converted principal == floored profile idle minus marginfi-rounding reserve"
    );
    // Conservation must hold on the new Fixed loan at promotion.
    fixture.assert_loan_conservation_holds(1).await;
}

/// The P2Pool→fixed refinance matcher MUST enforce the crossed vault
/// profile's curator-set `max_ltv_bps` cap per cross, just like the
/// primary `match_order`. A market-level aggregate check at the
/// marginfi-init weights alone is not enough: a borrower could
/// otherwise refinance variable debt into a conservative low-LTV
/// curator's quote at an LTV that curator never agreed to.
///
/// Setup: a P2Pool loan that comfortably passes the aggregate
/// marginfi-init-weight check (SOL collateral is ~0.8-weighted), but the
/// only resting vault ask belongs to a profile with `max_ltv_bps = 100`
/// (1% LTV) — that cap demands ~100× the collateral the aggregate check
/// needs. The per-cross profile-cap gate must reject (skip) the ask, and
/// the convert must then fail with "no asks crossed" rather than minting
/// a Fixed loan at an LTV the curator never agreed to.
#[tokio::test]
async fn convert_rejected_when_cross_breaches_profile_max_ltv() {
    let fixture = MarketFixture::new().await;

    // A small P2Pool loan with collateral sized to clear the aggregate
    // marginfi-init-weight check but NOT a 1%-LTV profile cap.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    let liability_pre = borrower_liability_shares(&fixture).await;
    assert!(liability_pre > 0, "P2Pool loan must carry a live liability");

    // The only resting ask belongs to a profile with an aggressively
    // low `max_ltv_bps` of 100 (1% LTV). The marginfi-init-weight
    // aggregate check passes (SOL collateral comfortably backs $-tiny
    // debt), but the per-cross profile-cap gate must reject this cross.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 100,
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Convert must FAIL: the sole vault ask is skipped by the per-cross
    // profile-LTV-cap gate, leaving no compatible maker → the processor
    // errors. Exact reject path is the "no asks crossed" branch which
    // surfaces InvalidArgument from convert_p2pool_to_fixed.
    let result = fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 1_000)
        .await;
    // The per-cross profile-LTV gate SKIPS the only ask, so the matcher
    // crosses nothing and the processor's "no asks crossed" guard fires
    // with InvalidArgument — not just any error.
    crate::assert_custom_error!(result, ydelta::program::YdeltaError::InvalidArgument);
    // Stamping more strictly: the exact error path depends on whether
    // the processor falls back to "no crosses" or the LTV gate fires
    // directly. Both paths leave the P2Pool loan completely untouched.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "rejected convert must not insert a new MatchedLoan"
    );

    // The P2Pool loan is untouched: still Active, still P2Pool, the
    // marginfi liability intact.
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.state,
        LoanState::Active as u8,
        "rejected convert must leave the P2Pool loan Active"
    );
    assert_eq!(loan.loan_type, LoanType::P2Pool as u8);
    let liability_post = borrower_liability_shares(&fixture).await;
    assert_eq!(
        liability_post, liability_pre,
        "rejected convert must not retire any liability"
    );
}

/// After a convert, the borrower's total NEW fixed debt must never
/// exceed the OLD variable (P2Pool) debt actually retired.
///
/// The new Fixed loans are sized from `total_filled_principal`, while the
/// borrower's marginfi P2Pool liability is retired by the repay CPI. The
/// asset-share floor on the lender-side withdraw and the liability-share
/// floor on the repay could each shave an atom, leaving the borrower
/// owing a sub-atom-scale phantom fixed debt the destroyed variable debt
/// never covered. The withdraw/repay rounds up to whole shares (and
/// uses `repay_all` for a full refinance), so the retired variable
/// debt is always `>=` the minted fixed debt.
///
/// Exercises a PARTIAL conversion (vault idle < the P2Pool debt) so the
/// atom-capped partial-repay path with genuine share rounding is hit,
/// not the trivial full `repay_all`.
#[tokio::test]
async fn convert_new_fixed_debt_never_exceeds_retired_variable_debt() {
    let fixture = MarketFixture::new().await;

    // Small principal (the mainnet bank fixture overflows its
    // liquidity-vault u64 on larger P2Pool borrows). The bank's share
    // values are not exactly 1.0, so even small amounts carry a genuine
    // atoms→shares→atoms fractional remainder for the rounding to
    // exercise.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    // Vault profile with LESS idle than the P2Pool debt → the
    // conversion is necessarily partial, exercising the atom-capped
    // partial-repay path.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 40,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Snapshot the borrower's live liability IMMEDIATELY before the
    // convert — marginfi accrues interest continuously, so a snapshot
    // taken at loan-open time would understate the true pre-convert
    // debt.
    let liability_atoms_pre = borrower_liability_atoms(&fixture).await;
    assert!(
        liability_atoms_pre > 0,
        "P2Pool loan must carry a live liability"
    );

    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 1_000)
        .await
        .unwrap();

    // Variable debt actually retired = the drop in the borrower's
    // marginfi liability (in atoms).
    let liability_atoms_post = borrower_liability_atoms(&fixture).await;
    let retired_variable_debt = liability_atoms_pre.saturating_sub(liability_atoms_post);
    assert!(
        retired_variable_debt > 0,
        "convert must retire variable debt"
    );

    // New fixed debt = the converted Fixed loan's outstanding debt,
    // summed across every converted loan. One cross here → sequence 1.
    fixture
        .crank_matched_loan_for_risk_profile(1)
        .await
        .unwrap();
    let converted = fixture.read_loan(1).await;
    assert_eq!(converted.loan_type, LoanType::Fixed as u8);
    let new_fixed_debt = converted.outstanding_debt_atoms as u128;
    assert!(new_fixed_debt > 0);

    // The core invariant: the borrower never ends up owing more
    // fixed debt than the variable debt that was destroyed.
    assert!(
        new_fixed_debt <= retired_variable_debt,
        "new fixed debt {} must not exceed retired variable debt {}",
        new_fixed_debt,
        retired_variable_debt,
    );
}
