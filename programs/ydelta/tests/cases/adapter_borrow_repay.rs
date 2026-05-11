//! Phase-2 integration tests for the borrow + repay CPI paths through real
//! marginfi v0.1.8.
//!
//! Setup pattern:
//!
//! 1. Init a marginfi-account.
//! 2. Deposit USDC as collateral.
//! 3. Borrow SOL against that collateral. Marginfi rejects same-bank
//!    self-borrow with `OperationBorrowOnly`, so we use a *different*
//!    bank (SOL) for the liability side.
//! 4. Repay all SOL liability shares.
//!
//! Borrow is health-checking → `(bank, oracle)` for **every active balance**
//! in remaining accounts, ordered to match marginfi's iter over actives
//! (after `sort_balances()` — descending by bank_pk, so SOL pair comes
//! before USDC pair). Repay is not health-checking → no remaining accounts.
//!
//! Oracle freshness: the dumped `usdc_oracle.json` (Pyth-pull) and
//! `sol_oracle.json` (Switchboard-pull) carry their mainnet timestamps,
//! which the test ledger sees as stale relative to `Clock::unix_timestamp`.
//! Each test calls `fixture.refresh_oracle_freshness()` immediately
//! before the health-checked CPI; the helper rewrites the dumped
//! accounts' `publish_time` / `last_update_timestamp` / `result.slot` to
//! match the current ledger clock — same pattern as marginfi v2's
//! `set_pyth_oracle_timestamp`.

use borsh::BorshSerialize;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use marginfi_mocks::state::MarginfiAccount;
use ydelta_test_harness::{BorrowParams, DepositParams, HarnessInstruction, RepayParams};

use crate::test_utils::marginfi_fixture::{mainnet, AdapterFixture};

const TAG_INIT_MARGINFI_ACCOUNT: u8 = HarnessInstruction::InitMarginfiAccount as u8;
const TAG_DEPOSIT: u8 = HarnessInstruction::Deposit as u8;
const TAG_BORROW: u8 = HarnessInstruction::Borrow as u8;
const TAG_REPAY: u8 = HarnessInstruction::Repay as u8;

fn init_mfi_acct_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    fee_payer: Pubkey,
) -> Instruction {
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![
            AccountMeta::new_readonly(group, false),
            AccountMeta::new(marginfi_account, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(fee_payer, true),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(marginfi_mocks::ID, false),
        ],
        data: vec![TAG_INIT_MARGINFI_ACCOUNT],
    }
}

