//! v1 D17: LTV is decoupled from marginfi.
//!
//! - Origination: the sub-vault's `max_ltv_bps` is the ONLY gate on
//!   fixed fills — a cap ABOVE marginfi's implied init-weight LTV fills
//!   a loan marginfi itself would refuse.
//! - Fallback: the P2Pool residual opens a REAL marginfi borrow, so
//!   marginfi init weights gate it (and only it) via
//!   `FallbackLtvInsufficient`.
//! - Liquidation: Fixed loans gate on the LTV pair STAMPED at match —
//!   curator updates never move thresholds on open loans, and marginfi
//!   maint weights are not consulted.
//! - `liquidation_ltv ≥ max_ltv + MIN_LIQ_GAP_BPS` at create/update —
//!   no loan is born liquidatable.

use solana_sdk::signer::Signer;

use marginfi_mocks::state::BankConfigView;
use solana_program::pubkey::Pubkey;
use ydelta::program::YdeltaError;
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;
use ydelta::state::loan::LoanState;
use ydelta::state::ltv::{
    get_required_quote_collateral_to_back_debt, required_collateral_at_ltv_cap,
};
use ydelta::state::market_helpers::RESIDUAL_MODE_DROP;

use crate::assert_custom_error;
use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

const TERM: u32 = 30 * 86_400;

/// Mirror of `oracles::scale_to_fp48` (private to the protocol module).
fn scale_to_fp48(value: u128, exponent: i32) -> u128 {
    if exponent >= 0 {
        let mut factor: u128 = 1;
        for _ in 0..exponent {
            factor = factor.saturating_mul(10);
        }
        (value.saturating_mul(factor)) << 48
    } else {
        let abs = (-exponent) as u32;
        let mut denom: u128 = 1;
        for _ in 0..abs {
            denom = denom.saturating_mul(10);
        }
        (value << 48) / denom
    }
}

/// Decode the USDC Pyth-push oracle fixture → fp48 price.
fn decode_pyth_price(data: &[u8]) -> u128 {
    let price = i64::from_le_bytes(data[73..81].try_into().unwrap());
    let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap());
    assert!(price > 0);
    scale_to_fp48(price as u128, exponent)
}

/// Decode the SOL Switchboard-pull oracle fixture → fp48 price.
fn decode_swb_price(data: &[u8]) -> u128 {
    let result_value = i128::from_le_bytes(data[2264..2280].try_into().unwrap());
    assert!(result_value > 0);
    ((result_value as u128).saturating_mul(1u128 << 48)) / 10u128.pow(18)
}

/// Fixture oracle prices as fp48 `(debt, collateral)`.
async fn oracle_prices(fixture: &MarketFixture) -> (ydelta::math::Fp48, ydelta::math::Fp48) {
    let usdc_oracle_data = fixture.account_data(mainnet::usdc_oracle()).await;
    let sol_oracle_data = fixture.account_data(mainnet::sol_oracle()).await;
    (
        ydelta::math::Fp48::from_raw(decode_pyth_price(&usdc_oracle_data)),
        ydelta::math::Fp48::from_raw(decode_swb_price(&sol_oracle_data)),
    )
}

/// What MARGINFI would require (init weights) to back `debt_atoms` —
/// the threshold the old coupled gate enforced and D17 removed from
/// fixed fills.
async fn marginfi_required_collateral(fixture: &MarketFixture, debt_atoms: u64) -> u64 {
    let (debt_price, coll_price) = oracle_prices(fixture).await;
    let usdc_bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let sol_bank_data = fixture.account_data(mainnet::sol_bank()).await;
    let usdc_cfg = BankConfigView::try_from_account_data(&usdc_bank_data).unwrap();
    let sol_cfg = BankConfigView::try_from_account_data(&sol_bank_data).unwrap();
    let debt_liability_weight_init = ydelta::math::Fp48::from_raw(
        wrapped_i80f48_to_u128(usdc_cfg.liability_weight_init()).unwrap(),
    );
    let collateral_asset_weight_init = ydelta::math::Fp48::from_raw(
        wrapped_i80f48_to_u128(sol_cfg.asset_weight_init()).unwrap(),
    );
    get_required_quote_collateral_to_back_debt(
        debt_atoms,
        debt_price,
        coll_price,
        debt_liability_weight_init,
        collateral_asset_weight_init,
        /*debt_mint_decimals=*/ 6,
        /*collateral_mint_decimals=*/ 9,
    )
    .unwrap()
}

/// Required collateral at a sub-vault cap (the D17 origination gate).
async fn cap_required_collateral(fixture: &MarketFixture, debt_atoms: u64, cap_bps: u16) -> u64 {
    let (debt_price, coll_price) = oracle_prices(fixture).await;
    required_collateral_at_ltv_cap(debt_atoms, debt_price, coll_price, cap_bps, 6, 9).unwrap()
}

