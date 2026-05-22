//! End-to-end SBPF vault tests.
//!
//! Uses `MarketFixture` to spin up real marginfi banks + the ydelta
//! program, then exercises the full vault surface with actual CPIs.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Smoke test: vault create → profile create → deposit → withdraw
/// round trip with no fills. Validates that the `create_vault`,
/// `create_risk_profile`, `global_vault_deposit`, and `global_vault_withdraw`
/// processors compose end-to-end against a real marginfi bank.
#[tokio::test]
async fn vault_genesis_round_trip() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    // Mint some USDC to the depositor's ATA for the deposit.
    let depositor_token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        depositor_token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000, // 1000 USDC at 6 decimals
    );

    // 1. Create the USDC vault (admin = signer).
    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // 2. Create a single risk profile.
    fixture.refresh_blockhash().await;
    fixture
        .create_risk_profile(
            &admin,
            curator.pubkey(),
            /*max_ltv_bps=*/ 8_000,
            /*max_term_seconds=*/ 30 * 86_400,
        )
        .await
        .unwrap();

    // 3. Deposit 100 USDC.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();

    // SPL side: the deposit physically debited the depositor's ATA by the
    // full 100 USDC (not just bookkeeping).
    let ata_after_deposit = fixture.token_balance(depositor_token).await;
    assert_eq!(
        ata_after_deposit,
        1_000_000_000 - 100_000_000,
        "deposit must move exactly 100 USDC out of the depositor's ATA"
    );

    // Verify profile state reflects the deposit.
    let profile = fixture.read_risk_profile(0).await;
    let total_principal = profile.total_principal_atoms;
    let total_assets = profile.total_assets_atoms;
    let total_shares = profile.total_shares;
    assert_eq!(total_principal, 99_999_999);
    assert_eq!(total_assets, 99_999_999);
    // Genesis: 1 share = 1 atom.
    assert_eq!(total_shares, 99_999_999_u128);

    // 4. Withdraw 40 USDC. Use distinct share counts in two
    //    withdrawals so solana-program-test doesn't dedup them as
    //    replays of the same tx.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 40_000_000_u128)
        .await
        .unwrap();

    let profile = fixture.read_risk_profile(0).await;
    let total_shares = profile.total_shares;
    assert_eq!(total_shares, 59_999_999_u128);

    // Withdraw the rest (60).
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 59_999_999_u128)
        .await
        .unwrap();

    let profile = fixture.read_risk_profile(0).await;
    let total_shares = profile.total_shares;
    let total_assets = profile.total_assets_atoms;
    let total_principal = profile.total_principal_atoms;
    assert_eq!(total_shares, 0_u128);
    assert_eq!(total_assets, 0);
    assert_eq!(total_principal, 0);

    // SPL side: the full round-trip physically returned the deposited
    // atoms to the depositor's ATA. Realized-only exit with no loans →
    // the entire principal comes back, minus marginfi's deposit/withdraw
    // share-floor dust — and NEVER more than was deposited (no leak).
    let ata_final = fixture.token_balance(depositor_token).await;
    let returned = ata_final - ata_after_deposit;
    assert!(
        returned <= 100_000_000,
        "round-trip returned {} > 100_000_000 deposited — rounding leak",
        returned
    );
    assert!(
        100_000_000 - returned <= 3,
        "round-trip returned {} atoms, expected ~100_000_000 (floor dust only)",
        returned
    );
}

/// Reject `global_vault_withdraw` when shares > depositor's holdings.
#[tokio::test]
async fn global_vault_withdraw_rejects_overburn() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

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
        .create_risk_profile(&admin, curator.pubkey(), 8_000, 30 * 86_400)
        .await
        .unwrap();

    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();

    // Try to burn more shares than the depositor holds — must reject
    // with the exact InvalidArgument variant from the shares-balance gate.
    fixture.refresh_blockhash().await;
    let result = fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 200_000_000_u128)
        .await;
    crate::assert_custom_error!(result, ydelta::program::YdeltaError::InvalidArgument);
}