fn deposit_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    bank: Pubkey,
    signer_token: Pubkey,
    liquidity_vault: Pubkey,
    amount_atoms: u64,
) -> Instruction {
    let mut data = vec![TAG_DEPOSIT];
    DepositParams { amount_atoms }.serialize(&mut data).unwrap();
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![
            AccountMeta::new_readonly(group, false),
            AccountMeta::new(marginfi_account, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(bank, false),
            AccountMeta::new(signer_token, false),
            AccountMeta::new(liquidity_vault, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(marginfi_mocks::ID, false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)]
fn borrow_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    bank: Pubkey,
    destination_token: Pubkey,
    bank_liquidity_vault_authority: Pubkey,
    liquidity_vault: Pubkey,
    health_pairs: &[(Pubkey, Pubkey)],
    amount_atoms: u64,
) -> Instruction {
    let mut data = vec![TAG_BORROW];
    BorrowParams { amount_atoms }.serialize(&mut data).unwrap();
    let mut metas = vec![
        AccountMeta::new_readonly(group, false),
        AccountMeta::new(marginfi_account, false),
        AccountMeta::new_readonly(authority, true),
        AccountMeta::new(bank, false),
        AccountMeta::new(destination_token, false),
        AccountMeta::new_readonly(bank_liquidity_vault_authority, false),
        AccountMeta::new(liquidity_vault, false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(marginfi_mocks::ID, false),
    ];
    for (pair_bank, pair_oracle) in health_pairs {
        metas.push(AccountMeta::new_readonly(*pair_bank, false));
        metas.push(AccountMeta::new_readonly(*pair_oracle, false));
    }
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: metas,
        data,
    }
}

fn repay_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    bank: Pubkey,
    signer_token: Pubkey,
    liquidity_vault: Pubkey,
    shares: u128,
) -> Instruction {
    let mut data = vec![TAG_REPAY];
    RepayParams { shares }.serialize(&mut data).unwrap();
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![
            AccountMeta::new_readonly(group, false),
            AccountMeta::new(marginfi_account, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(bank, false),
            AccountMeta::new(signer_token, false),
            AccountMeta::new(liquidity_vault, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(marginfi_mocks::ID, false),
        ],
        data,
    }
}

/// Boot fixture, init marginfi-account, deposit collateral. Returns the
/// (fixture, marginfi_account_keypair, authority_keypair, signer_token_pubkey)
/// tuple ready to drive borrow/repay against.
async fn setup_with_collateral(
    initial_signer_balance: u64,
    deposit_amount: u64,
) -> (AdapterFixture, Keypair, Keypair, Pubkey) {
    let fixture = AdapterFixture::new_with_mainnet_marginfi().await;
    let marginfi_account = Keypair::new();
    let authority = fixture.payer.insecure_clone();

    fixture
        .process(
            init_mfi_acct_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                fixture.payer.pubkey(),
            ),
            &[&marginfi_account],
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let signer_token = Pubkey::new_unique();
    fixture.put_token_account(
        signer_token,
        mainnet::usdc_mint(),
        authority.pubkey(),
        initial_signer_balance,
    );
    fixture
        .process(
            deposit_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                mainnet::usdc_bank(),
                signer_token,
                mainnet::usdc_liquidity_vault(),
                deposit_amount,
            ),
            &[],
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    (fixture, marginfi_account, authority, signer_token)
}

#[tokio::test]
async fn borrow_sol_against_usdc_collateral_credits_liability_shares() {
    // Deposit 1000 USDC, borrow 0.01 SOL (worth ~$1). Plenty of headroom
    // even with 0.85-ish liability weight.
    let (fixture, marginfi_account, authority, signer_usdc_token) =
        setup_with_collateral(2_000_000_000, 1_000_000_000).await;

    // Set up signer-side wSOL token account to receive the borrowed SOL.
    let signer_sol_token = Pubkey::new_unique();
    fixture.put_token_account(
        signer_sol_token,
        mainnet::wsol_mint(),
        authority.pubkey(),
        0,
    );

    // Borrow runs marginfi's solvency check, which reads both bank
    // oracles. Refresh the dumped fixtures to the test ledger's clock so
    // they don't trip `SwitchboardStalePrice` / `PythStalePrice` —
    // identical pattern to marginfi v2's `set_pyth_oracle_timestamp`.
    fixture.refresh_oracle_freshness().await;

    // Borrow 0.01 SOL.
    let borrow_amount: u64 = 10_000_000; // 0.01 SOL (9 decimals)
    let result = fixture
        .process(
            borrow_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                mainnet::sol_bank(),
                signer_sol_token,
                mainnet::liquidity_vault_authority(mainnet::sol_bank()),
                mainnet::sol_liquidity_vault(),
                // Health-check remaining accounts: (bank, oracle) per
                // active balance, ordered to match marginfi's iter over
                // active balances. v0.1.8 mainnet binary iterates in
                // *slot order* (the order balances were added), not by
                // sorted bank_pk: USDC was deposited first, SOL borrowed
                // second.
                &[
                    (mainnet::sol_bank(), mainnet::sol_oracle()),
                    (mainnet::usdc_bank(), mainnet::usdc_oracle()),
                ],
                borrow_amount,
            ),
            &[],
        )
        .await;
    assert!(result.is_ok(), "borrow failed: {:?}", result);

    // Signer's wSOL token account grew by the borrow amount.
    let post_signer_sol = fixture.token_balance(signer_sol_token).await;
    assert_eq!(post_signer_sol, borrow_amount);

    // Marginfi-account now has a SOL-bank balance with non-zero liability.
    let data = fixture.account_data(marginfi_account.pubkey()).await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    let sol_bal = mfi
        .find_balance(&mainnet::sol_bank())
        .expect("SOL balance entry exists after borrow");
    assert!(
        sol_bal.liability_shares.to_i128_bits() > 0,
        "liability_shares didn't grow"
    );
    // USDC collateral side untouched.
    let usdc_bal = mfi
        .find_balance(&mainnet::usdc_bank())
        .expect("USDC balance entry exists");
    assert!(usdc_bal.asset_shares.to_i128_bits() > 0);
    let _ = signer_usdc_token;
}

#[tokio::test]
async fn deposit_borrow_repay_round_trips_through_real_marginfi() {
    // Deposit 1000 USDC, borrow 0.01 SOL, then repay all SOL liability.
    let (fixture, marginfi_account, authority, _signer_usdc_token) =
        setup_with_collateral(2_000_000_000, 1_000_000_000).await;

    let signer_sol_token = Pubkey::new_unique();
    fixture.put_token_account(
        signer_sol_token,
        mainnet::wsol_mint(),
        authority.pubkey(),
        0,
    );

    // Refresh oracles before borrow (health-checked).
    fixture.refresh_oracle_freshness().await;

    // Borrow 0.01 SOL.
    let borrow_amount: u64 = 10_000_000;
    fixture
        .process(
            borrow_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                mainnet::sol_bank(),
                signer_sol_token,
                mainnet::liquidity_vault_authority(mainnet::sol_bank()),
                mainnet::sol_liquidity_vault(),
                &[
                    (mainnet::sol_bank(), mainnet::sol_oracle()),
                    (mainnet::usdc_bank(), mainnet::usdc_oracle()),
                ],
                borrow_amount,
            ),
            &[],
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Read the post-borrow SOL liability shares.
    let liability_shares = {
        let data = fixture.account_data(marginfi_account.pubkey()).await;
        let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
        mfi.find_balance(&mainnet::sol_bank())
            .expect("SOL balance present after borrow")
            .liability_shares
            .to_i128_bits() as u128
    };
    assert!(liability_shares > 0);

    let pre_signer = fixture.token_balance(signer_sol_token).await;
    let pre_vault = fixture.token_balance(mainnet::sol_liquidity_vault()).await;

    // Repay all SOL liability shares.
    let result = fixture
        .process(
            repay_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                mainnet::sol_bank(),
                signer_sol_token,
                mainnet::sol_liquidity_vault(),
                liability_shares,
            ),
            &[],
        )
        .await;
    assert!(result.is_ok(), "repay failed: {:?}", result);

    let post_signer = fixture.token_balance(signer_sol_token).await;
    let post_vault = fixture.token_balance(mainnet::sol_liquidity_vault()).await;
    let signer_paid = pre_signer as i128 - post_signer as i128;
    let vault_received = post_vault as i128 - pre_vault as i128;
    assert!(signer_paid > 0, "signer balance didn't decrease on repay");
    assert!(
        (signer_paid - vault_received).abs() <= 1,
        "signer/vault deltas don't match: paid={}, received={}",
        signer_paid,
        vault_received
    );

    // SOL liability now ~0 within fp48 precision.
    let data = fixture.account_data(marginfi_account.pubkey()).await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    let final_liability = mfi
        .find_balance(&mainnet::sol_bank())
        .map(|b| b.liability_shares.to_i128_bits())
        .unwrap_or(0);
    assert!(
        final_liability.abs() <= 1 << 48,
        "expected near-zero residual liability, got {}",
        final_liability
    );
}
