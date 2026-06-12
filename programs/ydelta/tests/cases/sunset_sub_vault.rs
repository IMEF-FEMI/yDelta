use solana_sdk::signer::Signer;
use ydelta::program::instruction_builders::admin_cancel_sub_vault_order_instruction::admin_cancel_sub_vault_order_instruction;
use ydelta::program::instruction_builders::cancel_order_for_sub_vault_instruction::cancel_order_for_sub_vault_instruction;
use ydelta::program::instruction_builders::place_order_for_sub_vault_instruction::place_order_for_sub_vault_instruction;
use ydelta::program::instruction_builders::remove_sub_vault_instruction::remove_sub_vault_instruction;
use ydelta::program::instruction_builders::resume_sub_vault_instruction::resume_sub_vault_instruction;
use ydelta::program::instruction_builders::sunset_sub_vault_instruction::sunset_sub_vault_instruction;
use ydelta::program::YdeltaError;

use crate::test_utils::{mainnet, MarketFixture};

async fn setup_vault_with_profile(
    fixture: &MarketFixture,
    admin: &solana_sdk::signature::Keypair,
    curator_pk: solana_program::pubkey::Pubkey,
) {
    fixture.refresh_blockhash().await;
    fixture.create_vault(admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(admin, curator_pk, Some(8_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
}

async fn sunset(fixture: &MarketFixture, admin: &solana_sdk::signature::Keypair, sub_vault_id: u16) {
    let ix = sunset_sub_vault_instruction(&mainnet::usdc_bank(), &admin.pubkey(), sub_vault_id);
    fixture.process(ix, &[admin]).await.unwrap();
    fixture.refresh_blockhash().await;
}

#[tokio::test]
async fn sunset_blocks_new_deposits() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, depositor.pubkey()).await;
    sunset(&fixture, &admin, 1).await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000,
    );
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 500_000)
        .await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultSunset);
}

#[tokio::test]
async fn sunset_blocks_new_orders() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;
    sunset(&fixture, &admin, 1).await;

    let result = fixture
        .place_order_for_sub_vault(&curator, 1, 500, 30 * 86_400, 0)
        .await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultSunset);
}

#[tokio::test]
async fn sunset_blocks_param_update() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;
    sunset(&fixture, &admin, 1).await;

    // v1 D15: update is curator-gated, so even the CURATOR's update must
    // reject on a sunset sub-vault…
    let result = fixture
        .update_sub_vault(&curator, 1, Some(7_500), None)
        .await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultSunset);

    // …and a non-curator (the vault admin) is rejected at the curator
    // gate before the sunset check is even reached.
    fixture.refresh_blockhash().await;
    let result = fixture
        .update_sub_vault(&admin, 1, Some(7_500), None)
        .await;
    crate::assert_custom_error!(result, YdeltaError::VaultCuratorRequired);
}

#[tokio::test]
async fn sunset_allows_withdrawals() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, depositor.pubkey()).await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 500_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    sunset(&fixture, &admin, 1).await;

    let shares = fixture.read_sub_vault(1).await.total_shares;
    assert!(shares > 0);

    fixture
        .global_vault_withdraw(&depositor, depositor_token, 1, shares)
        .await
        .expect("withdraw must succeed during sunset");
}

#[tokio::test]
async fn sunset_allows_curator_cancel() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;

    fixture
        .place_order_for_sub_vault(&curator, 1, 500, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    sunset(&fixture, &admin, 1).await;

    let cancel_ix = cancel_order_for_sub_vault_instruction(
        &mainnet::usdc_bank(),
        &fixture.market.pubkey(),
        &admin.pubkey(),
        &curator.pubkey(),
        1,
    );
    fixture
        .process_ixs(&[cancel_ix], &[&admin, &curator])
        .await
        .expect("curator cancel must succeed during sunset");
}

