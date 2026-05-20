//! Integration test for the deposit CPI path. Uses real marginfi
//! mainnet state (group + USDC bank + USDC mint + liquidity vault) replayed
//! into ProgramTest, plus a freshly-initialised marginfi-account, plus a
//! synthesised signer USDC account with a starting balance. Exercises the
//! deposit flow end-to-end:
//!
//!   harness ix
//!     -> MarginfiV18Adapter::deposit
//!       -> CPI marginfi.so lending_account_deposit
//!         -> SPL token transfer signer_token -> liquidity_vault
//!         -> marginfi state updates
//!     <- shares minted (read from MarginfiAccount.balance pre/post)

use borsh::BorshSerialize;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use marginfi_mocks::state::{Bank, MarginfiAccount};
use ydelta_test_harness::{DepositParams, HarnessInstruction};

use crate::test_utils::marginfi_fixture::{mainnet, AdapterFixture};

const HARNESS_TAG_INIT_MARGINFI_ACCOUNT: u8 = HarnessInstruction::InitMarginfiAccount as u8;
const HARNESS_TAG_DEPOSIT: u8 = HarnessInstruction::Deposit as u8;

/// Build the harness's `InitMarginfiAccount` ix. Account layout per the
/// harness handler: `[group, marginfi_account (signer), authority (signer),
/// fee_payer (signer), system_program, marginfi_program]`.
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

/// Build the harness's `Deposit` ix. Layout per the harness handler:
/// `[group, marginfi_account, authority, bank, signer_token, liquidity_vault,
/// token_program, marginfi_program]` (no remaining accounts; deposit
/// doesn't run a health check).
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

#[tokio::test]
async fn deposit_into_real_mainnet_usdc_bank_credits_shares() {
    let fixture = AdapterFixture::new_with_mainnet_marginfi().await;

    // Fresh marginfi-account keypair (signer in the init ix). Authority is
    // the fee payer keypair we already control.
    let marginfi_account = Keypair::new();
    let authority = fixture.payer.insecure_clone();

    // Init the marginfi-account via real CPI through the harness.
    // Marginfi's `MarginfiAccountInitialize` uses Anchor's `init`
    // constraint, which creates the account internally — don't pre-allocate.
    let ix = init_mfi_acct_ix(
        mainnet::marginfi_group(),
        marginfi_account.pubkey(),
        authority.pubkey(),
        fixture.payer.pubkey(),
    );
    fixture.process(ix, &[&marginfi_account]).await.unwrap();
    fixture.refresh_blockhash().await;

    // Verify the marginfi-account got initialised by marginfi (discriminator
    // populated, group/authority set).
    {
        let data = fixture.account_data(marginfi_account.pubkey()).await;
        let mfi = MarginfiAccount::try_from_account_data(&data)
            .expect("marginfi initialised the account");
        assert_eq!(mfi.group, mainnet::marginfi_group());
        assert_eq!(mfi.authority, authority.pubkey());
    }

    // Mint USDC to a fresh signer-side token account by synthesising it
    // directly. (Real spl-token mint_to would need the USDC mint authority
    // private key, which we don't have. set_account bypasses by writing the
    // amount field directly — marginfi only reads the balance pre/post for
    // its own accounting, doesn't validate provenance.)
    let signer_token = Pubkey::new_unique();
    fixture.put_token_account(
        signer_token,
        mainnet::usdc_mint(),
        authority.pubkey(),
        10_000_000, // 10 USDC (6 decimals)
    );

    // Snapshot vault + signer balances pre-deposit.
    let pre_signer = fixture.token_balance(signer_token).await;
    let pre_vault = fixture.token_balance(mainnet::usdc_liquidity_vault()).await;
    assert_eq!(pre_signer, 10_000_000);

    // Deposit 1 USDC.
    let amount: u64 = 1_000_000;
    let ix = deposit_ix(
        mainnet::marginfi_group(),
        marginfi_account.pubkey(),
        authority.pubkey(),
        mainnet::usdc_bank(),
        signer_token,
        mainnet::usdc_liquidity_vault(),
        amount,
    );
    fixture.process(ix, &[]).await.expect("deposit succeeds");

    // Token balances moved by exactly `amount` atoms.
    assert_eq!(
        fixture.token_balance(signer_token).await,
        pre_signer - amount
    );
    assert_eq!(
        fixture.token_balance(mainnet::usdc_liquidity_vault()).await,
        pre_vault + amount
    );

    // marginfi-account now has an active balance for the USDC bank with
    // non-zero asset_shares.
    let data = fixture.account_data(marginfi_account.pubkey()).await;
    let mfi = MarginfiAccount::try_from_account_data(&data).unwrap();
    let bal = mfi
        .find_balance(&mainnet::usdc_bank())
        .expect("bank balance present after deposit");
    assert!(bal.asset_shares.to_i128_bits() > 0);

    // Cross-check via Bank's current asset_share_value: shares × asv ≈ amount.
    // Sanity-only since marginfi accrues interest mid-tx; ±1 atom tolerance.
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;
    let bank = Bank::try_from_account_data(&bank_data).unwrap();
    let asv_bits = bank.asset_share_value.to_i128_bits() as u128;
    let shares_u128 = bal.asset_shares.to_i128_bits() as u128;
    let atoms_recovered =
        ydelta::math::from_scaled_floor(ydelta::math::mul_scale(shares_u128, asv_bits).unwrap());
    let diff = (atoms_recovered as i128 - amount as i128).abs();
    assert!(diff <= 1, "atom round-trip diff {} > 1", diff);
}
