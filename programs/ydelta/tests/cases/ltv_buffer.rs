//! Borrower-set LTV buffer (origination-cap tightening).
//!
//! A borrower attaches an optional `ltv_buffer_bps` to a bid. The
//! collateral gate (and the stamped `origination_ltv_bps`) use
//!   `effective_cap = sub_vault.max_ltv_bps − buffer` (saturating).
//!
//! GATE semantics, NOT shrink: a bid only fills against asks whose
//! effective cap its collateral clears; unfilled principal follows
//! `residual_mode` (it is never silently reduced). `buffer == 0` is the
//! prior behavior. The buffer PERSISTS on a rested bid (RestingOrder
//! field) so a later ask/MatchCrank cross honors it, and `UpdateOrder`
//! can change it.

use hypertree::NIL;
use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use ydelta::program::instruction_builders::update_order_instruction::update_order_instruction;
use ydelta::program::YdeltaError;
use ydelta::state::ltv::required_collateral_at_ltv_cap;
use ydelta::state::market_helpers::{RESIDUAL_MODE_DROP, RESIDUAL_MODE_REST};

use crate::assert_custom_error;
use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

const TERM: u32 = 30 * 86_400;
const BID_RATE: u16 = 3_000;

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

fn decode_pyth_price(data: &[u8]) -> u128 {
    let price = i64::from_le_bytes(data[73..81].try_into().unwrap());
    let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap());
    assert!(price > 0);
    scale_to_fp48(price as u128, exponent)
}

fn decode_swb_price(data: &[u8]) -> u128 {
    let result_value = i128::from_le_bytes(data[2264..2280].try_into().unwrap());
    assert!(result_value > 0);
    ((result_value as u128).saturating_mul(1u128 << 48)) / 10u128.pow(18)
}

async fn oracle_prices(fixture: &MarketFixture) -> (ydelta::math::Fp48, ydelta::math::Fp48) {
    let usdc_oracle_data = fixture.account_data(mainnet::usdc_oracle()).await;
    let sol_oracle_data = fixture.account_data(mainnet::sol_oracle()).await;
    (
        ydelta::math::Fp48::from_raw(decode_pyth_price(&usdc_oracle_data)),
        ydelta::math::Fp48::from_raw(decode_swb_price(&sol_oracle_data)),
    )
}

/// Required collateral at a sub-vault cap — the same formula the
/// on-chain origination gate uses (`required_collateral_at_ltv_cap`).
async fn cap_required_collateral(fixture: &MarketFixture, debt_atoms: u64, cap_bps: u16) -> u64 {
    let (debt_price, coll_price) = oracle_prices(fixture).await;
    required_collateral_at_ltv_cap(debt_atoms, debt_price, coll_price, cap_bps, 6, 9).unwrap()
}

/// A borrower seat seeded with enough collateral shares to back any of
/// these bids without a real wSOL deposit (the gate reads posted atoms,
/// not deposited balance).
async fn fund_borrower(fixture: &MarketFixture) -> solana_sdk::signature::Keypair {
    let bob = fixture.create_trader().await;
    fixture.claim_seat(&bob).await;
    fixture
        .seed_seat_shares(&bob.pubkey(), 1_000_000_000_000, /*is_debt=*/ false)
        .await;
    fixture.refresh_blockhash().await;
    bob
}

/// Provide a vault sub-vault that rests an unbounded ask at the given
/// `max_ltv_bps`. Mirrors the decoupling-test maker setup.
async fn provide_ask(fixture: &MarketFixture, max_ltv_bps: u16, deposit_atoms: u64) {
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*sub_vault_id=*/ 1,
            Some(max_ltv_bps),
            /*spread_bps=*/ 100,
            TERM,
            deposit_atoms,
        )
        .await;
}

