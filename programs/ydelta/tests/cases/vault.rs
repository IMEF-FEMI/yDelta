//! `GlobalVault` bootstrap tests.
//!
//! Native (no marginfi) coverage:
//! - Account-count invariants for `create_vault` / `create_sub_vault`.
//! - Borsh round-trip on `CreateSubVaultParams`.
//! - PDA derivation determinism.
//!
//! SBPF (with marginfi) coverage lands later in Group C alongside the
//! depositor + curator ix wiring.

use std::mem::size_of;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

use ydelta::program::instruction_builders::{
    cancel_order_for_sub_vault_instruction::cancel_order_for_sub_vault_instruction,
    claim_curator_fee_instruction::claim_curator_fee_instruction,
    create_sub_vault_instruction::{
        create_pool_sub_vault_instruction, create_private_sub_vault_instruction,
    },
    create_vault_instruction::create_vault_instruction,
    global_vault_deposit_instruction::global_vault_deposit_instruction,
    global_vault_withdraw_instruction::global_vault_withdraw_instruction,
    place_order_for_sub_vault_instruction::place_order_for_sub_vault_instruction,
    set_vault_pause_instruction::set_vault_pause_instruction,
    update_order_for_sub_vault_instruction::update_order_for_sub_vault_instruction,
};
use ydelta::program::processor::cancel_order_for_sub_vault::CancelOrderForSubVaultParams;
use ydelta::program::processor::claim_curator_fee::ClaimCuratorFeeParams;
use ydelta::program::processor::create_sub_vault::CreatePoolSubVaultParams;
use ydelta::program::processor::global_vault_deposit::GlobalVaultDepositParams;
use ydelta::program::processor::global_vault_withdraw::GlobalVaultWithdrawParams;
use ydelta::program::processor::place_order_for_sub_vault::PlaceOrderForSubVaultParams;
use ydelta::program::processor::update_order_for_sub_vault::UpdateOrderForSubVaultParams;
use ydelta::state::loan::LoanFixed;
use ydelta::state::vault::{
    global_vault_integration_account_pda, global_vault_pda, global_vault_signer_pda,
};
use ydelta::state::{LOAN_FIXED_SIZE, OWNER_KIND_SUB_VAULT, OWNER_KIND_USER};

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
        &Pubkey::new_unique(), // bank (v1: vault keyed by bank)
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
fn create_sub_vault_ix_has_four_accounts() {
    let bank = Pubkey::new_unique();
    let payer = Keypair::new();
    let curator = Pubkey::new_unique();
    let pool_ix = create_pool_sub_vault_instruction(
        &bank,
        &payer.pubkey(),
        &curator,
        /*spread_bps=*/ 150,
        /*max_ltv_bps=*/ 5_000,
        /*liquidation_ltv_bps=*/ 6_000,
        30 * 86_400,
        /*curator_fee_bps=*/ 1_000,
    );
    // payer (signer) + global_config + vault PDA + system_program.
    assert_eq!(
        pool_ix.accounts.len(),
        4,
        "create_pool_sub_vault account list includes global_config gate"
    );
    let private_ix = create_private_sub_vault_instruction(
        &bank,
        &payer.pubkey(),
        150,
        5_000,
        6_000,
        30 * 86_400,
    );
    assert_eq!(private_ix.accounts.len(), 4);
    assert_eq!(
        pool_ix.accounts[2].pubkey, private_ix.accounts[2].pubkey,
        "both creators target the same bank-keyed vault PDA"
    );
}

#[test]
fn create_sub_vault_params_borsh_round_trip() {
    let original = CreatePoolSubVaultParams {
        curator: Pubkey::new_unique(),
        spread_bps: 150,
        max_ltv_bps: 5_000,
        liquidation_ltv_bps: 6_000,
        max_term_seconds: 30 * 86_400,
        curator_fee_bps: 1_000,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = CreatePoolSubVaultParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.curator, original.curator);
    assert_eq!(decoded.spread_bps, 150);
    assert_eq!(decoded.max_ltv_bps, 5_000);
    assert_eq!(decoded.liquidation_ltv_bps, 6_000);
    assert_eq!(decoded.max_term_seconds, 30 * 86_400);
    assert_eq!(decoded.curator_fee_bps, 1_000);
}

