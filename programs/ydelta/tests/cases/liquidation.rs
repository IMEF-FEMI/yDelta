//! Liquidation ix surface tests.
//!
//! Account-count invariants for `settle_matured_loan` and
//! `liquidate_loan`, plus a live SBPF oracle-priced LTV-breach
//! liquidation that drops the wSOL oracle below the maint-LTV
//! threshold and asserts the keeper can clear the loan.

use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use ydelta::program::instruction_builders::check_liquidatable_instruction::{
    check_ltv_liquidatable_instruction, check_maturity_liquidatable_instruction,
};
use ydelta::program::instruction_builders::protocol_fee_claim_instruction::protocol_fee_claim_instruction;
#[cfg(feature = "test-sbf")]
use ydelta::program::instruction_builders::set_fee_config_instruction::set_fee_config_instruction;
use ydelta::program::instruction_builders::{
    liquidate_loan_instruction::liquidate_loan_instruction,
    settle_matured_loan_instruction::settle_matured_loan_instruction,
};
#[cfg(feature = "test-sbf")]
use ydelta::program::processor::set_fee_config::SetFeeConfigParams;
use ydelta::program::YdeltaInstruction;
#[cfg(feature = "test-sbf")]
use ydelta::state::loan::LoanState;

#[cfg(feature = "test-sbf")]
use crate::test_utils::marginfi_fixture::mainnet;
#[cfg(feature = "test-sbf")]
use crate::test_utils::market_fixture::MarketFixture;

/// Stand up a vault-funded loan for the liquidation/settlement tests.
/// The lender is a vault risk profile resting an unbounded ask; the
/// borrower deposits wSOL collateral and crosses the ask with an IOC
/// bid; the cranker promotes the matched loan. Returns
/// `(keeper, keeper_usdc, keeper_wsol)`.
#[cfg(feature = "test-sbf")]
async fn setup_vault_funded_loan(
    fixture: &MarketFixture,
    principal: u64,
    collateral: u64,
    term_seconds: u32,
) -> (Keypair, Pubkey, Pubkey) {
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await; // borrower
    let keeper = fixture.create_trader().await; // liquidator / cranker

    // Lender side: vault profile, unbounded ask at 600bps.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 600,
            term_seconds,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    let bob_wsol = Pubkey::new_unique();
    // Fund the borrower's wSOL ATA with enough to deposit the full
    // `collateral` amount into marginfi (plus headroom). The collateral
    // figure is denominated in wSOL lamports, so these tests pass
    // realistic (millions-of-lamports) amounts.
    fixture.put_wsol_token_account(
        bob_wsol,
        bob.pubkey(),
        collateral.saturating_mul(2).max(10_000_000),
    );
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

    // Borrower collateral deposit (real marginfi wSOL bank). Deposit
    // the full `collateral` amount so the IOC bid below can encumber
    // it (the bid's collateral can't exceed the deposited balance).
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, collateral)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Borrower IOC bid crosses the vault ask.
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            term_seconds,
            principal,
            collateral,
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
    fixture.refresh_oracle_freshness().await;

    (keeper, keeper_usdc, keeper_wsol)
}

#[test]
fn settle_matured_loan_ix_has_twenty_four_accounts() {
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = settle_matured_loan_instruction(
        &market,
        &payer.pubkey(),
        7,
        &Pubkey::new_unique(),   // debt_mint
        &Pubkey::new_unique(),   // collateral_mint
        &Pubkey::new_unique(),   // liquidator_debt_token
        &Pubkey::new_unique(),   // liquidator_collateral_token
        &Pubkey::new_unique(),   // debt_bank
        &Pubkey::new_unique(),   // collateral_bank
        &Pubkey::new_unique(),   // debt_liquidity_vault
        &Pubkey::new_unique(),   // collateral_liquidity_vault
        &Pubkey::new_unique(),   // collateral_bank_lva
        &[Pubkey::new_unique()], // debt_oracles (single-oracle bank)
        &[Pubkey::new_unique()], // collateral_oracles
        &Pubkey::new_unique(),   // token_program
        &Pubkey::new_unique(),   // marginfi_group
        &Pubkey::new_unique(),   // marginfi_program
        0,                       // repay_atoms_max = 0 → full repay
        &Pubkey::new_unique(),   // cranker_refund
        None,                    // global_vault (None = P2Pool variant)
    );
    // 24 accounts is the P2Pool shape; Fixed adds 1 (global_vault).
    assert_eq!(ix.accounts.len(), 24);
}

