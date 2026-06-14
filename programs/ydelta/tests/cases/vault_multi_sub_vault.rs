//! Multi-sub-vault vault tests.
//!
//! The enforcement mechanism is the market-side `ClaimedSeat` keyed
//! by `(vault, owner_kind=Vault, sub_vault_id)` plus
//! `SubVaultOrderRef` keyed by `(market, sub_vault_id)` on the
//! vault side. These cases exercise that surface with N>1 sub_vaults.

use solana_sdk::signer::Signer;

use crate::test_utils::{mainnet, MarketFixture};

/// Two sub_vaults in one vault, two distinct depositors. Each deposits
/// into their own sub_vault; withdrawing from one doesn't affect the
/// other's share count or asset total.
#[tokio::test]
async fn two_sub_vaults_independent_deposit_state() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor_a = fixture.create_trader().await;
    let depositor_b = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    let token_a = fixture.signer_debt_token(&depositor_a.pubkey());
    let token_b = fixture.signer_debt_token(&depositor_b.pubkey());
    fixture.put_token_account(
        token_a,
        mainnet::usdc_mint(),
        depositor_a.pubkey(),
        1_000_000_000,
    );
    fixture.put_token_account(
        token_b,
        mainnet::usdc_mint(),
        depositor_b.pubkey(),
        1_000_000_000,
    );

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // SubVault 0 — curator_a, 50% LTV cap, 30-day max term.
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_a.pubkey(), Some(5_000), 30 * 86_400)
        .await
        .unwrap();

    // SubVault 1 — curator_b, 80% LTV cap, 90-day max term. Different
    // policy, different curator.
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_b.pubkey(), Some(8_000), 90 * 86_400)
        .await
        .unwrap();

    // Verify both sub_vaults exist with distinct curators + policies.
    let sub_vault_a = fixture.read_sub_vault(1).await;
    let sub_vault_b = fixture.read_sub_vault(2).await;
    assert_eq!(sub_vault_a.sub_vault_id, 1);
    assert_eq!(sub_vault_b.sub_vault_id, 2);
    assert_eq!(sub_vault_a.curator, curator_a.pubkey());
    assert_eq!(sub_vault_b.curator, curator_b.pubkey());
    assert_eq!(sub_vault_a.max_ltv_bps, 5_000);
    assert_eq!(sub_vault_b.max_ltv_bps, 8_000);
    assert_eq!(sub_vault_a.max_term_seconds, 30 * 86_400);
    assert_eq!(sub_vault_b.max_term_seconds, 90 * 86_400);

    // Depositor A deposits 100 USDC into sub_vault 0.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_a, token_a, 1, 100_000_000)
        .await
        .unwrap();

    // Depositor B deposits 250 USDC into sub_vault 1.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_b, token_b, 2, 250_000_000)
        .await
        .unwrap();

    // Verify each sub_vault's state is independent.
    let sub_vault_a = fixture.read_sub_vault(1).await;
    let sub_vault_b = fixture.read_sub_vault(2).await;
    let total_shares_a = sub_vault_a.total_shares;
    let total_principal_a = sub_vault_a.total_principal_atoms;
    let total_shares_b = sub_vault_b.total_shares;
    let total_principal_b = sub_vault_b.total_principal_atoms;
    assert_eq!(total_shares_a, 99_999_999_u128);
    assert_eq!(total_principal_a, 99_999_999);
    assert_eq!(total_shares_b, 249_999_999_u128);
    assert_eq!(total_principal_b, 249_999_999);

    // Vault-level counter reflects both sub_vaults.
    let vault_fixed = fixture.read_vault_fixed().await;
    assert_eq!(vault_fixed.sub_vault_count, 2);

    // Depositor A withdraws 40 USDC from sub_vault 0.
    // SubVault 1's state must NOT be affected.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor_a, token_a, 1, 40_000_000_u128)
        .await
        .unwrap();

    let sub_vault_a = fixture.read_sub_vault(1).await;
    let sub_vault_b = fixture.read_sub_vault(2).await;
    let shares_a_after = sub_vault_a.total_shares;
    let principal_a_after = sub_vault_a.total_principal_atoms;
    let shares_b_after = sub_vault_b.total_shares;
    let principal_b_after = sub_vault_b.total_principal_atoms;
    assert_eq!(shares_a_after, 59_999_999_u128);
    assert_eq!(principal_a_after, 59_999_999);
    // SubVault B is untouched.
    assert_eq!(shares_b_after, 249_999_999_u128);
    assert_eq!(principal_b_after, 249_999_999);
}

