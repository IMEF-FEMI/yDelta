//! Per-match LTV gate at the matching engine using real oracle prices
//! from the loaded mainnet fixtures. v1 D17: the sub-vault's
//! `max_ltv_bps` is the ONLY origination gate — marginfi weights are
//! not consulted — and a failing bid is SKIPPED (the scan walks on),
//! never aborted.
//!
//! In the quote-only model the lender is a vault sub-vault resting
//! an unbounded ask; the borrower crosses it with an IOC bid carrying
//! `collateral_atoms`. The gate checks
//! `actual_ltv <= sub_vault.max_ltv_bps` at live oracle prices.
//!
//! The market is USDC(6-dec) debt / wSOL(9-dec) collateral. The
//! required-collateral helper normalizes for the 3-decimal gap. These
//! tests compute the true requirement from the fixture oracle prices
//! rather than hard-coding it, then post collateral relative to that
//! boundary:
//!   - over-collateralized: comfortably ABOVE the true requirement,
//!   - under-collateralized: ~10× BELOW the true requirement.

use solana_sdk::signer::Signer;

use ydelta::state::ltv::required_collateral_at_ltv_cap;

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

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

/// Compute the TRUE match-time required collateral (lamports of wSOL)
/// to back `debt_atoms` of USDC at the sub-vault's `max_ltv_bps` cap,
/// reading the fixture oracle prices straight off the loaded fixture
/// accounts. Mirrors exactly what the matching engine computes on-chain
/// (v1 D17 single gate) — debt 6 decimals, collateral 9 decimals.
async fn true_required_collateral(
    fixture: &MarketFixture,
    debt_atoms: u64,
    max_ltv_bps: u16,
) -> u64 {
    let usdc_oracle_data = fixture.account_data(mainnet::usdc_oracle()).await;
    let sol_oracle_data = fixture.account_data(mainnet::sol_oracle()).await;
    let debt_price_fp48 = ydelta::math::Fp48::from_raw(decode_pyth_price(&usdc_oracle_data));
    let collateral_price_fp48 = ydelta::math::Fp48::from_raw(decode_swb_price(&sol_oracle_data));

    required_collateral_at_ltv_cap(
        debt_atoms,
        debt_price_fp48,
        collateral_price_fp48,
        max_ltv_bps,
        /*debt_mint_decimals=*/ 6,
        /*collateral_mint_decimals=*/ 9,
    )
    .unwrap()
}

#[tokio::test]
async fn match_passes_with_overcollateralized_bid() {
    // USDC(6 dec) debt / wSOL(9 dec) collateral. Compute the TRUE
    // requirement to back $1 (1_000_000 atoms) of USDC debt, then post
    // 5× that — comfortably over-collateralized and well under the
    // sub_vault's 80% max_ltv cap.
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
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_000_000;
    // Compute the requirement at the sub-vault's 80% cap — the single
    // on-chain origination gate (v1 D17).
    let required = true_required_collateral(&fixture, principal_atoms, 8_000).await;
    assert!(
        required > 1_000_000,
        "requirement for $1 of debt against ~$100-200 SOL must be \
         in the millions of lamports, got {} — decimal normalization missing?",
        required
    );
    // Post 5× the true requirement — genuinely over-collateralized.
    let collateral_atoms: u64 = required.checked_mul(5).unwrap();
    let result = fixture
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
        .await;
    result.expect("well-collateralised match should pass LTV");

    // Match landed.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1);
    // Vault sub_vault encumbered for exactly the matched principal —
    // the cross fully filled.
    let sub_vault = fixture.read_sub_vault(1).await;
    assert_eq!(sub_vault.encumbered_in_orders_atoms, principal_atoms);
    fixture.assert_vault_idle_invariant(1).await;
}

#[tokio::test]
async fn undercollateralized_bid_is_skipped_not_filled() {
    // Post collateral in the genuinely-dangerous band: ~10× UNDER the
    // true requirement. A 10×-under loan is the realistic attack — a
    // decimal-blind requirement formula would let it through, while the
    // decimal-normalized gate correctly rejects it. v1 D17: the failing
    // bid is SKIPPED (no abort) and the Drop residual mode discards it,
    // so the ix succeeds but no loan is minted.
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
            /*max_ltv_bps=*/ Some(8_000),
            /*rate_bps=*/ 600,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 10_000_000,
        )
        .await;

    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000_000, false)
        .await;
    fixture.refresh_blockhash().await;

    let principal_atoms: u64 = 1_000_000;
    let required = true_required_collateral(&fixture, principal_atoms, 8_000).await;
    // ~10× under the true requirement — the dangerous middle band.
    // A decimal-blind requirement would be ~1000× smaller, so
    // `required / 10` would land ~100× ABOVE that bar and the cross
    // would be (wrongly) accepted.
    let collateral_atoms: u64 = (required / 10).max(1);
    assert!(
        collateral_atoms < required,
        "test setup: under-collateralized amount {} must be below required {}",
        collateral_atoms,
        required
    );
    let result = fixture
        .place_order_with_flags(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            3_000,
            30 * 86_400,
            principal_atoms,
            collateral_atoms,
            // Drop the residual so the skip isn't masked by the P2Pool
            // fallback (which has its own marginfi-weights gate).
            ydelta::state::market_helpers::RESIDUAL_MODE_DROP,
        )
        .await;
    result.expect("v1 D17: a failing bid is skipped, never aborted");

    // No match landed and nothing was reserved on the sub-vault.
    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 0);
    let sub_vault = fixture.read_sub_vault(1).await;
    assert_eq!(sub_vault.encumbered_in_orders_atoms, 0);
    assert_eq!(sub_vault.open_loans_count, 0);
    // The dropped residual released the borrower's collateral.
    let seat = fixture.read_seat(&bob.pubkey()).await;
    assert_eq!(seat.collateral_encumbered_shares, 0);
}