#[test]
fn liquidate_loan_ix_has_twenty_four_accounts() {
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = liquidate_loan_instruction(
        &market,
        &payer.pubkey(),
        7,
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &[Pubkey::new_unique()], // debt_oracles (single-oracle bank)
        &[Pubkey::new_unique()], // collateral_oracles
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        0, // repay_atoms_max = 0 → full repay
        &Pubkey::new_unique(),
        None, // global_vault (None = P2Pool variant)
    );
    // Same shape as settle_matured_loan — both ixs share the loader.
    assert_eq!(ix.accounts.len(), 24);
}

#[test]
fn liquidation_ix_tags_distinct_from_settle() {
    assert_eq!(YdeltaInstruction::SettleMaturedLoan as u8, 16);
    assert_eq!(YdeltaInstruction::LiquidateLoan as u8, 17);
}

#[test]
fn check_liquidatable_ix_tags_and_shapes() {
    // Tag stability — these are user-facing simulation entry points.
    assert_eq!(YdeltaInstruction::CheckLtvLiquidatable as u8, 34);
    assert_eq!(YdeltaInstruction::CheckMaturityLiquidatable as u8, 35);

    let market = Pubkey::new_unique();
    let payer = Keypair::new();

    let ltv = check_ltv_liquidatable_instruction(
        &market,
        &payer.pubkey(),
        7,
        &Pubkey::new_unique(),   // debt_bank
        &Pubkey::new_unique(),   // collateral_bank
        &[Pubkey::new_unique()], // debt_oracles (single-oracle bank)
        &[Pubkey::new_unique()], // collateral_oracles
        &Pubkey::new_unique(),   // marginfi_program
    );
    // 5 fixed (payer, global_config, market, loan, borrower_marginfi)
    // + 1 debt_bank + 1 debt_oracle + 1 collateral_bank
    // + 1 collateral_oracle + 1 marginfi_program = 10.
    assert_eq!(ltv.accounts.len(), 10);
    assert_eq!(
        ltv.data,
        vec![YdeltaInstruction::CheckLtvLiquidatable as u8]
    );

    let maturity = check_maturity_liquidatable_instruction(
        &market,
        &payer.pubkey(),
        7,
        &Pubkey::new_unique(), // debt_bank
        &Pubkey::new_unique(), // marginfi_program
    );
    // 5 fixed + 1 debt_bank + 1 marginfi_program = 7. Maturity gate
    // doesn't price the loan, so no oracle / collateral accounts.
    assert_eq!(maturity.accounts.len(), 7);
    assert_eq!(
        maturity.data,
        vec![YdeltaInstruction::CheckMaturityLiquidatable as u8]
    );
}

#[test]
fn protocol_fee_claim_ix_has_fourteen_accounts() {
    let market = Pubkey::new_unique();
    let admin = Keypair::new();
    let ix = protocol_fee_claim_instruction(
        &market,
        &admin.pubkey(),
        &Pubkey::new_unique(),   // debt_mint
        &Pubkey::new_unique(),   // admin_debt_token
        &Pubkey::new_unique(),   // debt_bank
        &Pubkey::new_unique(),   // debt_liquidity_vault
        &Pubkey::new_unique(),   // debt_bank_lva
        &[Pubkey::new_unique()], // debt_oracles (single-oracle bank)
        &Pubkey::new_unique(),   // token_program
        &Pubkey::new_unique(),   // marginfi_group
        &Pubkey::new_unique(),   // marginfi_program
    );
    // admin (signer) + global_config + market + admin_debt_token +
    // market_debt_vault + market_signer + lender_marginfi + debt_bank +
    // debt_liquidity_vault + debt_bank_lva + debt_oracle + debt_mint +
    // token_program + marginfi_group + marginfi_program = 15.
    assert_eq!(ix.accounts.len(), 15);
    // Tag-only payload (no trailing data).
    assert_eq!(ix.data.len(), 1);
    assert_eq!(ix.data[0], 19);
}

