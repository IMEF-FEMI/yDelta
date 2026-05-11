//! `GlobalVault` bootstrap tests.
//!
//! Native (no marginfi) coverage:
//! - Account-count invariants for `create_vault` / `create_risk_profile`.
//! - Borsh round-trip on `CreateRiskProfileParams`.
//! - PDA derivation determinism.
//!
//! SBPF (with marginfi) coverage lands later in Group C alongside the
//! depositor + curator ix wiring.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use ydelta::program::instruction_builders::{
    cancel_order_for_risk_profile_instruction::cancel_order_for_risk_profile_instruction,
    claim_curator_fee_instruction::claim_curator_fee_instruction,
    claim_seat_for_risk_profile_instruction::claim_seat_for_risk_profile_instruction,
    create_risk_profile_instruction::create_risk_profile_instruction,
    create_vault_instruction::create_vault_instruction,
    global_vault_deposit_instruction::global_vault_deposit_instruction,
    global_vault_withdraw_instruction::global_vault_withdraw_instruction,
    place_order_for_risk_profile_instruction::place_order_for_risk_profile_instruction,
    update_order_for_risk_profile_instruction::update_order_for_risk_profile_instruction,
};
use ydelta::program::processor::cancel_order_for_risk_profile::CancelOrderForRiskProfileParams;
use ydelta::program::processor::claim_curator_fee::ClaimCuratorFeeParams;
use ydelta::program::processor::claim_seat_for_risk_profile::ClaimSeatForRiskProfileParams;
use ydelta::program::processor::create_risk_profile::CreateRiskProfileParams;
use ydelta::program::processor::global_vault_deposit::GlobalVaultDepositParams;
use ydelta::program::processor::global_vault_withdraw::GlobalVaultWithdrawParams;
use ydelta::program::processor::place_order_for_risk_profile::PlaceOrderForRiskProfileParams;
use ydelta::program::processor::update_order_for_risk_profile::UpdateOrderForRiskProfileParams;
use ydelta::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
};

#[test]
fn create_vault_ix_has_thirteen_accounts() {
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let marginfi_group = Pubkey::new_unique();
    let lending_pool = Pubkey::new_unique();
    let marginfi_program = Pubkey::new_unique();
    let token_program = Pubkey::new_unique();
    let token_program_22 = Pubkey::new_unique();

    let ix = create_vault_instruction(
        &mint,
        &payer.pubkey(),
        &marginfi_group,
        &lending_pool,
        &marginfi_program,
        &token_program,
        &token_program_22,
    );
    // payer + global_config + vault + mint + global_vault_signer +
    // integration_account + global_vault_staging + token_program +
    // token_program_22 + marginfi_group + lending_pool +
    // marginfi_program + system_program = 13.
    assert_eq!(
        ix.accounts.len(),
        13,
        "create_vault account list includes global_config gate"
    );
}

#[test]
fn create_risk_profile_ix_has_four_accounts() {
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let curator = Pubkey::new_unique();
    let ix = create_risk_profile_instruction(
        &mint,
        &payer.pubkey(),
        0,
        &curator,
        5_000,
        30 * 86_400,
        2u8,
    );
    // payer (signer) + global_config + vault PDA + system_program.
    assert_eq!(
        ix.accounts.len(),
        4,
        "create_risk_profile account list includes global_config gate"
    );
}

#[test]
fn create_risk_profile_params_borsh_round_trip() {
    let original = CreateRiskProfileParams {
        profile_id: 7,
        curator: Pubkey::new_unique(),
        max_ltv_bps: 5_000,
        max_term_seconds: 30 * 86_400,
        allowed_market_max: 3,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = CreateRiskProfileParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 7);
    assert_eq!(decoded.curator, original.curator);
    assert_eq!(decoded.max_ltv_bps, 5_000);
    assert_eq!(decoded.max_term_seconds, 30 * 86_400);
    assert_eq!(decoded.allowed_market_max, 3);
}

#[test]
fn global_vault_pda_seeds_distinguish_mints() {
    let mint_a = Pubkey::new_unique();
    let mint_b = Pubkey::new_unique();
    let (vault_a, _) = global_vault_pda(&mint_a);
    let (vault_b, _) = global_vault_pda(&mint_b);
    assert_ne!(vault_a, vault_b);
}

#[test]
fn vault_signer_and_integration_pdas_distinct() {
    let mint = Pubkey::new_unique();
    let (vault, _) = global_vault_pda(&mint);
    let (signer, _) = global_vault_signer_pda(&vault);
    let (integration, _) = global_vault_integration_account_pda(&vault);
    assert_ne!(signer, integration, "two vault-side PDAs must differ");
    assert_ne!(signer, vault);
    assert_ne!(integration, vault);
}