/// Same vault, two sub_vaults both resting asks in the same market.
/// Each `place_order_for_sub_vault` auto-creates a vault-owned
/// `ClaimedSeat` with a distinct (vault, OWNER_KIND_SUB_VAULT,
/// sub_vault_id) key.
#[tokio::test]
async fn two_sub_vaults_rest_asks_in_same_market() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    // SubVault 0 funded + ask resting.
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator_a,
            1,
            Some(5_000),
            500,
            30 * 86_400,
            100_000_000,
        )
        .await;
    // SubVault 1 funded + ask resting (vault already created — the
    // helper tolerates the second create_vault failing).
    fixture
        .provide_vault_liquidity(
            &admin,
            &depositor,
            &curator_b,
            2,
            Some(8_000),
            600,
            90 * 86_400,
            100_000_000,
        )
        .await;

    // Both vault seats exist with distinct (vault, sub_vault_id) keys —
    // read_vault_seat panics if either is missing.
    let (gv, _) = ydelta::state::vault::global_vault_pda(&mainnet::usdc_bank());
    let seat0 = fixture.read_vault_seat(&gv, 1).await;
    let seat1 = fixture.read_vault_seat(&gv, 2).await;
    assert_eq!(seat0.sub_vault_id, 1);
    assert_eq!(seat1.sub_vault_id, 2);

    // Both asks rest on the same market's asks tree.
    let market = fixture.read_market_fixed().await;
    assert_ne!(market.asks_best_index, hypertree::NIL);
}

/// SubVault A's curator cannot place_order_for_sub_vault for sub_vault B.
/// Cross-curator authorization gate must hold.
#[tokio::test]
async fn cross_curator_cannot_place_for_other_sub_vault() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_a.pubkey(), Some(5_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_b.pubkey(), Some(8_000), 90 * 86_400)
        .await
        .unwrap();

    // Curator A signing place_order_for_sub_vault with sub_vault_id =
    // 1 (curator B's sub_vault) must reject. The processor's
    // `signer == sub_vault.curator` gate catches this — and the vault
    // market-seat for sub_vault 1 must NOT be auto-created by a rejected
    // call.
    fixture.refresh_blockhash().await;
    let result = fixture
        .place_order_for_sub_vault(
            &curator_a,
            /*sub_vault_id=*/ 2, // curator_b's sub_vault
            500,
            30 * 86_400,
            0,
        )
        .await;
    // Cross-curator signing gate must surface the exact
    // VaultCuratorRequired variant.
    crate::assert_custom_error!(result, ydelta::program::YdeltaError::VaultCuratorRequired);

    // A curator must never be able to sign for a sub_vault they don't
    // own — that's the security property this test guards. We don't
    // need an "affirmative half" (curator_b CAN place for sub_vault 1)
    // because that path is exercised end-to-end in
    // `vault_match::vault_ask_crossed_by_borrower_bid_full_fill` and
    // the lifecycle of every vault test that places, matches, and
    // settles vault orders.
}

/// Both sub_vaults share a single pool of vault-level state but
/// per-sub_vault aggregates stay independent. Verifies that
/// `SubVault` reads are correctly keyed by `sub_vault_id` and don't
/// alias.
#[tokio::test]
async fn sub_vault_aggregates_dont_alias() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    let token = fixture.signer_debt_token(&depositor.pubkey());
    fixture.put_token_account(
        token,
        mainnet::usdc_mint(),
        depositor.pubkey(),
        1_000_000_000,
    );

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();

    // Create THREE sub_vaults to verify the tree handles N>2.
    for (id, curator) in [(0u8, &curator_a), (1u8, &curator_b), (2u8, &admin)] {
        fixture.refresh_blockhash().await;
        fixture
            .create_sub_vault(
                &admin,
                curator.pubkey(),
                Some(5_000 + id as u16 * 1_000), // distinct max_ltv per sub_vault
                30 * 86_400,
            )
            .await
            .unwrap();
    }

    let vault = fixture.read_vault_fixed().await;
    assert_eq!(vault.sub_vault_count, 3);

    // Read all three. Each must have its own curator and max_ltv.
    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    let p2 = fixture.read_sub_vault(3).await;
    assert_eq!(p0.curator, curator_a.pubkey());
    assert_eq!(p1.curator, curator_b.pubkey());
    assert_eq!(p2.curator, admin.pubkey());
    assert_eq!(p0.max_ltv_bps, 5_000);
    assert_eq!(p1.max_ltv_bps, 6_000);
    assert_eq!(p2.max_ltv_bps, 7_000);

    // Deposit only into sub_vault 1; verify aggregates on 0 and 2 stay zero.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor, token, 2, 50_000_000)
        .await
        .unwrap();

    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    let p2 = fixture.read_sub_vault(3).await;
    let p0_shares = p0.total_shares;
    let p1_shares = p1.total_shares;
    let p2_shares = p2.total_shares;
    assert_eq!(p0_shares, 0);
    assert_eq!(p1_shares, 49_999_999_u128);
    assert_eq!(p2_shares, 0);
    // Per-sub_vault principal must follow per-sub_vault shares (not alias).
    assert_eq!(p0.total_principal_atoms, 0);
    assert_eq!(p1.total_principal_atoms, 49_999_999);
    assert_eq!(p2.total_principal_atoms, 0);
    // total_assets_atoms == total_principal_atoms (no yield, no loans).
    assert_eq!(p1.total_assets_atoms, 49_999_999);
    // Vault-idle invariant on every sub_vault.
    fixture.assert_vault_idle_invariant(1).await;
    fixture.assert_vault_idle_invariant(1).await;
    fixture.assert_vault_idle_invariant(2).await;
}