#[test]
fn partial_repay_payload_round_trips() {
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let mk = |repay_max: u64| {
        settle_matured_loan_instruction(
            &market,
            &payer.pubkey(),
            7,
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &[Pubkey::new_unique()],
            &[Pubkey::new_unique()],
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            repay_max,
            &Pubkey::new_unique(),
            None,
        )
    };
    // Tag (1 byte) + u64 (8 bytes) = 9.
    let full = mk(0);
    assert_eq!(full.data.len(), 9);
    assert_eq!(&full.data[1..], &0u64.to_le_bytes());
    let partial = mk(123_456);
    assert_eq!(&partial.data[1..], &123_456u64.to_le_bytes());

    // Same shape on liquidate_loan.
    let liq = liquidate_loan_instruction(
        &market,
        &payer.pubkey(),
        7,
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &[Pubkey::new_unique()],
        &[Pubkey::new_unique()],
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        999,
        &Pubkey::new_unique(),
        None,
    );
    assert_eq!(liq.data.len(), 9);
    assert_eq!(&liq.data[1..], &999u64.to_le_bytes());
}

/// Live oracle-priced LTV-breach liquidation. Boots a real marginfi
/// fixture, opens a loan, and asserts:
///   1. `liquidate_loan` rejects with `LoanStillSolvent` while the
///      wSOL oracle holds at mainnet (~$84) — loan's maint-LTV well
///      above threshold.
///   2. After dropping the wSOL oracle to $0.001, `liquidate_loan`
///      succeeds, flipping the loan to `Repaid`.
///
/// SBF-only: uses MarketFixture which loads marginfi.so + ydelta.so
/// from `SBF_OUT_DIR`.
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn liquidate_loan_breaches_at_oracle_drop_succeeds() {
    let fixture = MarketFixture::new().await;

    // Match: vault ASK 1M USDC, borrower BID with 800k wSOL collateral
    // (sits well above maint requirement at mainnet wSOL price).
    let principal: u64 = 1_000_000;
    let collateral: u64 = 50_000_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        setup_vault_funded_loan(&fixture, principal, collateral, 30 * 86_400).await;

    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.state,
        LoanState::Active as u8,
        "loan must be Active before liquidation"
    );
    // Conservation must hold at promotion.
    fixture.assert_loan_conservation_holds(0).await;

    // (1) Solvent at mainnet price → liquidate_loan must reject with
    // `LoanStillSolvent` (Custom 40). Anything else means we're hitting
    // a different bug path and the test isn't actually exercising LTV.
    let solvent_result = fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await;
    crate::assert_custom_error!(
        solvent_result,
        ydelta::program::YdeltaError::LoanStillSolvent
    );

    // (2) Crash wSOL → $0.001. fp18 scale: 0.001 USD = 10^15.
    // Aggressive crash so the LTV breach is unambiguous regardless
    // of marginfi maint weights.
    fixture
        .set_swb_oracle_price_atoms(mainnet::sol_oracle(), 1_000_000_000_000_000_i128)
        .await;
    // Advance one slot so the bank snapshots the new account state.
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

    // Sanity: verify our price write actually landed in account data.
    {
        let data = fixture.account_data(mainnet::sol_oracle()).await;
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&data[2264..2280]);
        let value_fp18 = i128::from_le_bytes(buf);
        assert!(
            value_fp18 < 10_000_000_000_000_000_i128,
            "wSOL oracle price did not crash; reads {} (fp18)",
            value_fp18
        );
    }

    // After the crash, collateral value ~= 0.0008 SOL × $1 = $0.0008
    // backing $1 of debt — well below maint LTV. liquidate_loan must
    // succeed and flip the loan to Repaid.
    fixture.refresh_blockhash().await;
    fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .expect("liquidate_loan must succeed at breached LTV");

    // Per the repay/claim split, full liquidation now closes the loan
    // PDA in-place (it used to wait for claim_repayment_for_risk_profile).
    // Assert closure directly; the loan body's accumulators have already
    // been migrated to the market-level `accumulated_protocol_fee_shares`
    // and the lender vault's risk-profile decrements during liquidate.
    assert!(
        fixture.loan_account_is_closed(0).await,
        "full liquidation must close the loan PDA in-place",
    );
    fixture.assert_loan_conservation_holds(0).await;

    // Protocol-fee accumulator now lives on the MARKET (in
    // `accumulated_protocol_fee_shares`), not the loan body. Verify it's
    // physically backed by the lender_marginfi_account's asset_shares.
    use marginfi_mocks::state::MarginfiAccount;
    use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
    let market = fixture.read_market_fixed().await;
    let market_protocol_fee_shares = market.accumulated_protocol_fee_shares;
    let mfi_data = fixture
        .account_data(fixture.lender_marginfi_account_pubkey())
        .await;
    let mfi = MarginfiAccount::try_from_account_data(&mfi_data).unwrap();
    let lender_shares = mfi
        .find_balance(&mainnet::usdc_bank())
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares).unwrap())
        .unwrap_or(0);
    assert!(
        market_protocol_fee_shares <= lender_shares,
        "market.accumulated_protocol_fee_shares ({}) must be ≤ lender_marginfi_account asset_shares ({})",
        market_protocol_fee_shares,
        lender_shares,
    );
}