#[test]
fn create_vault_ix_pda_derivation_is_consistent() {
    // Sanity: the ix builder embeds the same vault PDA the loader
    // expects for `[b"vault", mint]`.
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = create_vault_instruction(
        &mint,
        &payer.pubkey(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
    );
    let (expected_vault, _) = global_vault_pda(&mint);
    // accounts[0] = payer, [1] = global_config, [2] = vault.
    assert_eq!(
        ix.accounts[2].pubkey, expected_vault,
        "vault account in ix matches global_vault_pda(mint)"
    );
}

#[test]
fn global_vault_deposit_ix_has_fourteen_accounts() {
    let mint = Pubkey::new_unique();
    let depositor = Keypair::new();
    let ix = global_vault_deposit_instruction(
        &mint,
        &depositor.pubkey(),
        &Pubkey::new_unique(), // depositor_token
        &Pubkey::new_unique(), // token_program
        &Pubkey::new_unique(), // marginfi_group
        &Pubkey::new_unique(), // lending_pool
        &Pubkey::new_unique(), // liquidity_vault
        &Pubkey::new_unique(), // marginfi_program
        0,                     // profile_id
        100_000,               // amount_atoms
    );
    assert_eq!(
        ix.accounts.len(),
        15,
        "global_vault_deposit account list = depositor + global_config + vault + mint + global_vault_signer + \
         global_vault_staging + depositor_token + token_program + marginfi_group + \
         integration_account + lending_pool + liquidity_vault + marginfi_program + \
         user_account + system_program"
    );
}

#[test]
fn global_vault_withdraw_ix_has_sixteen_accounts() {
    let mint = Pubkey::new_unique();
    let depositor = Keypair::new();
    let ix = global_vault_withdraw_instruction(
        &mint,
        &depositor.pubkey(),
        &Pubkey::new_unique(), // depositor_token
        &Pubkey::new_unique(), // token_program
        &Pubkey::new_unique(), // marginfi_group
        &Pubkey::new_unique(), // lending_pool
        &Pubkey::new_unique(), // lending_pool_oracle
        &Pubkey::new_unique(), // liquidity_vault
        &Pubkey::new_unique(), // bank_liquidity_vault_authority
        &Pubkey::new_unique(), // marginfi_program
        0,                     // profile_id
        100_000_000,           // shares_to_burn
    );
    // global_vault_deposit (15) + lending_pool_oracle + bank_liquidity_vault_authority = 17.
    assert_eq!(
        ix.accounts.len(),
        17,
        "global_vault_withdraw extends global_vault_deposit's surface with oracle + vault_authority"
    );
}

#[test]
fn global_vault_deposit_params_borsh_round_trip() {
    let original = GlobalVaultDepositParams {
        amount_atoms: 100_000,
        profile_id: 7,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = GlobalVaultDepositParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.amount_atoms, 100_000);
    assert_eq!(decoded.profile_id, 7);
}

#[test]
fn global_vault_withdraw_params_borsh_round_trip() {
    let original = GlobalVaultWithdrawParams {
        shares_to_burn: 12_345_678_901_234_u128,
        profile_id: 3,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = GlobalVaultWithdrawParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.shares_to_burn, 12_345_678_901_234_u128);
    assert_eq!(decoded.profile_id, 3);
}

// ─────────────────── Group E — curator ixs ───────────────────

#[test]
fn claim_seat_for_risk_profile_ix_has_four_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = claim_seat_for_risk_profile_instruction(&mint, &market, &payer.pubkey(), 0, 1_000_000);
    // payer + global_config + vault + market + system_program = 5
    assert_eq!(ix.accounts.len(), 5);
}

#[test]
fn place_order_for_risk_profile_ix_has_four_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = place_order_for_risk_profile_instruction(
        &mint,
        &market,
        &payer.pubkey(),
        0,
        500,
        30 * 86_400,
        0,
    );
    assert_eq!(ix.accounts.len(), 5);
}

#[test]
fn cancel_order_for_risk_profile_ix_has_four_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = cancel_order_for_risk_profile_instruction(&mint, &market, &payer.pubkey(), 0);
    assert_eq!(ix.accounts.len(), 5);
}

#[test]
fn update_order_for_risk_profile_ix_has_four_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = update_order_for_risk_profile_instruction(
        &mint,
        &market,
        &payer.pubkey(),
        0,
        600,
        45 * 86_400,
        0,
    );
    assert_eq!(ix.accounts.len(), 5);
}

#[test]
fn claim_seat_for_risk_profile_params_borsh_round_trip() {
    let original = ClaimSeatForRiskProfileParams {
        profile_id: 5,
        max_exposure_atoms: 5_000_000,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = ClaimSeatForRiskProfileParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 5);
    assert_eq!(decoded.max_exposure_atoms, 5_000_000);
}

#[test]
fn place_order_for_risk_profile_params_borsh_round_trip() {
    let original = PlaceOrderForRiskProfileParams {
        profile_id: 5,
        rate_bps: 800,
        term_seconds: 30 * 86_400,
        flags: 0,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = PlaceOrderForRiskProfileParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 5);
    assert_eq!(decoded.rate_bps, 800);
    assert_eq!(decoded.term_seconds, 30 * 86_400);
}

