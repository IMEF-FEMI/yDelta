//! Phase-2 integration test for the withdraw CPI path. Builds on the
//! deposit fixture: deposit 1 USDC, then withdraw a fraction of the shares
//! back to a destination token account. Withdraw is health-checking, so
//! `remaining_accounts` carries `(bank, oracle)` for the marginfi-account's
//! sole active balance.

use borsh::BorshSerialize;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use marginfi_mocks::state::MarginfiAccount;
use ydelta_test_harness::{DepositParams, HarnessInstruction, WithdrawParams};

use crate::test_utils::marginfi_fixture::{mainnet, AdapterFixture};

const HARNESS_TAG_INIT_MARGINFI_ACCOUNT: u8 = HarnessInstruction::InitMarginfiAccount as u8;
const HARNESS_TAG_DEPOSIT: u8 = HarnessInstruction::Deposit as u8;
const HARNESS_TAG_WITHDRAW: u8 = HarnessInstruction::Withdraw as u8;

fn init_mfi_acct_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    fee_payer: Pubkey,
) -> Instruction {
    let data = vec![HARNESS_TAG_INIT_MARGINFI_ACCOUNT];
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
        data,
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
    let mut data = vec![HARNESS_TAG_DEPOSIT];
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
fn withdraw_ix(
    group: Pubkey,
    marginfi_account: Pubkey,
    authority: Pubkey,
    bank: Pubkey,
    destination_token: Pubkey,
    bank_liquidity_vault_authority: Pubkey,
    liquidity_vault: Pubkey,
    oracle: Pubkey,
    shares: u128,
) -> Instruction {
    let mut data = vec![HARNESS_TAG_WITHDRAW];
    WithdrawParams { shares }.serialize(&mut data).unwrap();
    Instruction {
        program_id: ydelta_test_harness::ID,
        accounts: vec![
            // 8 explicit (matches WithdrawAccounts IDL list)
            AccountMeta::new_readonly(group, false),
            AccountMeta::new(marginfi_account, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(bank, false),
            AccountMeta::new(destination_token, false),
            AccountMeta::new_readonly(bank_liquidity_vault_authority, false),
            AccountMeta::new(liquidity_vault, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            // marginfi program (slice index 8)
            AccountMeta::new_readonly(marginfi_mocks::ID, false),
            // remaining: (bank, oracle) for the sole active balance.
            AccountMeta::new_readonly(bank, false),
            AccountMeta::new_readonly(oracle, false),
        ],
        data,
    }
}

#[tokio::test]
async fn deposit_then_withdraw_round_trips_through_real_marginfi() {
    let fixture = AdapterFixture::new_with_mainnet_marginfi().await;

    let marginfi_account = Keypair::new();
    let authority = fixture.payer.insecure_clone();

    // Init marginfi-account.
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

    // Mint USDC to a signer-side token account (10 USDC), then deposit 5 USDC.
    let signer_token = Pubkey::new_unique();
    fixture.put_token_account(
        signer_token,
        mainnet::usdc_mint(),
        authority.pubkey(),
        10_000_000,
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
                5_000_000,
            ),
            &[],
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    // Read the post-deposit share balance — that's what we'll withdraw.
    let shares_to_withdraw = {
        let data = fixture.account_data(marginfi_account.pubkey()).await;
        let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
        mfi.find_balance(&mainnet::usdc_bank())
            .expect("balance present after deposit")
            .asset_shares
            .to_i128_bits() as u128
    };
    assert!(shares_to_withdraw > 0);

    // Snapshot balances pre-withdraw.
    let pre_signer = fixture.token_balance(signer_token).await;
    let pre_vault = fixture.token_balance(mainnet::usdc_liquidity_vault()).await;

    // Withdraw all shares back to the same signer token account.
    let result = fixture
        .process(
            withdraw_ix(
                mainnet::marginfi_group(),
                marginfi_account.pubkey(),
                authority.pubkey(),
                mainnet::usdc_bank(),
                signer_token,
                mainnet::liquidity_vault_authority(mainnet::usdc_bank()),
                mainnet::usdc_liquidity_vault(),
                mainnet::usdc_oracle(),
                shares_to_withdraw,
            ),
            &[],
        )
        .await;
    assert!(result.is_ok(), "withdraw failed: {:?}", result);

    // Token balances moved back. Allow ±1 atom drift from share-truncation.
    let post_signer = fixture.token_balance(signer_token).await;
    let post_vault = fixture.token_balance(mainnet::usdc_liquidity_vault()).await;
    let signer_delta = post_signer as i128 - pre_signer as i128;
    let vault_delta = pre_vault as i128 - post_vault as i128;
    assert!(signer_delta > 0, "signer balance didn't grow on withdraw");
    assert!(
        (signer_delta - vault_delta).abs() <= 1,
        "signer/vault deltas don't match: signer={}, vault={}",
        signer_delta,
        vault_delta
    );

    // marginfi-account balance for USDC should now be zero (or near-zero
    // due to integer truncation in marginfi's share math).
    let data = fixture.account_data(marginfi_account.pubkey()).await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    let final_shares = mfi
        .find_balance(&mainnet::usdc_bank())
        .map(|b| b.asset_shares.to_i128_bits())
        .unwrap_or(0);
    // Allow up to ~SCALE residual (= 1 unit of fp48 precision = 1 share-atom).
    assert!(
        final_shares.abs() <= 1 << 48,
        "expected near-zero residual, got {}",
        final_shares
    );
}