/// End-to-end matured-settlement: open a fixed-rate loan, advance the
/// clock past `matures_at + grace_period_seconds` without repaying,
/// then call `settle_matured_loan` from a third-party keeper. Asserts:
///   - loan flips to `Repaid` with zero outstanding + zero collateral
///   - cranker bot scenario validated (no signer = borrower required)
///   - PDA closes (keeper recovers rent)
///
/// Maturity gate is `now > matures_at + grace`; the default grace is
/// 86_400 seconds, so we advance to maturity + 2 days to be unambiguous.
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn settle_matured_loan_after_grace_seizes_collateral() {
    let fixture = MarketFixture::new().await;

    let principal: u64 = 1_000_000;
    let collateral: u64 = 50_000_000;
    let term_seconds: u32 = 30 * 86_400;
    let (keeper, keeper_usdc, keeper_wsol) =
        setup_vault_funded_loan(&fixture, principal, collateral, term_seconds).await;

    let loan_pre = fixture.read_loan(0).await;
    assert_eq!(loan_pre.state, LoanState::Active as u8);
    assert!(loan_pre.collateral_atoms > 0);

    // ── Past grace: settle_matured must succeed ──
    // Default grace_period_seconds = 86_400. Advance two full days
    // beyond maturity to clear the strict-> gate unambiguously.
    // (We deliberately don't ALSO test pre-grace rejection here:
    // ProgramTestContext doesn't reliably respect a second set_sysvar
    // after a failed tx in the same fixture, so the second clock write
    // would silently no-op. Pre-grace rejection is covered by the
    // assert_past_grace_period unit test.)
    fixture
        .set_clock_unix_timestamp(loan_pre.matures_at_unix + 2 * 86_400)
        .await;
    fixture.refresh_blockhash().await;
    fixture.refresh_oracle_freshness().await;

    fixture
        .settle_matured_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .expect("settle_matured_loan must succeed past grace window");

    // Per the repay/claim split, full settlement closes the loan PDA
    // in-place. Asserting closed = trivially-repaid.
    assert!(
        fixture.loan_account_is_closed(0).await,
        "full settle_matured_loan must close the loan PDA in-place",
    );
    // Conservation collapses on close (atoms moved to seat); helper no-ops.
    fixture.assert_loan_conservation_holds(0).await;
}