/// Crash the wSOL Switchboard oracle so the loan's live LTV (debt / coll
/// value, in bps) hits `target_bps`, given the loan's outstanding debt
/// and posted collateral. Inverts the on-chain required-collateral
/// formula for a USDC(6-dec)/wSOL(9-dec) market:
///   `effective_ltv = 10000 × out × debt_price × 10^3 / (coll_price × coll)`
/// solved for `coll_price`. Uses f64 (test-side only) to stay clear of
/// i128 overflow; precision is far finer than the ≥150 bps test margins.
/// Writes the price (fp18), warps a slot so the bank re-snapshots, and
/// refreshes oracle freshness.
async fn crash_sol_to_effective_ltv(
    fixture: &MarketFixture,
    outstanding_atoms: u64,
    collateral_atoms: u64,
    target_bps: u32,
) {
    let (debt_price_fp48, _coll_price_fp48) = oracle_prices(fixture).await;
    let debt_price_usd = (debt_price_fp48.raw() as f64) / 2f64.powi(48);
    let coll_price_usd = 10_000.0 * (outstanding_atoms as f64) * debt_price_usd * 1_000.0
        / ((target_bps as f64) * (collateral_atoms as f64));
    let value_fp18 = (coll_price_usd * 1e18) as i128;
    fixture
        .set_swb_oracle_price_atoms(mainnet::sol_oracle(), value_fp18)
        .await;
    let cur = fixture
        .context
        .borrow()
        .banks_client
        .get_root_slot()
        .await
        .unwrap();
    fixture.context.borrow_mut().warp_to_slot(cur + 1).unwrap();
    fixture.refresh_blockhash().await;
    fixture.refresh_oracle_freshness().await;
}

/// A sub-vault cap ABOVE marginfi's implied LTV fills a fixed loan that
/// marginfi itself would refuse: collateral sits between the cap's
/// requirement and marginfi's init-weight requirement.
#[tokio::test]
async fn cap_above_marginfi_fills_what_marginfi_would_refuse() {
    let fixture = MarketFixture::new().await;
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
            /*max_ltv_bps=*/ Some(9_000),
            /*rate_bps=*/ 100,
            TERM,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_000_000;
    let required_at_cap = cap_required_collateral(&fixture, principal_atoms, 9_000).await;
    let required_marginfi = marginfi_required_collateral(&fixture, principal_atoms).await;
    assert!(
        required_at_cap < required_marginfi,
        "test premise: a 90% cap must demand LESS collateral than marginfi's \
         init weights (cap {} vs marginfi {}); otherwise the decoupling has \
         nothing to prove",
        required_at_cap,
        required_marginfi
    );
    // Strictly between the two thresholds: marginfi would refuse this
    // loan; the sub-vault cap accepts it.
    let collateral_atoms = (required_at_cap + required_marginfi) / 2;

    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            3_000,
            TERM,
            principal_atoms,
            collateral_atoms,
            RESIDUAL_MODE_DROP,
        )
        .await
        .expect("D17: the sub-vault cap is the only origination gate");

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "the fill marginfi would refuse must land under the 90% cap"
    );
    let profile = fixture.read_sub_vault(1).await;
    assert_eq!(profile.encumbered_in_orders_atoms, principal_atoms);
}

/// The same in-between collateral with the P2Pool-fallback residual mode
/// errors `FallbackLtvInsufficient`: the fallback is a REAL marginfi
/// borrow, so marginfi's init weights gate it.
#[tokio::test]
async fn fallback_below_marginfi_weights_errors() {
    let fixture = MarketFixture::new().await;
    let bob = fixture.create_trader().await;

    // Empty ask book: the whole bid is residual → P2Pool fallback.
    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000_000, false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_000_000;
    let required_at_cap = cap_required_collateral(&fixture, principal_atoms, 9_000).await;
    let required_marginfi = marginfi_required_collateral(&fixture, principal_atoms).await;
    let collateral_atoms = (required_at_cap + required_marginfi) / 2;
    assert!(collateral_atoms < required_marginfi);

    let result = fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            3_000,
            TERM,
            principal_atoms,
            collateral_atoms,
            /*residual_mode: P2PoolFallback=*/ 0,
        )
        .await;
    assert_custom_error!(result, YdeltaError::FallbackLtvInsufficient);
}