/// Per-sub_vault `total_shares` / `total_assets_atoms` /
/// `total_principal_atoms` are the SOLE source of truth for vault-wide
/// totals; there is no separate aggregate header. This test drives
/// deposits and withdrawals across two sub_vaults and asserts the
/// per-sub_vault totals move correctly and sum to the expected
/// vault-wide figure.
#[tokio::test]
async fn vault_aggregate_equals_sum_of_sub_vaults() {
    let fixture = MarketFixture::new().await;
    let admin = fixture.create_trader().await;
    let depositor_a = fixture.create_trader().await;
    let depositor_b = fixture.create_trader().await;
    let curator_a = fixture.create_trader().await;
    let curator_b = fixture.create_trader().await;

    let token_a = fixture.signer_debt_token(&depositor_a.pubkey());
    let token_b = fixture.signer_debt_token(&depositor_b.pubkey());
    fixture.put_token_account(
        token_a,
        mainnet::usdc_mint(),
        depositor_a.pubkey(),
        1_000_000_000,
    );
    fixture.put_token_account(
        token_b,
        mainnet::usdc_mint(),
        depositor_b.pubkey(),
        1_000_000_000,
    );

    fixture.refresh_blockhash().await;
    fixture.create_vault(&admin).await.unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_a.pubkey(), Some(5_000), 30 * 86_400)
        .await
        .unwrap();
    fixture.refresh_blockhash().await;
    fixture
        .create_sub_vault(&admin, curator_b.pubkey(), Some(8_000), 90 * 86_400)
        .await
        .unwrap();

    // Deposit into sub_vault 0. No loans / orders → assets == principal,
    // and at genesis shares == principal (1:1).
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_a, token_a, 1, 100_000_000)
        .await
        .unwrap();
    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    assert_eq!(p0.total_principal_atoms, 99_999_999);
    assert_eq!(p0.total_shares, 99_999_999_u128);
    assert_eq!(p1.total_principal_atoms, 0);

    // Deposit into sub_vault 1.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_deposit(&depositor_b, token_b, 2, 250_000_000)
        .await
        .unwrap();
    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    assert_eq!(p1.total_principal_atoms, 249_999_999);
    assert_eq!(p1.total_shares, 249_999_999_u128);
    // Vault-wide total = Σ per-sub_vault (the per-sub_vault fields are the
    // sole source of truth).
    assert_eq!(
        p0.total_principal_atoms + p1.total_principal_atoms,
        349_999_998
    );
    assert_eq!(p0.total_shares + p1.total_shares, 349_999_998_u128);

    // Partial withdraw from sub_vault 0 — only sub_vault 0 moves.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor_a, token_a, 1, 40_000_000_u128)
        .await
        .unwrap();
    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    assert_eq!(p0.total_shares, 59_999_999_u128);
    assert_eq!(p0.total_principal_atoms, 59_999_999);
    assert_eq!(
        p1.total_principal_atoms, 249_999_999,
        "withdraw from p0 must not touch p1",
    );

    // Full drain of sub_vault 0 — exercises the final-burn zeroing path.
    // No loans deployed in p0, so the last-share gate permits it.
    fixture.refresh_blockhash().await;
    fixture
        .global_vault_withdraw(&depositor_a, token_a, 1, 59_999_999_u128)
        .await
        .unwrap();
    let p0 = fixture.read_sub_vault(1).await;
    let p1 = fixture.read_sub_vault(2).await;
    // SubVault 0 fully drained; vault-wide total now reflects only p1.
    assert_eq!(p0.total_shares, 0);
    assert_eq!(p0.total_principal_atoms, 0);
    assert_eq!(p1.total_principal_atoms, 249_999_999);
}
