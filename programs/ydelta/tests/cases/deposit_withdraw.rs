//! End-to-end deposit / withdraw tests against real marginfi state.
//! Each test boots `MarketFixture` (USDC-debt / wSOL-collateral market on
//! the mainnet marginfi group), synthesises a wSOL ATA for the trader, and
//! asserts the seat's collateral share accounting matches what
//! `MarginfiV18Adapter::amount_to_shares` reports against the live wSOL
//! (SOL) bank.
//!
//! Quote-only model: the debt asset (USDC) may NOT be deposited into a
//! market seat — `process_deposit` rejects `is_debt == true`. Lenders fund
//! via vaults; borrowers post collateral into the seat. These tests
//! therefore exercise the collateral axis (wSOL), routed to the
//! per-market *borrower* marginfi-account on the SOL bank.
//!
//! Why share-share comparisons rather than atom-share: `process_deposit`
//! credits the seat by the post-CPI share *delta* read off the
//! marginfi-account, which equals `amount_to_shares(amount_atoms)` modulo
//! marginfi's I80F48 truncation. Asserting against the adapter's
//! `amount_to_shares` (same truncation rule, same bank state) gives an
//! exact match.
//!
//! Amounts are kept small (≤ a few hundred thousand lamports): the mainnet
//! SOL liquidity-vault snapshot is near-full, so large deposits overflow
//! its u64 balance.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;

use marginfi_mocks::state::{Bank, MarginfiAccount};
use ydelta::math::{div_scale, to_scaled};
use ydelta::program::processor::withdraw::WithdrawParams;
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
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value).unwrap();
    let amount_fp48 = to_scaled(amount_atoms as u128).unwrap();
    div_scale(amount_fp48, asv_u128).unwrap()
}

/// Read the asset_shares balance for `bank_pk` on a marginfi account
/// snapshot. Returns 0 if no balance exists for that bank yet.
fn marginfi_asset_shares(mfi_data: &[u8], bank_pk: &Pubkey) -> u128 {
    let mfi = MarginfiAccount::try_from_account_data(mfi_data).unwrap();
    mfi.find_balance(bank_pk)
        .map(|b| wrapped_i80f48_to_u128(b.asset_shares).unwrap())
        .unwrap_or(0)
}

#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn deposit_credits_withdrawable() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 10_000;
    fixture
        .deposit(&trader, trader_wsol, /*is_debt=*/ false, deposit_atoms)
        .await
        .unwrap();

    // Marginfi accrues interest at the start of every deposit ix, so the
    // bank's `asset_share_value` at the time of share computation differs
    // slightly from any pre-CPI snapshot. The seat's
    // `collateral_withdrawable_shares` should match the borrower
    // marginfi-account's `asset_shares` for the SOL bank exactly — that's
    // the value the adapter read off the marginfi-account post-CPI and
    // credited to the seat.
    let mfi_data = fixture
        .account_data(fixture.borrower_marginfi_account_pubkey())
        .await;
    let post_shares = marginfi_asset_shares(&mfi_data, &mainnet::sol_bank());

    // And as a sanity check, that share count round-trips back to the
    // requested atoms (within ±1 share of marginfi's I80F48 rounding).
    let post_bank_data = fixture.account_data(mainnet::sol_bank()).await;
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
        seat.collateral_withdrawable_shares, post_shares,
        "seat collateral_withdrawable_shares should equal the share-delta \
         credited to the marginfi-account by `lending_account_deposit`"
    );
    assert_eq!(seat.debt_withdrawable_shares, 0);
}

