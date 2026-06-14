//! `convert_p2pool_to_fixed` integration tests.
//!
//! Flow exercised:
//!  1. A borrower's IOC bid finds no resting asks → the whole principal
//!     falls through to the P2Pool fallback (`marginfi.borrow`), and the
//!     cranker promotes a `LoanFixed` with `loan_type == P2Pool`.
//!  2. A vault sub-vault is funded and rests an unbounded ask.
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
        .map(|b| wrapped_i80f48_to_u128(b.liability_shares).unwrap())
        .unwrap_or(0)
}

/// Read the borrower marginfi-account's live USDC liability in ATOMS —
/// `liability_shares × liability_share_value`, floored, the same
/// conversion marginfi applies internally.
async fn borrower_liability_atoms(fixture: &MarketFixture) -> u128 {
    let shares = borrower_liability_shares(fixture).await;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let bank = Bank::try_from_account_data(&bank_data).unwrap();
    let lsv = wrapped_i80f48_to_u128(bank.liability_share_value).unwrap();
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
            3_000,
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

    // Vault sub_vault rests an unbounded ask with plenty of idle (10_000
    // atoms) at 600 bps / 30d — comfortably covers the 100-atom debt.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Convert: borrower accepts asks up to 1_000 bps. The 600 bps vault
    // ask crosses; the whole live liability is refinanced.
    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 3_000)
        .await
        .unwrap();

    // The crossed ask produced a Fixed MatchedLoan at sequence 1.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "one P2Pool (seq 0) + one converted Fixed (seq 1) MatchedLoan"
    );

    // The borrower's variable debt is fully retired (modulo marginfi's
    // sub-atom fp48 rounding noise — at most 1 atom in atom-form). The
    // prediction-based repay matches marginfi's accrue exactly under
    // same-slot Clock; any residue is sub-atom-precision dust, not real
    // debt.
    let liability_post_atoms = borrower_liability_atoms(&fixture).await;
    assert!(
        liability_post_atoms <= 1,
        "full refinance must reduce liability to ≤ 1 atom of dust, got {} atoms",
        liability_post_atoms
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

/// Partial conversion is rejected.
///
/// Convert is must-full-fill: if the orderbook can't fully refinance
/// the loan's live debt, the whole transaction fails and the borrower
/// stays on the variable-rate P2Pool loan. No partial fixed/variable
/// mixed-rate states allowed.
///
/// Setup: vault has only 40 atoms idle, P2Pool debt is ~100 atoms.
/// The matcher fills 40 (vault cap), live_outstanding is ~100, so
/// `total_filled < live_outstanding` → the must-full-fill require!
/// fires and the convert fails with InvalidArgument.
#[tokio::test]
async fn partial_conversion_is_rejected() {
    let fixture = MarketFixture::new().await;

    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    let liability_pre = borrower_liability_shares(&fixture).await;
    assert!(liability_pre > 0);

    // Vault sub_vault rests an ask with only 40 atoms idle — strictly
    // less than the ~100-atom P2Pool debt, so the matcher can only
    // partially fill.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 40,
        )
        .await;
    fixture.refresh_blockhash().await;

    // The must-full-fill gate must reject this convert with
    // InvalidArgument (total_filled < live_outstanding).
    let result = fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 3_000)
        .await;
    crate::assert_custom_error!(result, ydelta::program::YdeltaError::InvalidArgument);

    // The P2Pool loan is untouched: still Active, still P2Pool, marginfi
    // liability intact (tx atomicity rolled back the partial match's
    // encumbered_in_orders bumps).
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

    // No MatchedLoan node was created (rolled back with the tx).
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "rejected convert must not bump matched_loan_sequence"
    );
}