/// (1) GATE: collateral sized to clear the raw cap fills with
/// `buffer == 0`, but the SAME collateral does NOT fill once a buffer
/// pulls the effective cap below what the collateral can back. Unfilled
/// principal follows `residual_mode` — never silently reduced.
#[tokio::test]
async fn gate_blocks_fill_that_passes_at_raw_cap() {
    let principal_atoms: u64 = 1_000_000;
    let max_ltv: u16 = 8_000;

    // --- buffer = 0: the bid fills at the raw 8000 cap. ---
    {
        let fixture = MarketFixture::new().await;
        provide_ask(&fixture, max_ltv, 100_000_000).await;
        let bob = fund_borrower(&fixture).await;

        // Exactly clears 8000 (tiny +1 margin to absorb rounding).
        let collateral = cap_required_collateral(&fixture, principal_atoms, max_ltv).await + 1;
        // Sanity: this same collateral CANNOT back the 7000 effective cap.
        let need_at_7000 = cap_required_collateral(&fixture, principal_atoms, 7_000).await;
        assert!(
            collateral < need_at_7000,
            "test premise: 8000-sized collateral must fall short of the \
             7000 requirement (8000-need {} vs 7000-need {})",
            collateral,
            need_at_7000
        );

        fixture
            .place_order_with_buffer(
                &bob,
                ydelta::state::Side::Bid,
                ydelta::state::OrderType::Limit,
                BID_RATE,
                TERM,
                principal_atoms,
                collateral,
                /*ltv_buffer_bps=*/ 0,
                RESIDUAL_MODE_DROP,
            )
            .await
            .expect("buffer=0: collateral clears the raw 8000 cap → fills");

        let market = fixture.read_market_fixed().await;
        assert_eq!(
            market.matched_loan_sequence, 1,
            "buffer=0 must fill at the raw cap"
        );
        fixture.crank_matched_loan_for_sub_vault(0).await.unwrap();
        let loan = fixture.read_loan(0).await;
        assert_eq!(
            loan.origination_ltv_bps, 8_000,
            "buffer=0 stamps the raw sub-vault cap"
        );
    }

    // --- buffer = 1000 (effective 7000): the SAME bid does NOT fill on
    //     the fixed book; Drop leaves the collateral unencumbered. ---
    {
        let fixture = MarketFixture::new().await;
        provide_ask(&fixture, max_ltv, 100_000_000).await;
        let bob = fund_borrower(&fixture).await;
        let collateral = cap_required_collateral(&fixture, principal_atoms, max_ltv).await + 1;

        let seat_pre = fixture.read_seat(&bob.pubkey()).await;
        assert_eq!(seat_pre.collateral_encumbered_shares, 0);

        fixture
            .place_order_with_buffer(
                &bob,
                ydelta::state::Side::Bid,
                ydelta::state::OrderType::Limit,
                BID_RATE,
                TERM,
                principal_atoms,
                collateral,
                /*ltv_buffer_bps=*/ 1_000,
                RESIDUAL_MODE_DROP,
            )
            .await
            .expect("Drop mode: the un-crossable bid succeeds, just fills nothing");

        let market = fixture.read_market_fixed().await;
        assert_eq!(
            market.matched_loan_sequence, 0,
            "buffer=1000 (effective 7000) must NOT cross the 8000-sized collateral"
        );
        let sv = fixture.read_sub_vault(1).await;
        assert_eq!(
            sv.encumbered_in_orders_atoms, 0,
            "no fill → no principal encumbered on the sub-vault"
        );
        let seat_post = fixture.read_seat(&bob.pubkey()).await;
        assert_eq!(
            seat_post.collateral_encumbered_shares, 0,
            "Drop returns the residual's collateral: nothing stays encumbered"
        );
    }

    // --- buffer = 1000 with REST: the un-crossable bid rests, principal
    //     intact (not silently reduced), collateral encumbered alive. ---
    {
        let fixture = MarketFixture::new().await;
        provide_ask(&fixture, max_ltv, 100_000_000).await;
        let bob = fund_borrower(&fixture).await;
        let collateral = cap_required_collateral(&fixture, principal_atoms, max_ltv).await + 1;

        fixture
            .place_order_with_buffer(
                &bob,
                ydelta::state::Side::Bid,
                ydelta::state::OrderType::Limit,
                BID_RATE,
                TERM,
                principal_atoms,
                collateral,
                /*ltv_buffer_bps=*/ 1_000,
                RESIDUAL_MODE_REST,
            )
            .await
            .expect("Rest mode: the un-crossable bid rests");

        let market = fixture.read_market_fixed().await;
        assert_eq!(market.matched_loan_sequence, 0, "nothing crossed");
        assert_ne!(
            market.bids_root_index, NIL,
            "the full principal rests (gate, not shrink)"
        );
        let seat = fixture.read_seat(&bob.pubkey()).await;
        assert!(
            seat.collateral_encumbered_shares > 0,
            "rested bid keeps its collateral encumbered"
        );
    }
}