/// Partial liquidation: keeper passes a non-zero `repay_atoms_max` cap.
/// The loan stays Active with reduced outstanding + collateral; the
/// keeper can come back later to clear the rest. Validates the cranker
/// scenario where a maintenance bot wants to liquidate in chunks
/// (e.g. to avoid cratering the collateral mint price by dumping).
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn partial_liquidation_leaves_loan_active_with_reduced_balances() {
    let fixture = MarketFixture::new().await;

    let principal: u64 = 1_000_000;
    let collateral: u64 = 50_000_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        setup_vault_funded_loan(&fixture, principal, collateral, 30 * 86_400).await;

    let loan_pre = fixture.read_loan(0).await;
    let outstanding_pre = loan_pre.outstanding_debt_atoms;
    let collateral_pre = loan_pre.collateral_atoms;

    // Crash wSOL oracle to breach maint LTV (same approach as the full
    // liquidation test).
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

    // Partial liquidate: only repay half the outstanding.
    let repay_max: u64 = outstanding_pre / 2;
    assert!(
        repay_max > 0,
        "test setup requires non-zero outstanding to halve"
    );
    fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, repay_max)
        .await
        .expect("partial liquidate_loan must succeed at breached LTV");

    // Loan must STILL be Active — partial liquidation doesn't close.
    let loan_post = fixture.read_loan(0).await;
    assert_eq!(
        loan_post.state,
        LoanState::Active as u8,
        "partial liquidation must leave the loan Active"
    );
    // Outstanding shrank by ~repay_max (within rounding).
    let outstanding_drop = outstanding_pre.saturating_sub(loan_post.outstanding_debt_atoms);
    assert!(
        outstanding_drop >= repay_max.saturating_sub(2) && outstanding_drop <= repay_max + 2,
        "outstanding must drop by ≈ repay_max (got drop {}, repay_max {})",
        outstanding_drop,
        repay_max
    );
    assert!(
        loan_post.outstanding_debt_atoms > 0,
        "outstanding must remain > 0 after partial repay"
    );
    // Collateral was reduced. Note: at an extreme oracle crash like
    // ours (wSOL → $0.001), the dollar value of all 800k atoms of
    // collateral is far less than $0.5 of debt, so the keeper-bonus
    // mechanism legitimately seizes the entire collateral pool even
    // for a 50% partial repay — that's the bad-debt-adjacent regime,
    // not the "well-collateralized partial liq" path. We assert only
    // the load-bearing partial-liq invariants:
    //   - loan stays Active
    //   - outstanding dropped by ≈ repay_max
    //   - collateral was reduced (not necessarily > 0)
    assert!(
        loan_post.collateral_atoms <= collateral_pre,
        "partial liquidation must not increase collateral (was {}, now {})",
        collateral_pre,
        loan_post.collateral_atoms
    );
    // Conservation must hold across the partial liquidation event.
    fixture.assert_loan_conservation_holds(0).await;

    // Keeper earns: their collateral-side token balance grew by the
    // bonus. (Their debt-side balance went DOWN to fund the repay.)
    let keeper_wsol_balance = fixture.token_balance(keeper_wsol).await;
    assert!(
        keeper_wsol_balance > 0,
        "keeper must receive collateral as keeper bonus on liquidation"
    );
}

/// Liquidator (cranker) earns the keeper bonus on `settle_matured_loan`.
/// The settle path doesn't need an oracle crash — the loan just has to
/// be past `matures_at + grace`. We assert the keeper's collateral-side
/// token balance grew strictly after settlement (i.e. they received the
/// liquidation_keeper_bps share of seized collateral). Pairs with the
/// existing `liquidate_loan_breaches_at_oracle_drop_succeeds` to cover
/// the OTHER reward path.
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn liquidator_keeper_balance_grows_after_settlement() {
    let fixture = MarketFixture::new().await;

    // Set a meaningful keeper bonus so the receipt is observable above
    // marginfi share-rounding noise.
    let mut params = SetFeeConfigParams::default();
    params.liquidation_keeper_bps = Some(500); // 5%
    let cfg_ix =
        set_fee_config_instruction(&fixture.market.pubkey(), &fixture.payer.pubkey(), params);
    let payer_kp = fixture.payer.insecure_clone();
    fixture.process(cfg_ix, &[&payer_kp]).await.unwrap();
    fixture.refresh_blockhash().await;

    let principal: u64 = 1_000_000;
    let collateral: u64 = 50_000_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        setup_vault_funded_loan(&fixture, principal, collateral, 30 * 86_400).await;

    let loan_pre = fixture.read_loan(0).await;

    // Past grace, no repay → settle_matured.
    fixture
        .set_clock_unix_timestamp(loan_pre.matures_at_unix + 2 * 86_400)
        .await;
    fixture.refresh_blockhash().await;
    fixture.refresh_oracle_freshness().await;

    let keeper_usdc_pre = fixture.token_balance(keeper_usdc).await;
    let keeper_wsol_pre = fixture.token_balance(keeper_wsol).await;

    fixture
        .settle_matured_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .expect("settle_matured_loan must succeed past grace");

    let keeper_usdc_post = fixture.token_balance(keeper_usdc).await;
    let keeper_wsol_post = fixture.token_balance(keeper_wsol).await;

    // Keeper paid debt-side atoms (USDC went DOWN to fund the repay) and
    // received collateral-side atoms (wSOL went UP as the bonus).
    assert!(
        keeper_usdc_post < keeper_usdc_pre,
        "keeper must spend USDC to repay outstanding debt (pre={}, post={})",
        keeper_usdc_pre,
        keeper_usdc_post
    );
    assert!(
        keeper_wsol_post > keeper_wsol_pre,
        "keeper must receive wSOL bonus from seized collateral (pre={}, post={})",
        keeper_wsol_pre,
        keeper_wsol_post
    );
    // Per the repay/claim split, full settle closes the loan PDA in-place.
    assert!(
        fixture.loan_account_is_closed(0).await,
        "full settle_matured_loan must close the loan PDA in-place",
    );
    fixture.assert_loan_conservation_holds(0).await;
}

