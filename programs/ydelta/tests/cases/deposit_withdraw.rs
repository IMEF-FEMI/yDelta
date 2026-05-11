//! End-to-end deposit / withdraw tests against real marginfi state.
//! Each test boots `MarketFixture` (USDC-debt / wSOL-collateral market on
//! the mainnet marginfi group), pre-mints USDC into the trader's
//! synthesised ATA, and asserts the seat's share accounting matches what
//! `MarginfiV18Adapter::amount_to_shares` reports against the live USDC
//! bank.
//!
//! Why share-share comparisons rather than atom-share: `process_deposit`
//! credits the seat by the post-CPI share *delta* read off the
//! marginfi-account, which equals `amount_to_shares(amount_atoms)` modulo
//! marginfi's I80F48 truncation. Asserting against the adapter's
//! `amount_to_shares` (same truncation rule, same bank state) gives an
//! exact match.

use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::{Bank, MarginfiAccount};
use ydelta::math::{div_scale, to_scaled};
use ydelta::program::YdeltaError;
use ydelta::protocol::marginfi::wrapped_i80f48_to_u128;

use crate::assert_custom_error;
use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

/// Mirror `MarginfiV18Adapter::amount_to_shares` against a snapshot of the
/// bank account at the time of the call. We can't call the adapter
/// directly from the test process (it borrows AccountInfo), so re-derive
/// from the bank's raw account data the same way the adapter does.
fn amount_to_shares_against(bank_data: &[u8], amount_atoms: u64) -> u128 {
    let bank = Bank::try_from_account_data(bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value);
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

/// Read the asset_shares balance for `bank_pk` on a marginfi account
/// snapshot. Returns 0 if no balance exists for that bank yet.
fn marginfi_asset_shares(mfi_data: &[u8], bank_pk: &Pubkey) -> u128 {
    let mfi = MarginfiAccount::try_from_account_data(mfi_data).unwrap();
    mfi.find_balance(bank_pk)
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares))
        .unwrap_or(0)
}

#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn deposit_credits_withdrawable() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        trader_usdc,
        mainnet::usdc_mint(),
        trader.pubkey(),
        10_000_000, // 10 USDC (6 decimals)
    );
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 5_000_000; // 5 USDC
    fixture
        .deposit(&trader, trader_usdc, /*is_debt=*/ true, deposit_atoms)
        .await
        .unwrap();

    // Marginfi accrues interest at the start of every deposit ix, so the
    // bank's `asset_share_value` at the time of share computation differs
    // slightly from any pre-CPI snapshot. The seat's `_withdrawable_shares`
    // should match the marginfi-account's `asset_shares` for this bank
    // exactly — that's the value the adapter read off the marginfi-account
    // post-CPI and credited to the seat.
    let mfi_data = fixture
        .account_data(fixture.marginfi_account_pubkey())
        .await;
    let post_shares = marginfi_asset_shares(&mfi_data, &mainnet::usdc_bank());

    // And as a sanity check, that share count round-trips back to the
    // requested atoms (within ±1 atom of marginfi's I80F48 rounding).
    let post_bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let round_tripped = amount_to_shares_against(&post_bank_data, deposit_atoms);
    let drift = (post_shares as i128 - round_tripped as i128).abs();
    assert!(
        drift <= (1u128 << 48) as i128,
        "post-deposit shares ({}) drift > 1 share from amount_to_shares({}) = {} (drift {})",
        post_shares,
        deposit_atoms,
        round_tripped,
        drift
    );

    let seat = fixture.read_seat(&trader.pubkey()).await;
    assert_eq!(
        seat.debt_withdrawable_shares, post_shares,
        "seat debt_withdrawable_shares should equal the share-delta credited \
         to the marginfi-account by `lending_account_deposit`"
    );
    assert_eq!(seat.collateral_withdrawable_shares, 0);
}

#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_after_deposit_round_trips() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        trader_usdc,
        mainnet::usdc_mint(),
        trader.pubkey(),
        10_000_000,
    );
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 5_000_000;
    fixture
        .deposit(&trader, trader_usdc, true, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let seat_after_deposit = fixture.read_seat(&trader.pubkey()).await;
    let post_deposit_balance = fixture.token_balance(trader_usdc).await;
    assert_eq!(post_deposit_balance, 10_000_000 - deposit_atoms);

    // Withdraw 60% of the deposit.
    let withdraw_atoms: u64 = 3_000_000;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let expected_shares_burned = amount_to_shares_against(&bank_data, withdraw_atoms);

    fixture
        .withdraw(&trader, trader_usdc, true, withdraw_atoms)
        .await
        .unwrap();

    // Seat shares dropped by exactly the share quantity the adapter would
    // compute for the requested atom amount.
    let seat_after_withdraw = fixture.read_seat(&trader.pubkey()).await;
    assert_eq!(
        seat_after_deposit.debt_withdrawable_shares - seat_after_withdraw.debt_withdrawable_shares,
        expected_shares_burned,
        "seat shares decremented exactly by amount_to_shares({}) = {}",
        withdraw_atoms,
        expected_shares_burned
    );

    // Trader's USDC balance increased — within ±1 atom of the requested
    // amount (marginfi's `assert_within_one_token` rounding tolerance).
    let post_withdraw_balance = fixture.token_balance(trader_usdc).await;
    let received = post_withdraw_balance - post_deposit_balance;
    let drift = (received as i128 - withdraw_atoms as i128).abs();
    assert!(
        drift <= 1,
        "received {} atoms, expected ~{} (drift {})",
        received,
        withdraw_atoms,
        drift
    );
}

#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_above_balance_fails() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_usdc = Pubkey::new_unique();
    fixture.put_token_account(
        trader_usdc,
        mainnet::usdc_mint(),
        trader.pubkey(),
        10_000_000,
    );
    fixture.refresh_blockhash().await;

    // Deposit a small amount.
    let deposit_atoms: u64 = 1_000_000;
    fixture
        .deposit(&trader, trader_usdc, true, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Try to withdraw more than the seat carries. The seat-side numeric
    // bound check fires before the marginfi CPI, returning
    // `InsufficientWithdrawableBalance`.
    let result = fixture
        .withdraw(&trader, trader_usdc, true, deposit_atoms * 2)
        .await;
    assert_custom_error!(result, YdeltaError::InsufficientWithdrawableBalance);
}