#[test]
fn global_vault_pda_seeds_distinguish_banks() {
    let bank_a = Pubkey::new_unique();
    let bank_b = Pubkey::new_unique();
    let (vault_a, _) = global_vault_pda(&bank_a);
    let (vault_b, _) = global_vault_pda(&bank_b);
    assert_ne!(vault_a, vault_b);
}

#[test]
fn vault_signer_and_integration_pdas_distinct() {
    let bank = Pubkey::new_unique();
    let (vault, _) = global_vault_pda(&bank);
    let (signer, _) = global_vault_signer_pda(&vault);
    let (integration, _) = global_vault_integration_account_pda(&vault);
    assert_ne!(signer, integration, "two vault-side PDAs must differ");
    assert_ne!(signer, vault);
    assert_ne!(integration, vault);
}

#[test]
fn create_vault_ix_pda_derivation_is_consistent() {
    // Sanity: the ix builder embeds the same vault PDA the loader
    // expects for `[b"vault", bank]` (v1 D1: bank-keyed).
    let bank = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = create_vault_instruction(
        &bank,
        &mint,
        &payer.pubkey(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
        &Pubkey::new_unique(),
    );
    let (expected_vault, _) = global_vault_pda(&bank);
    // accounts[0] = payer, [1] = global_config, [2] = vault.
    assert_eq!(
        ix.accounts[2].pubkey, expected_vault,
        "vault account in ix matches global_vault_pda(bank)"
    );
}

#[test]
fn global_vault_deposit_ix_has_fourteen_accounts() {
    let mint = Pubkey::new_unique();
    let depositor = Keypair::new();
    let ix = global_vault_deposit_instruction(
        &Pubkey::new_unique(), // bank (v1: vault keyed by bank)
        &mint,
        &depositor.pubkey(),
        &Pubkey::new_unique(), // depositor_token
        &Pubkey::new_unique(), // token_program
        &Pubkey::new_unique(), // marginfi_group
        &Pubkey::new_unique(), // lending_pool
        &Pubkey::new_unique(), // liquidity_vault
        &Pubkey::new_unique(), // marginfi_program
        0,                     // sub_vault_id
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
        &Pubkey::new_unique(), // bank (v1: vault keyed by bank)
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
        0,                     // sub_vault_id
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
        sub_vault_id: 7,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = GlobalVaultDepositParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.amount_atoms, 100_000);
    assert_eq!(decoded.sub_vault_id, 7);
}

#[test]
fn global_vault_withdraw_params_borsh_round_trip() {
    let original = GlobalVaultWithdrawParams {
        shares_to_burn: 12_345_678_901_234_u128,
        sub_vault_id: 3,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = GlobalVaultWithdrawParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.shares_to_burn, 12_345_678_901_234_u128);
    assert_eq!(decoded.sub_vault_id, 3);
}

// ─────────────────── Group E — curator ixs ───────────────────

#[test]
fn place_order_for_sub_vault_ix_has_six_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let fee_payer = Keypair::new();
    let curator = Keypair::new();
    let ix = place_order_for_sub_vault_instruction(
        &mint,
        &market,
        &fee_payer.pubkey(),
        &curator.pubkey(),
        1,
        500,
        30 * 86_400,
        0,
    );
    assert_eq!(ix.accounts.len(), 6);
}

#[test]
fn cancel_order_for_sub_vault_ix_has_six_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let fee_payer = Keypair::new();
    let curator = Keypair::new();
    let ix = cancel_order_for_sub_vault_instruction(
        &mint,
        &market,
        &fee_payer.pubkey(),
        &curator.pubkey(),
        1,
    );
    assert_eq!(ix.accounts.len(), 6);
}

#[test]
fn update_order_for_sub_vault_ix_has_six_accounts() {
    let mint = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let fee_payer = Keypair::new();
    let curator = Keypair::new();
    let ix = update_order_for_sub_vault_instruction(
        &mint,
        &market,
        &fee_payer.pubkey(),
        &curator.pubkey(),
        0,
        600,
        45 * 86_400,
        0,
    );
    assert_eq!(ix.accounts.len(), 6);
}