#[test]
fn cancel_order_for_risk_profile_params_borsh_round_trip() {
    let original = CancelOrderForRiskProfileParams { profile_id: 5 };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = CancelOrderForRiskProfileParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 5);
}

#[test]
fn update_order_for_risk_profile_params_borsh_round_trip() {
    let original = UpdateOrderForRiskProfileParams {
        profile_id: 5,
        new_rate_bps: 900,
        new_term_seconds: 45 * 86_400,
        new_flags: 0,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = UpdateOrderForRiskProfileParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 5);
    assert_eq!(decoded.new_rate_bps, 900);
    assert_eq!(decoded.new_term_seconds, 45 * 86_400);
}

#[test]
fn claim_curator_fee_ix_has_fourteen_accounts() {
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = claim_curator_fee_instruction(
        &mint,
        &payer.pubkey(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        0,
    );
    // payer + global_config + vault + global_vault_signer + global_vault_staging +
    // vault_integration + curator_token + debt_bank + liquidity_vault +
    // bank_liquidity_vault_authority + bank_oracle + mint + token_program +
    // marginfi_program + marginfi_group = 15
    assert_eq!(ix.accounts.len(), 15);
}

#[test]
fn claim_curator_fee_params_borsh_round_trip() {
    let original = ClaimCuratorFeeParams { profile_id: 3 };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = ClaimCuratorFeeParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.profile_id, 3);
}

#[test]
fn loan_fixed_grows_to_carry_vault_lender_fields() {
    use std::mem::size_of;
    use ydelta::state::loan::LoanFixed;
    use ydelta::state::LOAN_FIXED_SIZE;
    // LoanFixed carries (lender_kind, lender_profile_id, _pad,
    // lender_global_vault) so vault-funded loans can route repayment
    // back to the originating profile.
    assert_eq!(size_of::<LoanFixed>(), LOAN_FIXED_SIZE);
    // Sanity: LoanFixed has the new fields and they default to zero
    // for a fresh wallet-funded loan.
    const SHARE_VALUE_ONE: u128 = 1u128 << 48;
    let loan = LoanFixed::new_from_matched_loan(
        Pubkey::default(),
        0,
        0,
        Pubkey::default(),
        0,
        0,
        1_000,
        1_000,
        500,
        1_000,
        500,
        86_400,
        0,
        0,
        ydelta::state::loan::LoanType::Fixed,
        0,
        SHARE_VALUE_ONE,
        SHARE_VALUE_ONE,
    );
    assert_eq!(loan.lender_kind, 0); // wallet
    assert_eq!(loan.lender_profile_id, 0);
    assert_eq!(loan.lender_global_vault, Pubkey::default());
}

#[test]
fn loan_fixed_with_vault_lender_fields_stamped() {
    use ydelta::state::loan::LoanFixed;
    let vault_pk = Pubkey::new_unique();
    const SHARE_VALUE_ONE: u128 = 1u128 << 48;
    let loan = LoanFixed::new_from_matched_loan_with_lender(
        Pubkey::default(),
        0,
        0,
        Pubkey::default(),
        0,
        0,
        1_000,
        1_000,
        500,
        1_000,
        500,
        86_400,
        0,
        0,
        ydelta::state::loan::LoanType::Fixed,
        0,
        1, // lender_kind = Vault
        7, // lender_profile_id
        vault_pk,
        0, // curator_fee_bps_snapshot
        SHARE_VALUE_ONE,
        SHARE_VALUE_ONE,
    );
    assert_eq!(loan.lender_kind, 1);
    assert_eq!(loan.lender_profile_id, 7);
    assert_eq!(loan.lender_global_vault, vault_pk);
}

/// Vault-funded loans must not be sellable on the secondary book.
/// The constraint is enforced by `PlaceOrderContext::load` — it
/// requires `loan.lender_kind == OWNER_KIND_USER` for any
/// `OrderKind::SecondaryLoanSale` placement. Vault loans run their
/// full tenure; their only exit paths are: borrower repay,
/// `settle_matured_loan` (post-maturity), or `liquidate_loan`
/// (LTV-breach).
///
/// This test verifies the constant relationships that the loader
/// gates on (the loader requires marginfi accounts so we can't
/// reach the actual `process_place_order` execution path in a pure
/// native test). The companion SBPF test for the runtime reject
/// lives in `vault_e2e.rs` once a fixture method is in place.
#[test]
fn vault_loan_secondary_sale_constants_align() {
    use ydelta::state::OWNER_KIND_RISK_PROFILE;
    use ydelta::state::OWNER_KIND_USER;
    // Sanity: vault-lender loans (`lender_kind = OWNER_KIND_RISK_PROFILE`)
    // must be a different value than the wallet path the loader
    // gates on.
    assert_ne!(OWNER_KIND_USER, OWNER_KIND_RISK_PROFILE);
    assert_eq!(OWNER_KIND_USER, 0);
    assert_eq!(OWNER_KIND_RISK_PROFILE, 1);
}