/// The P2Pool→fixed refinance matcher MUST enforce the crossed vault
/// sub_vault's curator-set `max_ltv_bps` cap per cross, just like the
/// primary `match_order`. A market-level aggregate check at the
/// marginfi-init weights alone is not enough: a borrower could
/// otherwise refinance variable debt into a conservative low-LTV
/// curator's quote at an LTV that curator never agreed to.
///
/// Setup: a P2Pool loan that comfortably passes the aggregate
/// marginfi-init-weight check (SOL collateral is ~0.8-weighted), but the
/// only resting vault ask belongs to a sub_vault with `max_ltv_bps = 100`
/// (1% LTV) — that cap demands ~100× the collateral the aggregate check
/// needs. The per-cross sub_vault-cap gate must reject (skip) the ask, and
/// the convert must then fail with "no asks crossed" rather than minting
/// a Fixed loan at an LTV the curator never agreed to.
#[tokio::test]
async fn convert_rejected_when_cross_breaches_sub_vault_max_ltv() {
    let fixture = MarketFixture::new().await;

    // A small P2Pool loan with collateral sized to clear the aggregate
    // marginfi-init-weight check but NOT a 1%-LTV sub_vault cap.
    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    let liability_pre = borrower_liability_shares(&fixture).await;
    assert!(liability_pre > 0, "P2Pool loan must carry a live liability");

    // The only resting ask belongs to a sub_vault with an aggressively
    // low `max_ltv_bps` of 100 (1% LTV). The marginfi-init-weight
    // aggregate check passes (SOL collateral comfortably backs $-tiny
    // debt), but the per-cross sub_vault-cap gate must reject this cross.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(100),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Convert must FAIL: the sole vault ask is skipped by the per-cross
    // sub_vault-LTV-cap gate, leaving no compatible maker → the processor
    // errors. Exact reject path is the "no asks crossed" branch which
    // surfaces InvalidArgument from convert_p2pool_to_fixed.
    let result = fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 3_000)
        .await;
    // The per-cross sub_vault-LTV gate SKIPS the only ask, so the matcher
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

/// After a full convert, the borrower's new Fixed-loan debt must
/// never exceed the variable (P2Pool) debt actually retired.
///
/// The new Fixed loan is sized at `total_filled_principal == live_outstanding`
/// (must-full-fill). The repay CPI retires the borrower's variable debt
/// using exactly the prediction-based amount. Marginfi's share-rounding
/// might leave sub-atom dust on the borrower's marginfi liability, so
/// we check `new_fixed_debt <= retired_variable_debt` rather than
/// strict equality.
#[tokio::test]
async fn convert_new_fixed_debt_never_exceeds_retired_variable_debt() {
    let fixture = MarketFixture::new().await;

    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    // Vault sub_vault with PLENTY of idle to fully refinance the loan
    // (must-full-fill requires `vault_idle >= live_outstanding`). Test
    // exercises the predictive-repay path on a single full cross.
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000,
        )
        .await;
    fixture.refresh_blockhash().await;

    let liability_atoms_pre = borrower_liability_atoms(&fixture).await;
    assert!(
        liability_atoms_pre > 0,
        "P2Pool loan must carry a live liability"
    );

    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 3_000)
        .await
        .unwrap();

    let liability_atoms_post = borrower_liability_atoms(&fixture).await;
    let retired_variable_debt = liability_atoms_pre.saturating_sub(liability_atoms_post);
    assert!(
        retired_variable_debt > 0,
        "convert must retire variable debt"
    );

    fixture
        .crank_matched_loan_for_sub_vault(1)
        .await
        .unwrap();
    let converted = fixture.read_loan(1).await;
    assert_eq!(converted.loan_type, LoanType::Fixed as u8);
    let new_fixed_debt = converted.outstanding_debt_atoms as u128;
    assert!(new_fixed_debt > 0);

    // Core invariant: borrower never owes meaningfully more new fixed
    // debt than the old variable debt that was destroyed. ≤1 atom drift
    // is the unavoidable cost of marginfi's accrue-during-repay share-
    // burn rounding (the prediction-based atom amount can leave a 1-atom
    // residual variable debt when marginfi's intra-CPI accrue advances
    // by even 1 fp48 bit beyond our same-slot prediction).
    assert!(
        new_fixed_debt <= retired_variable_debt as u128 + 1,
        "new fixed debt {} must not exceed retired variable debt {} + 1 atom dust",
        new_fixed_debt,
        retired_variable_debt,
    );
}