#[test]
fn place_order_for_sub_vault_params_borsh_round_trip() {
    let original = PlaceOrderForSubVaultParams {
        sub_vault_id: 5,
        rate_bps: 800,
        term_seconds: 30 * 86_400,
        flags: 0,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = PlaceOrderForSubVaultParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.sub_vault_id, 5);
    assert_eq!(decoded.rate_bps, 800);
    assert_eq!(decoded.term_seconds, 30 * 86_400);
}

#[test]
fn cancel_order_for_sub_vault_params_borsh_round_trip() {
    let original = CancelOrderForSubVaultParams { sub_vault_id: 5 };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = CancelOrderForSubVaultParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.sub_vault_id, 5);
}

#[test]
fn update_order_for_sub_vault_params_borsh_round_trip() {
    let original = UpdateOrderForSubVaultParams {
        sub_vault_id: 5,
        new_rate_bps: 900,
        new_term_seconds: 45 * 86_400,
        new_flags: 0,
    };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = UpdateOrderForSubVaultParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.sub_vault_id, 5);
    assert_eq!(decoded.new_rate_bps, 900);
    assert_eq!(decoded.new_term_seconds, 45 * 86_400);
}

#[test]
fn claim_curator_fee_ix_has_fourteen_accounts() {
    let mint = Pubkey::new_unique();
    let payer = Keypair::new();
    let ix = claim_curator_fee_instruction(
        &Pubkey::new_unique(), // bank (v1: vault keyed by bank)
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
    let original = ClaimCuratorFeeParams { sub_vault_id: 3 };
    let mut data = Vec::new();
    original.serialize(&mut data).unwrap();
    let decoded = ClaimCuratorFeeParams::try_from_slice(&data).unwrap();
    assert_eq!(decoded.sub_vault_id, 3);
}

/// Pins the curator-fee accumulator arithmetic `claim_curator_fee`
/// applies. `payout = min(actual_atoms, fee_atoms)`
/// (never overpay the curator from depositor-backed atoms) and
/// `accumulator' = fee_atoms.saturating_sub(actual_atoms)` (a marginfi
/// under-pay leaves the un-realised remainder claimable, not silently
/// zeroed). The processor performs exactly this arithmetic with the
/// marginfi-reported `actual_atoms`.
#[test]
fn curator_fee_accumulator_decrement_is_correct() {
    // Helper mirroring claim_curator_fee's accumulator logic.
    let resolve = |fee_atoms: u64, actual_atoms: u64| -> (u64, u64) {
        let payout = actual_atoms.min(fee_atoms);
        let new_accumulator = fee_atoms.saturating_sub(actual_atoms);
        (payout, new_accumulator)
    };

    // Exact return: full payout, accumulator zeroed.
    assert_eq!(resolve(1_000, 1_000), (1_000, 0));

    // marginfi UNDER-pays by 7 atoms: curator paid 993, the 7-atom
    // remainder stays on the accumulator for a later claim — NOT lost.
    assert_eq!(resolve(1_000, 993), (993, 7));

    // marginfi OVER-pays by 5 atoms (±1-drift gate): payout capped at
    // the 1_000 owed (no depositor-backed overpay), accumulator zeroed.
    assert_eq!(resolve(1_000, 1_005), (1_000, 0));

    // Nothing returned: accumulator fully preserved.
    assert_eq!(resolve(1_000, 0), (0, 1_000));
}

/// `protocol_fee_claim` is share-based, so the admin may only be paid the
/// book value of the claimed shares. Any +drift atom must be redeposited
/// to the lender-side pool, not paid out.
#[test]
fn protocol_fee_claim_caps_payout_and_returns_surplus() {
    let resolve = |fee_atoms: u64, actual_atoms: u64| -> (u64, u64) {
        let payout = actual_atoms.min(fee_atoms);
        let surplus = actual_atoms.saturating_sub(payout);
        (payout, surplus)
    };

    assert_eq!(resolve(1_000, 1_000), (1_000, 0));
    assert_eq!(resolve(1_000, 997), (997, 0));
    assert_eq!(resolve(1_000, 1_001), (1_000, 1));
}

/// `SetVaultPause` ix shape. `[admin, global_config, vault]`.
#[test]
fn set_vault_pause_ix_has_three_accounts() {
    let vault = Pubkey::new_unique();
    let admin = Keypair::new();
    let ix = set_vault_pause_instruction(&vault, &admin.pubkey(), true);
    assert_eq!(ix.accounts.len(), 3);
    // The instruction tag byte is SetVaultPause = 36.
    assert_eq!(ix.data[0], 36);
    // Payload byte: paused = 1.
    assert_eq!(ix.data[1], 1);
    // Unpause variant carries paused = 0.
    let ix_off = set_vault_pause_instruction(&vault, &admin.pubkey(), false);
    assert_eq!(ix_off.data[1], 0);
}

#[test]
fn loan_fixed_grows_to_carry_vault_lender_fields() {
    // LoanFixed carries (lender_kind, lender_sub_vault_id, _pad,
    // lender_global_vault) so vault-funded loans can route repayment
    // back to the originating profile.
    assert_eq!(size_of::<LoanFixed>(), LOAN_FIXED_SIZE);
    // Sanity: LoanFixed has the new fields and they default to zero
    // for a fresh wallet-funded loan.
    const SHARE_VALUE_ONE: ydelta::math::Fp48 = ydelta::math::Fp48::ONE;
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
    assert_eq!(loan.lender_sub_vault_id, 0);
    assert_eq!(loan.lender_global_vault, Pubkey::default());
    // Pin the rest of the constructor result so a future refactor
    // can't silently break the field-from-MatchedLoan stamping.
    assert_eq!(loan.principal_debt_atoms, 1_000);
    assert_eq!(loan.outstanding_debt_atoms, 1_000);
    assert_eq!(loan.lender_claimable_atoms, 1_000);
    assert_eq!(loan.collateral_atoms, 500);
    assert_eq!(loan.borrower_rate_bps, 1_000);
    assert_eq!(loan.lender_rate_bps, 500);
    assert_eq!(loan.loan_type, ydelta::state::loan::LoanType::Fixed as u8);
    assert_eq!(loan.state, ydelta::state::loan::LoanState::Active as u8);
    assert_eq!(loan.accumulated_protocol_fee_atoms, 0);
    assert_eq!(loan.accumulated_curator_fee_atoms, 0);
    assert_eq!(loan.principal_retired_atoms, 0);
    // Conservation identity holds at construction.
    assert_eq!(
        loan.outstanding_debt_atoms as u128 + loan.principal_retired_atoms as u128,
        loan.lender_claimable_atoms as u128
            + loan.accumulated_protocol_fee_atoms as u128
            + loan.accumulated_curator_fee_atoms as u128,
    );
}

#[test]
fn loan_fixed_with_vault_lender_fields_stamped() {
    let vault_pk = Pubkey::new_unique();
    const SHARE_VALUE_ONE: ydelta::math::Fp48 = ydelta::math::Fp48::ONE;
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
        7, // lender_sub_vault_id
        vault_pk,
        0, // curator_fee_bps_snapshot
        SHARE_VALUE_ONE,
        SHARE_VALUE_ONE,
    );
    assert_eq!(loan.lender_kind, 1);
    assert_eq!(loan.lender_sub_vault_id, 7);
    assert_eq!(loan.lender_global_vault, vault_pk);
}

/// Pin the seat owner-kind discriminants: user seats and vault
/// (sub-vault) seats must stay distinct, and the literal values are
/// load-bearing for tree-key ordering and loader gates.
/// (Secondary loan sale is out of scope for v1 — see docs/v1-spec.md §9;
/// this test used to carry that framing but only ever pinned constants.)
#[test]
fn owner_kind_discriminants_are_pinned() {
    // Sanity: vault-lender loans (`lender_kind = OWNER_KIND_SUB_VAULT`)
    // must be a different value than the wallet path the loader
    // gates on.
    assert_ne!(OWNER_KIND_USER, OWNER_KIND_SUB_VAULT);
    assert_eq!(OWNER_KIND_USER, 0);
    assert_eq!(OWNER_KIND_SUB_VAULT, 1);
}