/// (2) STAMP: a bid that DOES fill under a buffer stamps
/// `origination_ltv_bps == max_ltv − buffer`.
#[tokio::test]
async fn stamp_reflects_effective_cap() {
    let fixture = MarketFixture::new().await;
    let max_ltv: u16 = 8_000;
    let buffer: u16 = 500;
    let effective: u16 = 7_500;
    let principal_atoms: u64 = 1_000_000;

    provide_ask(&fixture, max_ltv, 100_000_000).await;
    let bob = fund_borrower(&fixture).await;

    // Generous enough to clear the 7500 effective cap.
    let collateral = cap_required_collateral(&fixture, principal_atoms, effective).await + 1;

    fixture
        .place_order_with_buffer(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BID_RATE,
            TERM,
            principal_atoms,
            collateral,
            buffer,
            RESIDUAL_MODE_DROP,
        )
        .await
        .expect("collateral clears the 7500 effective cap → fills");

    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 1, "the buffered bid fills");
    fixture.crank_matched_loan_for_sub_vault(0).await.unwrap();

    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.origination_ltv_bps, effective,
        "origination stamp must reflect max_ltv − buffer (8000 − 500)"
    );
    assert_eq!(
        loan.liquidation_ltv_bps, 9_000,
        "liquidation stamp is the sub-vault's (max+1000), unchanged by the buffer"
    );
}

/// (3) PERSISTENCE: a bid rests WITH a buffer against an empty book;
/// when a sub-vault ask later becomes crossable the buffer is honored on
/// the cross — the loan stamps `max_ltv − buffer`, and collateral that
/// would clear only the raw cap proves the buffer actually gated.
#[tokio::test]
async fn buffer_persists_on_rested_bid() {
    let fixture = MarketFixture::new().await;
    let max_ltv: u16 = 8_000;
    let buffer: u16 = 500;
    let effective: u16 = 7_500;
    let principal_atoms: u64 = 1_000_000;

    // Borrower rests first against an empty book, carrying the buffer.
    let bob = fund_borrower(&fixture).await;
    // Collateral clears the 7500 effective cap (so the later cross can
    // fill) yet is far short of what marginfi would want — irrelevant
    // here, the gate is the sub-vault cap.
    let collateral = cap_required_collateral(&fixture, principal_atoms, effective).await + 1;
    // A bid whose collateral only cleared the RAW 8000 cap would fall
    // short of the persisted 7500 cap → would not cross. Assert the
    // separation so the persistence claim is meaningful.
    let raw_only = cap_required_collateral(&fixture, principal_atoms, max_ltv).await + 1;
    assert!(
        raw_only < cap_required_collateral(&fixture, principal_atoms, effective).await,
        "test premise: 8000-sized collateral must fall short of the persisted 7500 cap"
    );

    fixture
        .place_order_with_buffer(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BID_RATE,
            TERM,
            principal_atoms,
            collateral,
            buffer,
            RESIDUAL_MODE_REST,
        )
        .await
        .expect("empty book + Rest → the buffered bid rests");

    let market = fixture.read_market_fixed().await;
    assert_eq!(market.matched_loan_sequence, 0, "nothing crossed yet");
    assert_ne!(market.bids_root_index, NIL, "buffered bid rests");

    // A sub-vault ask now arrives and TAKES the resting bid in the same
    // placement (v1 D7). The persisted buffer must gate that cross.
    provide_ask(&fixture, max_ltv, 100_000_000).await;

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "ask placement crosses the resting buffered bid"
    );
    assert_eq!(market.bids_root_index, NIL, "fully-consumed bid leaves the tree");

    fixture.crank_matched_loan_for_sub_vault(0).await.unwrap();
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.origination_ltv_bps, effective,
        "the buffer persisted on the rested bid and gated/stamped the later cross"
    );
}

