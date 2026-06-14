//! Integration tests. Each scenario lives in `tests/cases/<name>.rs`
//! and is included as a submodule here so they share the
//! `test_utils` crate and compile as one integration-test binary.

mod test_utils;

mod cases {
    // SBF-only — loads marginfi.so + ydelta.so + ydelta_test_harness.so via
    // ProgramTest, which requires real binaries on SBF_OUT_DIR. `cargo
    // test-sbf` provides them; native `cargo test` doesn't, so we gate.
    #[cfg(feature = "test-sbf")]
    mod adapter_borrow_repay;
    #[cfg(feature = "test-sbf")]
    mod adapter_conversions;
    mod claim_seat;
    mod create_market;
    // Encumbrance-conservation now exercises vault-ask makers, which
    // need real marginfi banks (MarketFixture) — SBF-only.
    #[cfg(feature = "test-sbf")]
    mod encumbrance_conservation;
    mod liquidation;
    mod self_match;
    mod user_account;
    mod vault;
    // Deposit/withdraw exercise the marginfi adapter end-to-end and need
    // marginfi.so + mainnet bank fixtures, so they're SBF-only.
    #[cfg(feature = "test-sbf")]
    mod deposit_withdraw;
    // Matching-mechanics tests use vault-ask makers (MarketFixture +
    // real marginfi banks) — SBF-only.
    #[cfg(feature = "test-sbf")]
    mod r#match;
    mod place_order;
    // process_matched_loan also CPIs into marginfi (amount_to_shares
    // reads from the live bank); SBF-only.
    #[cfg(feature = "test-sbf")]
    mod bank_config_view;
    // Every loan's lender is a vault sub-vault, so claims go through
    // the sub-vault claim path.
    #[cfg(feature = "test-sbf")]
    mod claim_repayment_for_sub_vault;
    // `convert_p2pool_to_fixed`: the matcher
    // (`match_p2pool_residual_against_asks`) crosses vault sub-vault
    // asks under the unbounded-vault-ask model, so the variable→fixed
    // refinance works.
    #[cfg(feature = "test-sbf")]
    mod convert_p2pool_to_fixed;
    #[cfg(feature = "test-sbf")]
    mod fee_config;
    #[cfg(feature = "test-sbf")]
    mod loan_lifecycle;
    #[cfg(feature = "test-sbf")]
    mod ltv_at_match;
    #[cfg(feature = "test-sbf")]
    mod ltv_buffer;
    #[cfg(feature = "test-sbf")]
    mod ltv_decoupling;
    #[cfg(feature = "test-sbf")]
    mod ob_only_flag;
    #[cfg(feature = "test-sbf")]
    mod oracle_price;
    #[cfg(feature = "test-sbf")]
    mod p2pool;
    #[cfg(feature = "test-sbf")]
    mod process_matched_loan;
    #[cfg(feature = "test-sbf")]
    mod repay;
    #[cfg(feature = "test-sbf")]
    mod bid_rest;
    #[cfg(feature = "test-sbf")]
    mod rate_floor;
    #[cfg(feature = "test-sbf")]
    mod self_cross;
    #[cfg(feature = "test-sbf")]
    mod sunset_sub_vault;
    #[cfg(feature = "test-sbf")]
    mod vault_e2e;
    #[cfg(feature = "test-sbf")]
    mod vault_match;
    #[cfg(feature = "test-sbf")]
    mod vault_multi_profile;
}