#[tokio::test]
async fn resume_restores_active_state() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, depositor.pubkey()).await;
    sunset(&fixture, &admin, 1).await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000,
    );
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 500_000)
        .await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultSunset);
    fixture.refresh_blockhash().await;

    let resume_ix =
        resume_sub_vault_instruction(&mainnet::usdc_bank(), &admin.pubkey(), 1);
    fixture.process(resume_ix, &[&admin]).await.unwrap();
    fixture.refresh_blockhash().await;

    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 500_000)
        .await
        .expect("deposit must succeed after resume");
}

#[tokio::test]
async fn admin_cancel_requires_sunset() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;

    fixture
        .place_order_for_sub_vault(&curator, 1, 500, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let admin_cancel_ix = admin_cancel_sub_vault_order_instruction(
        &mainnet::usdc_bank(),
        &fixture.market.pubkey(),
        &admin.pubkey(),
        1,
    );
    let result = fixture.process(admin_cancel_ix, &[&admin]).await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultNotSunset);
}

#[tokio::test]
async fn admin_cancel_works_during_sunset() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;

    fixture
        .place_order_for_sub_vault(&curator, 1, 500, 30 * 86_400, 0)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    sunset(&fixture, &admin, 1).await;

    let admin_cancel_ix = admin_cancel_sub_vault_order_instruction(
        &mainnet::usdc_bank(),
        &fixture.market.pubkey(),
        &admin.pubkey(),
        1,
    );
    fixture
        .process(admin_cancel_ix, &[&admin])
        .await
        .expect("admin cancel must succeed during sunset");
}

#[tokio::test]
async fn remove_requires_sunset() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;

    let remove_ix = remove_sub_vault_instruction(&mainnet::usdc_bank(), &admin.pubkey(), 1);
    let result = fixture.process(remove_ix, &[&admin]).await;
    crate::assert_custom_error!(result, YdeltaError::SubVaultNotSunset);
}

#[tokio::test]
async fn remove_succeeds_after_sunset() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    setup_vault_with_profile(&fixture, &admin, curator.pubkey()).await;
    sunset(&fixture, &admin, 1).await;

    let remove_ix = remove_sub_vault_instruction(&mainnet::usdc_bank(), &admin.pubkey(), 1);
    fixture
        .process(remove_ix, &[&admin])
        .await
        .expect("remove must succeed after sunset");
}

#[tokio::test]
async fn sunset_skips_matching() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let borrower = fixture.create_trader().await;

    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, depositor.pubkey(), Some(8_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 1, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    let curator_ix = place_order_for_sub_vault_instruction(
        &mainnet::usdc_bank(),
        &fixture.market.pubkey(),
        &fixture.payer.pubkey(),
        &depositor.pubkey(),
        &mainnet::usdc_bank(),
        &mainnet::marginfi_group(),
        1,
        0,
    );
    fixture
        .process_ixs(&[curator_ix], &[&depositor])
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    sunset(&fixture, &admin, 1).await;

    fixture.claim_seat(&borrower).await;
    let borrower_wsol = solana_program::pubkey::Pubkey::new_unique();
    fixture.put_wsol_token_account(borrower_wsol, borrower.pubkey(), 200_000_000);
    fixture.refresh_blockhash().await;
    fixture
        .deposit(&borrower, borrower_wsol, false, 100_000_000)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let pre = fixture.read_sub_vault(1).await;
    assert_eq!(pre.encumbered_in_orders_atoms, 0);

    let principal_atoms: u64 = 1_000_000;
    fixture
        .place_order_with_flags(
            &borrower,
            ydelta::state::Side::Bid,
            ydelta::state::OrderType::Limit,
            800,
            30 * 86_400,
            principal_atoms,
            50_000_000,
            0,
        )
        .await
        .unwrap();
    fixture.refresh_blockhash().await;

    let post = fixture.read_sub_vault(1).await;
    assert_eq!(
        post.encumbered_in_orders_atoms, 0,
        "borrower bid must NOT match against a sunset profile's ask"
    );
}