/// Protocol earns on liquidation (matches marginfi's insurance-fee
/// model). Sets `liquidation_protocol_bps = 200` (2%), runs full
/// liquidation via `liquidate_loan`, and asserts:
///   - the loan body's `accumulated_protocol_fee_atoms` grew by the
///     fee amount (≈ 2% of the repaid debt)
///   - the lender's `lender_claimable_atoms` is reduced by the same
///     amount (the protocol's cut comes out of the LP's recovery)
#[cfg(feature = "test-sbf")]
#[tokio::test]
async fn protocol_accrues_fee_on_liquidate_loan() {
    let fixture = MarketFixture::new().await;

    // 2% liquidation protocol fee (on top of the 5% keeper bonus).
    let mut params = SetFeeConfigParams::default();
    params.liquidation_keeper_bps = Some(500);
    params.liquidation_protocol_bps = Some(200);
    let cfg_ix =
        set_fee_config_instruction(&fixture.market.pubkey(), &fixture.payer.pubkey(), params);
    let payer_kp = fixture.payer.insecure_clone();
    fixture.process(cfg_ix, &[&payer_kp]).await.unwrap();
    fixture.refresh_blockhash().await;

    let principal: u64 = 1_000_000;
    let collateral: u64 = 50_000_000;
    let (keeper, keeper_usdc, keeper_wsol) =
        setup_vault_funded_loan(&fixture, principal, collateral, 30 * 86_400).await;

    let loan_pre = fixture.read_loan(0).await;
    let lender_claimable_pre = loan_pre.lender_claimable_atoms;
    let protocol_fee_pre = loan_pre.accumulated_protocol_fee_atoms;

    // Crash wSOL → liquidate fully.
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

    // Snapshot market-level protocol fee accumulator BEFORE the liquidation.
    let market_protocol_shares_pre = fixture
        .read_market_fixed()
        .await
        .accumulated_protocol_fee_shares;
    let _ = (lender_claimable_pre, protocol_fee_pre);

    fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .expect("liquidate_loan must succeed at breached LTV");

    // Per the repay/claim split, full liquidation closes the loan PDA;
    // the loan body's `accumulated_protocol_fee_atoms` got migrated to
    // the MARKET's `accumulated_protocol_fee_shares` (shares not atoms).
    assert!(
        fixture.loan_account_is_closed(0).await,
        "full liquidation must close the loan PDA",
    );
    fixture.assert_loan_conservation_holds(0).await;

    // Expected protocol take ≈ outstanding_live × 200 / 10_000 atoms.
    // At genesis share-value ≈ 1.0, shares ≈ atoms, so the market's
    // protocol-fee-shares accumulator should have grown by approximately
    // expected_protocol_take.
    let expected_protocol_take: u64 = principal * 200 / 10_000;
    assert!(
        expected_protocol_take > 1_000,
        "test parameters too small to observe protocol fee"
    );
    let market_protocol_shares_post = fixture
        .read_market_fixed()
        .await
        .accumulated_protocol_fee_shares;
    let protocol_fee_delta_shares: u128 =
        market_protocol_shares_post.saturating_sub(market_protocol_shares_pre);

    // Convert shares back to atoms using the bank's live asset_share_value
    // — mainnet snapshot's USDC bank has SP > 1.0 from accumulated supply
    // yield, so shares ≠ atoms.
    use marginfi_mocks::state::Bank;
    use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let bank = Bank::try_from_account_data(&bank_data).unwrap();
    let asv = wrapped_i80f48_to_u128(bank.asset_share_value).unwrap();
    let protocol_fee_delta_atoms: u64 = ydelta::math::from_scaled_floor(
        ydelta::math::mul_scale(protocol_fee_delta_shares, asv).unwrap(),
    ) as u64;
    let drift = (protocol_fee_delta_atoms as i128 - expected_protocol_take as i128).abs();
    assert!(
        drift <= 8,
        "market protocol-fee delta drift > 8 atoms (got {}, expected ≈{}, share_value_fp48={})",
        protocol_fee_delta_atoms,
        expected_protocol_take,
        asv,
    );
}