/// First `place_order_for_risk_profile` on a market auto-creates the
/// vault's per-(profile, market) `ClaimedSeat` — there is no explicit
/// claim-seat step in the quote-only model.
#[tokio::test]
async fn first_place_order_auto_creates_vault_seat() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    // provide_vault_liquidity runs create_vault → create_risk_profile →
    // global_vault_deposit → place_order_for_risk_profile. The vault
    // market-seat must not exist before the place_order call, and must
    // exist after it.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator,
            /*profile_id=*/ 0,
            /*max_ltv_bps=*/ 8_000,
            /*rate_bps=*/ 500,
            /*term_seconds=*/ 30 * 86_400,
            /*deposit_atoms=*/ 100_000_000,
        )
        .await;

    // The vault seat keyed by (global_vault, OWNER_KIND_RISK_PROFILE, 0)
    // exists — read_vault_seat panics if it is missing.
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_mint());
    let _seat = fixture.read_vault_seat(&gv, 0).await;

    // The resting risk-profile ask is on book.
    let market = fixture.read_market_fixed().await;
    assert_ne!(
        market.asks_best_index,
        hypertree::NIL,
        "place_order_for_risk_profile must rest an ask",
    );
    // Vault-idle invariant: nothing in flight yet, profile is fully
    // idle. Tightens this test to verify no spurious encumbrance.
    fixture.assert_vault_idle_invariant(0).await;
    let p = fixture.read_risk_profile(0).await;
    assert_eq!(p.deployed_principal_atoms, 0);
    assert_eq!(p.encumbered_in_orders_atoms, 0);
}

/// `create_risk_profile` rejects when signer is not global_vault_admin.
#[tokio::test]
async fn create_risk_profile_rejects_non_admin() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let interloper = fixture.create_trader().await;
    let curator = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    fixture.refresh_blockhash().await;
    let result = fixture
        .create_risk_profile(
            &interloper, // not the admin
            curator.pubkey(),
            8_000,
            30 * 86_400,
        )
        .await;
    // Loader's admin gate must surface the exact VaultAdminRequired variant.
    crate::assert_custom_error!(result, ydelta::program::YdeltaError::VaultAdminRequired);
}

/// A paused vault rejects vault-scoped state-mutating ixs (deposit,
/// withdraw, place-order), while the
/// two-step vault-admin transfer stays usable for recovery; unpausing
/// restores normal operation.
#[tokio::test]
async fn paused_vault_rejects_state_mutations_but_allows_admin_recovery() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator = fixture.create_trader().await;
    let new_admin = fixture.create_trader().await;

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
        .create_risk_profile(&admin, curator.pubkey(), 8_000, 30 * 86_400)
        .await
        .unwrap();

    // Seed an initial deposit while the vault is live (so a later
    // withdraw has shares to act on).
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 100_000_000)
        .await
        .unwrap();

    // Pause the vault.
    fixture.refresh_blockhash().await;
    fixture.set_vault_pause(&admin, true).await.unwrap();

    // Deposit must reject while paused — exact VaultPaused variant.
    fixture.refresh_blockhash().await;
    let deposit_res = fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 10_000_000)
        .await;
    crate::assert_custom_error!(deposit_res, ydelta::program::YdeltaError::VaultPaused);

    // Withdraw must reject while paused.
    fixture.refresh_blockhash().await;
    let withdraw_res = fixture
        .global_vault_withdraw(&depositor, depositor_token, 0, 10_000_000_u128)
        .await;
    crate::assert_custom_error!(withdraw_res, ydelta::program::YdeltaError::VaultPaused);

    // Curator place-order must reject while paused.
    fixture.refresh_blockhash().await;
    let place_res = fixture
        .place_order_for_risk_profile(&curator, 0, 500, 30 * 86_400, 0)
        .await;
    crate::assert_custom_error!(place_res, ydelta::program::YdeltaError::VaultPaused);

    // The two-step vault-admin transfer must STILL work while paused
    // (recovery surface). Initiate + accept the transfer to new_admin.
    fixture.refresh_blockhash().await;
    let transfer_ix =
        ydelta::program::instruction_builders::admin_transfer_instructions::transfer_global_vault_admin_instruction(
            &mainnet::usdc_mint(),
            &admin.pubkey(),
            &new_admin.pubkey(),
        );
    fixture
        .process(transfer_ix, &[&admin.insecure_clone()])
        .await
        .expect("transfer_global_vault_admin must work while paused");

    fixture.refresh_blockhash().await;
    let accept_ix =
        ydelta::program::instruction_builders::admin_transfer_instructions::accept_global_vault_admin_instruction(
            &mainnet::usdc_mint(),
            &new_admin.pubkey(),
        );
    fixture
        .process(accept_ix, &[&new_admin.insecure_clone()])
        .await
        .expect("accept_global_vault_admin must work while paused");

    // The new admin unpauses; deposits resume.
    fixture.refresh_blockhash().await;
    fixture.set_vault_pause(&new_admin, false).await.unwrap();

    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, depositor_token, 0, 10_000_000)
        .await
        .expect("deposit must succeed once the vault is unpaused");
}