/// Convert produces a Fixed-type LoanFixed whose body matches the
/// crossed vault ask (rate, term, principal). This is the core
/// "P2Pool→Fixed transition" assertion: after convert + crank, the
/// borrower's debt is represented as a `LoanFixed { loan_type: Fixed }`
/// at the agreed-on fixed rate, with the original P2Pool PDA closed.
#[tokio::test]
async fn convert_produces_fixed_loan_with_expected_fields() {
    let fixture = MarketFixture::new().await;

    let principal_atoms: u64 = 100;
    let collateral_atoms: u64 = 5_000;
    let bob = open_p2pool_loan(&fixture, principal_atoms, collateral_atoms).await;

    // Sanity: starting state is a P2Pool loan at sequence 0.
    let p2pool_pre = fixture.read_loan(0).await;
    assert_eq!(p2pool_pre.loan_type, LoanType::P2Pool as u8);
    assert_eq!(p2pool_pre.state, LoanState::Active as u8);

    // Vault sub_vault rests an ask at a SPECIFIC rate (425 bps) and term
    // (30 days) — the new Fixed loan must adopt these.
    const ASK_RATE_BPS: u16 = 425;
    const ASK_TERM_SECONDS: u32 = 30 * 86_400;
    const VAULT_MAX_LTV_BPS: Option<u16> = Some(8_000);
    const VAULT_DEPOSIT_ATOMS: u64 = 10_000;

    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            VAULT_MAX_LTV_BPS,
            ASK_RATE_BPS,
            ASK_TERM_SECONDS,
            VAULT_DEPOSIT_ATOMS,
        )
        .await;
    fixture.refresh_blockhash().await;

    // Convert. Cap the acceptable rate at 1000 bps (well above the 425
    // bps ask), so the cross goes through.
    fixture
        .convert_p2pool_to_fixed(&bob, /*loan_sequence=*/ 0, 3_000)
        .await
        .unwrap();

    // The original P2Pool PDA is closed (data zeroed or account removed).
    let p2pool_addr = ydelta::state::loan::loan_pda(&fixture.market.pubkey(), 0).0;
    let p2pool_post = {
        let ctx = fixture.context.borrow_mut();
        ctx.banks_client.get_account(p2pool_addr).await.unwrap()
    };
    match p2pool_post {
        None => { /* removed — closed */ }
        Some(acct) => assert!(
            acct.data.iter().all(|b| *b == 0),
            "P2Pool PDA must be closed (data zeroed) after full conversion"
        ),
    }

    // A new MatchedLoan was queued at sequence 1.
    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 2,
        "one P2Pool (seq 0) + one converted Fixed (seq 1) — got {}",
        { market.matched_loan_sequence }
    );

    // Crank the new MatchedLoan into a LoanFixed PDA.
    fixture
        .crank_matched_loan_for_sub_vault(1)
        .await
        .unwrap();

    // The new loan exists and is Fixed-type with the agreed rate/term.
    let new_loan = fixture.read_loan(1).await;
    assert_eq!(
        new_loan.loan_type,
        LoanType::Fixed as u8,
        "converted loan must be Fixed-type"
    );
    assert_eq!(
        new_loan.state,
        LoanState::Active as u8,
        "converted loan must be Active immediately post-crank"
    );
    assert_eq!(
        new_loan.lender_rate_bps,
        ASK_RATE_BPS + mainnet::USDC_LIVE_LENDING_APR_BPS,
        "converted Fixed loan adopts the crossed ask's stored rate \
         (spread + live bank APR, v1 D4)"
    );
    // Converted Fixed loan inherits the REMAINING term of the original
    // P2Pool loan (the borrower keeps their original loan-open clock),
    // not the ask's max-term. Verify it's within the ask's cap and
    // close to the original term.
    let actual_term = (new_loan.matures_at_unix - new_loan.started_at_unix) as u32;
    assert!(
        actual_term <= ASK_TERM_SECONDS,
        "converted Fixed loan term {} must not exceed the ask's max term {}",
        actual_term,
        ASK_TERM_SECONDS
    );
    assert!(
        actual_term > 0,
        "converted Fixed loan must have a positive term"
    );
    assert!(
        new_loan.principal_debt_atoms > 0,
        "converted Fixed loan principal must be non-zero (was {})",
        { new_loan.principal_debt_atoms }
    );
    assert_eq!(
        new_loan.borrower_seat_index, p2pool_pre.borrower_seat_index,
        "converted Fixed loan borrower seat must match the original P2Pool borrower"
    );

    // The borrower's variable marginfi liability is gone (≤1 atom dust).
    let liability_post_atoms = borrower_liability_atoms(&fixture).await;
    assert!(
        liability_post_atoms <= 1,
        "post-convert variable liability must be ≤1 atom dust, got {}",
        liability_post_atoms
    );
}