/// §5.6 three-way discrimination of the Fixed-loan liquidation gate.
/// Loan stamped 85/92 at match. Then:
///   1. live LTV pushed to ~90% → `LoanStillSolvent`. (The old
///      maint-weight gate — implied maint LTV well below 90% — would
///      have liquidated here; the stamp at 92% does not.)
///   2. curator raises the sub-vault's liquidation threshold to 95% →
///      the open loan's threshold must not move.
///   3. live LTV pushed to ~93.5% — above the 92% STAMP but below the
///      sub-vault's live 95% → liquidation SUCCEEDS, proving the gate
///      reads the stamp, not the live sub-vault and not maint weights.
#[tokio::test]
async fn liquidation_gate_reads_stamp_not_live_sub_vault() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let bob = fixture.create_trader().await;
    let keeper = fixture.create_trader().await;

    // Lender side with an EXPLICIT 85/92 LTV pair.
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        20_000_000,
    );
    fixture.refresh_blockhash().await;
    let _ = fixture.create_vault(&admin).await;
    fixture.refresh_blockhash().await;
    fixture
        .create_pool_sub_vault_full(
            curator.pubkey(),
            /*spread_bps=*/ 100,
            /*max_ltv_bps=*/ 8_500,
            /*liquidation_ltv_bps=*/ 9_200,
            TERM,
            /*curator_fee_bps=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 10_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_for_sub_vault(&curator, 1, 0, TERM, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Borrower side: real wSOL collateral so liquidation can seize it.
    let collateral: u64 = 50_000_000;
    let principal: u64 = 1_000_000;
    fixture.claim_seat(&bob).await;
    let bob_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(bob_wsol, bob.pubkey(), collateral * 2);
    let keeper_usdc = Pubkey::new_unique();
    fixture.put_token_account(keeper_usdc, mainnet::usdc_mint(), keeper.pubkey(), 100_000_000);
    let keeper_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(keeper_wsol, keeper.pubkey(), 0);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&bob, bob_wsol, /*is_debt=*/ false, collateral)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            3_000,
            TERM,
            principal,
            collateral,
            /*flags=*/ 0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture.crank_matched_loan_for_sub_vault(0).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture.refresh_oracle_freshness().await;

    // The LTV pair is stamped on the promoted loan.
    let loan = fixture.read_loan(0).await;
    assert_eq!(loan.state, LoanState::Active as u8);
    assert_eq!(loan.origination_ltv_bps, 8_500, "origination stamp");
    assert_eq!(loan.liquidation_ltv_bps, 9_200, "liquidation stamp");
    let outstanding = loan.outstanding_debt_atoms;

    // (1) Live LTV ~90%: below the 92% stamp → must stay solvent. (A
    // marginfi-maint-weight gate would likely liquidate here; the stamp
    // does not — that is the decoupling.)
    crash_sol_to_effective_ltv(&fixture, outstanding, collateral, 9_000).await;
    let result = fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await;
    assert_custom_error!(result, YdeltaError::LoanStillSolvent);

    // (2) Curator raises the sub-vault pair (max stays 8_500; the test
    // helper sets the liq companion to max + 1_000 = 9_500). The open
    // loan's stamp must be unaffected.
    fixture.refresh_blockhash().await;
    fixture
        .update_sub_vault(&curator, 1, Some(8_500), None)
        .await
        .unwrap();
    assert_eq!(fixture.read_sub_vault(1).await.liquidation_ltv_bps, 9_500);
    assert_eq!(
        fixture.read_loan(0).await.liquidation_ltv_bps,
        9_200,
        "curator update must never move an open loan's threshold"
    );

    // (3) Live LTV ~93.5%: above the 92% STAMP, below the live
    // sub-vault's now-95% → liquidation must succeed off the stamp. If
    // the gate (wrongly) read the live 95%, this would reject.
    crash_sol_to_effective_ltv(&fixture, outstanding, collateral, 9_350).await;
    fixture
        .liquidate_loan(&keeper, 0, keeper_usdc, keeper_wsol, /*repay_max=*/ 0)
        .await
        .expect("gate must read the stamped 92%, not the raised 95%");
    assert!(fixture.loan_account_is_closed(0).await);
}

/// `liquidation_ltv ≥ max_ltv + MIN_LIQ_GAP_BPS` at create AND update —
/// no loan can be born liquidatable.
#[tokio::test]
async fn liq_gap_violation_rejected_at_create_and_update() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let owner = fixture.create_trader().await;

    let _ = fixture.create_vault(&admin).await;
    fixture.refresh_blockhash().await;

    // Create: gap of 100 < MIN_LIQ_GAP_BPS (200) rejects.
    let result = fixture
        .create_private_sub_vault(&owner, 9_000, 9_100, TERM)
        .await;
    assert!(result.is_err(), "create with a 100bps liq gap must reject");

    // A valid pair creates fine…
    fixture.refresh_blockhash().await;
    fixture
        .create_private_sub_vault(&owner, 9_000, 9_300, TERM)
        .await
        .unwrap();

    // …and an update that shrinks the gap below the minimum rejects.
    fixture.refresh_blockhash().await;
    let bad_update =
        ydelta::program::instruction_builders::update_sub_vault_instruction::update_sub_vault_instruction(
            &mainnet::usdc_bank(),
            &owner.pubkey(),
            /*sub_vault_id=*/ 1,
            /*new_max_ltv_bps=*/ None,
            /*new_liquidation_ltv_bps=*/ Some(9_100),
            /*new_max_term_seconds=*/ None,
            /*new_spread_bps=*/ None,
        );
    let kp = owner.insecure_clone();
    let result = fixture.process_ixs(&[bad_update], &[&kp]).await;
    assert!(result.is_err(), "update shrinking the liq gap below 200bps must reject");
}