#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_after_deposit_round_trips() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 10_000;
    fixture
        .deposit(&trader, trader_wsol, false, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let seat_after_deposit = fixture.read_seat(&trader.pubkey()).await;
    let post_deposit_balance = fixture.token_balance(trader_wsol).await;
    assert_eq!(post_deposit_balance, 100_000 - deposit_atoms);

    // Withdraw 60% of the deposit.
    let withdraw_atoms: u64 = 6_000;
    let bank_data = fixture.account_data(mainnet::sol_bank()).await;
    let expected_shares_burned = amount_to_shares_against(&bank_data, withdraw_atoms);

    fixture
        .withdraw(&trader, trader_wsol, false, withdraw_atoms)
        .await
        .unwrap();

    // Seat shares dropped by exactly the share quantity the adapter would
    // compute for the requested atom amount.
    let seat_after_withdraw = fixture.read_seat(&trader.pubkey()).await;
    assert_eq!(
        seat_after_deposit.collateral_withdrawable_shares
            - seat_after_withdraw.collateral_withdrawable_shares,
        expected_shares_burned,
        "seat shares decremented exactly by amount_to_shares({}) = {}",
        withdraw_atoms,
        expected_shares_burned
    );

    // Trader's wSOL balance increased — within ±1 atom of the requested
    // amount (marginfi's `assert_within_one_token` rounding tolerance).
    let post_withdraw_balance = fixture.token_balance(trader_wsol).await;
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

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    // Deposit a small amount.
    let deposit_atoms: u64 = 2_000;
    fixture
        .deposit(&trader, trader_wsol, false, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Try to withdraw more than the seat carries. The seat-side numeric
    // bound check fires before the marginfi CPI, returning
    // `InsufficientWithdrawableBalance`.
    let result = fixture
        .withdraw(&trader, trader_wsol, false, deposit_atoms * 2)
        .await;
    assert_custom_error!(result, YdeltaError::InsufficientWithdrawableBalance);
}

/// `WithdrawParams` must round-trip through borsh including the
/// `withdraw_all` flag. Pure (de)serialization — no fixture.
#[test]
fn withdraw_params_borsh_round_trip() {
    for &(amount, hint, all) in &[
        (5_000_000u64, None, false),
        (0u64, Some(7i32 as hypertree::DataIndex), true),
        (123u64, Some(0), false),
    ] {
        let original = WithdrawParams {
            amount_atoms: amount,
            trader_index_hint: hint,
            withdraw_all: all,
        };
        let mut data = Vec::new();
        original.serialize(&mut data).unwrap();
        let decoded = WithdrawParams::try_from_slice(&data).unwrap();
        assert_eq!(decoded.amount_atoms, original.amount_atoms);
        assert_eq!(decoded.trader_index_hint, original.trader_index_hint);
        assert_eq!(decoded.withdraw_all, original.withdraw_all);
    }
}

/// `withdraw_all` drains the seat's entire withdrawable balance on
/// the selected side. After the call the seat's
/// `collateral_withdrawable_shares` must be exactly 0, and the wallet
/// must have received the full deposited balance back (within marginfi's
/// ±1-atom rounding tolerance, and never *more* than the deposit).
#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_all_drains_seat_to_zero() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 10_000;
    fixture
        .deposit(&trader, trader_wsol, false, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let seat_after_deposit = fixture.read_seat(&trader.pubkey()).await;
    assert!(
        seat_after_deposit.collateral_withdrawable_shares > 0,
        "deposit should have credited withdrawable shares"
    );
    let post_deposit_balance = fixture.token_balance(trader_wsol).await;

    // Drain everything — amount is ignored under withdraw_all.
    fixture
        .withdraw_all(&trader, trader_wsol, false)
        .await
        .unwrap();

    // Seat's collateral side is fully burned to 0.
    let seat_after_withdraw = fixture.read_seat(&trader.pubkey()).await;
    assert_eq!(
        seat_after_withdraw.collateral_withdrawable_shares, 0,
        "withdraw_all must burn the seat's entire collateral_withdrawable_shares"
    );
    assert_eq!(seat_after_withdraw.debt_withdrawable_shares, 0);

    // Wallet got the full balance back, never more than deposited.
    let post_withdraw_balance = fixture.token_balance(trader_wsol).await;
    let received = post_withdraw_balance - post_deposit_balance;
    assert!(
        received <= deposit_atoms,
        "withdraw_all paid {} > deposited {} — rounding leak",
        received,
        deposit_atoms
    );
    let drift = (received as i128 - deposit_atoms as i128).abs();
    assert!(
        drift <= 1,
        "withdraw_all received {} atoms, expected ~{} (drift {})",
        received,
        deposit_atoms,
        drift
    );
}

/// `withdraw_all` on a seat with nothing withdrawable is a clean
/// error (`InsufficientWithdrawableBalance`), consistent with the
/// explicit-amount path's `amount > 0` guard.
#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_all_empty_seat_errors() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    // No deposit — the seat carries 0 withdrawable shares.
    let result = fixture.withdraw_all(&trader, trader_wsol, false).await;
    assert_custom_error!(result, YdeltaError::InsufficientWithdrawableBalance);
}

/// The seat-share debit and the atoms paid must agree in the
/// protocol's favour: the user is paid `min(actual_atoms, expected_atoms)`,
/// where `expected_atoms` is what the *debited* shares are worth. The
/// adapter's ±1-atom drift therefore can never overpay the user. We pin
/// the direction: across a deposit + explicit withdraw, the wallet never
/// receives more atoms than the shares burned off the seat are worth.
#[tokio::test]
#[cfg(feature = "test-sbf")]
async fn withdraw_never_overpays_debited_shares() {
    let fixture = MarketFixture::new().await;
    let trader = fixture.create_trader().await;
    fixture.claim_seat(&trader).await;

    let trader_wsol = Pubkey::new_unique();
    fixture.put_wsol_token_account(trader_wsol, trader.pubkey(), 100_000);
    fixture.refresh_blockhash().await;

    let deposit_atoms: u64 = 10_000;
    fixture
        .deposit(&trader, trader_wsol, false, deposit_atoms)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let post_deposit_balance = fixture.token_balance(trader_wsol).await;

    // Withdraw an odd amount that is unlikely to be share-exact, so the
    // floor/round behaviour is exercised.
    let withdraw_atoms: u64 = 3_337;
    let bank_data = fixture.account_data(mainnet::sol_bank()).await;
    let shares_burned = amount_to_shares_against(&bank_data, withdraw_atoms);

    fixture
        .withdraw(&trader, trader_wsol, false, withdraw_atoms)
        .await
        .unwrap();

    // `expected_atoms` = what the floored shares are worth: shares * ASV,
    // floored. The payout must never exceed this.
    let bank = Bank::try_from_account_data(&bank_data).unwrap();
    let asv_u128 = wrapped_i80f48_to_u128(bank.asset_share_value).unwrap();
    let expected_atoms_fp = ydelta::math::mul_scale(shares_burned, asv_u128).unwrap();
    let expected_atoms = ydelta::math::from_scaled_floor(expected_atoms_fp) as u64;

    let received = fixture.token_balance(trader_wsol).await - post_deposit_balance;
    assert!(
        received <= expected_atoms,
        "paid {} atoms but debited shares are only worth {} — rounding leak",
        received,
        expected_atoms
    );
}