/// (4) UPDATE: rest a bid, change its buffer via `UpdateOrder`, and
/// confirm the NEW buffer is enforced on the later cross — the loan
/// stamps `max_ltv − new_buffer`.
#[tokio::test]
async fn update_order_changes_buffer() {
    let fixture = MarketFixture::new().await;
    let max_ltv: u16 = 8_000;
    let new_buffer: u16 = 1_000;
    let new_effective: u16 = 7_000;
    let principal_atoms: u64 = 1_000_000;

    let bob = fund_borrower(&fixture).await;
    // Collateral generous enough to clear even the post-update 7000 cap,
    // so the only variable under test is the stamped effective cap.
    let collateral = cap_required_collateral(&fixture, principal_atoms, new_effective).await + 1;

    // Rest with an initial buffer of 500 (effective 7500).
    fixture
        .place_order_with_buffer(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BID_RATE,
            TERM,
            principal_atoms,
            collateral,
            /*ltv_buffer_bps=*/ 500,
            RESIDUAL_MODE_REST,
        )
        .await
        .expect("buffered bid rests");
    fixture.refresh_blockhash().await;

    // UpdateOrder: bump the buffer to 1000 (effective 7000). The bid
    // takes the next sequence (1); principal/collateral are untouched.
    let update_ix = update_order_instruction(
        &fixture.market.pubkey(),
        &bob.pubkey(),
        /*order_sequence=*/ 0,
        None,
        /*new_rate_bps=*/ BID_RATE,
        /*new_term_seconds=*/ TERM,
        /*new_last_valid_unix_ts=*/ 0,
        new_buffer,
    );
    fixture
        .process_ixs(&[update_ix], &[&bob.insecure_clone()])
        .await
        .expect("owner update must succeed");
    fixture.refresh_blockhash().await;

    // Ask arrives and crosses the updated bid.
    provide_ask(&fixture, max_ltv, 100_000_000).await;

    let market = fixture.read_market_fixed().await;
    assert_eq!(
        market.matched_loan_sequence, 1,
        "the updated bid crosses the new ask"
    );
    fixture.crank_matched_loan_for_sub_vault(0).await.unwrap();
    let loan = fixture.read_loan(0).await;
    assert_eq!(
        loan.origination_ltv_bps, new_effective,
        "the updated buffer (1000) gated/stamped the cross: 8000 − 1000 = 7000"
    );
}

/// (5) VALIDATION: a buffer > 10_000 is rejected on every entry point
/// (place / update / convert) with `InvalidArgument`.
#[tokio::test]
async fn validation_rejects_oversize_buffer() {
    let fixture = MarketFixture::new().await;
    let bob = fund_borrower(&fixture).await;

    // place_order with an oversize buffer.
    let place_result = fixture
        .place_order_with_buffer(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BID_RATE,
            TERM,
            1_000_000,
            50_000_000,
            /*ltv_buffer_bps=*/ 10_001,
            RESIDUAL_MODE_REST,
        )
        .await;
    assert_custom_error!(place_result, YdeltaError::InvalidArgument);

    // Rest a valid bid so there's something to update.
    fixture.refresh_blockhash().await;
    fixture
        .place_order_with_buffer(
            &bob,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            BID_RATE,
            TERM,
            1_000_000,
            50_000_000,
            /*ltv_buffer_bps=*/ 0,
            RESIDUAL_MODE_REST,
        )
        .await
        .expect("a valid bid rests");
    fixture.refresh_blockhash().await;

    // update_order with an oversize buffer.
    let update_ix = update_order_instruction(
        &fixture.market.pubkey(),
        &bob.pubkey(),
        /*order_sequence=*/ 0,
        None,
        BID_RATE,
        TERM,
        0,
        /*new_ltv_buffer_bps=*/ 10_001,
    );
    let update_result = fixture
        .process_ixs(&[update_ix], &[&bob.insecure_clone()])
        .await;
    assert_custom_error!(update_result, YdeltaError::InvalidArgument);

    // convert_p2pool_to_fixed with an oversize buffer. The buffer bound
    // is checked before any account/loan loading, so a real P2Pool loan
    // is unnecessary — point the loan PDA at a fresh sequence and let the
    // require! fire first.
    let (debt_bank_lva, _) = Pubkey::find_program_address(
        &[b"liquidity_vault_auth", mainnet::usdc_bank().as_ref()],
        &marginfi_mocks::ID,
    );
    let convert_ix = ydelta::program::instruction_builders::convert_p2pool_to_fixed_instruction::convert_p2pool_to_fixed_instruction(
        &fixture.market.pubkey(),
        &bob.pubkey(),
        /*loan_sequence=*/ 0,
        &mainnet::usdc_mint(),
        &mainnet::usdc_bank(),
        &mainnet::usdc_liquidity_vault(),
        &debt_bank_lva,
        &[mainnet::usdc_oracle()],
        &mainnet::sol_bank(),
        &[mainnet::sol_oracle()],
        &spl_token::id(),
        &mainnet::marginfi_group(),
        &marginfi_mocks::ID,
        /*max_acceptable_rate_bps=*/ 10_000,
        /*ltv_buffer_bps=*/ 10_001,
        &bob.pubkey(),
    );
    let convert_result = fixture
        .process_ixs(&[convert_ix], &[&bob.insecure_clone()])
        .await;
    assert_custom_error!(convert_result, YdeltaError::InvalidArgument);
}
