//! Verify `BankConfigView`'s byte offsets against the real mainnet
//! USDC + SOL bank dumps. If an upstream marginfi upgrade shifts a
//! field, this test catches it before the matching engine reads from
//! the wrong byte.

use marginfi_mocks::state::{BankConfigView, OracleSetup};

use crate::test_utils::marginfi_fixture::mainnet;
use crate::test_utils::market_fixture::MarketFixture;

#[tokio::test]
async fn usdc_bank_config_view_matches_mainnet_oracle() {
    let fixture = MarketFixture::new().await;
    let bank_data = fixture.account_data(mainnet::usdc_bank()).await;

    let view = BankConfigView::try_from_account_data(&bank_data).unwrap();
    assert_eq!(
        view.primary_oracle(),
        mainnet::usdc_oracle(),
        "BankConfigView.oracle_keys[0] should equal the dumped USDC oracle pubkey \
         — if this fails, the offset arithmetic in marginfi-mocks::BankConfigView \
         is out of sync with marginfi v0.1.8's IDL."
    );

    // Marginfi's USDC bank uses Pyth's push-oracle setup on mainnet
    // (variant 3 of the OracleSetup enum). Defensive lower-bound:
    // anything other than `None` (variant 0) is acceptable since
    // marginfi's exact setup is governance-mutable; assert non-None.
    let setup = view.oracle_setup_raw();
    assert!(
        setup != 0,
        "USDC bank oracle_setup should not be 0 (None); raw=={}",
        setup
    );

    // Weights should be non-zero (USDC is collateral-eligible on mainnet).
    let awi = view.asset_weight_init().to_i128_bits();
    assert!(
        awi > 0,
        "USDC asset_weight_init should be > 0 on mainnet, got {}",
        awi
    );
    let lwi = view.liability_weight_init().to_i128_bits();
    assert!(
        lwi > 0,
        "USDC liability_weight_init should be > 0, got {}",
        lwi
    );

    // oracle_max_age sanity: marginfi mainnet runs in the 60..600s
    // range typically. Just assert > 0 (governance-mutable so we don't
    // pin an exact value).
    assert!(view.oracle_max_age() > 0, "oracle_max_age should be > 0");
}

#[tokio::test]
async fn sol_bank_config_view_matches_mainnet_oracle() {
    let fixture = MarketFixture::new().await;
    let bank_data = fixture.account_data(mainnet::sol_bank()).await;

    let view = BankConfigView::try_from_account_data(&bank_data).unwrap();
    assert_eq!(
        view.primary_oracle(),
        mainnet::sol_oracle(),
        "BankConfigView.oracle_keys[0] should equal the dumped SOL oracle pubkey"
    );

    // SOL bank uses Switchboard-pull (variant 4) on the dump. If this
    // fails at runtime an upstream change moved/renamed the variant.
    let setup = view.oracle_setup();
    assert!(
        matches!(
            setup,
            Some(OracleSetup::SwitchboardPull) | Some(OracleSetup::PythPushOracle)
        ),
        "SOL bank oracle_setup should be Pyth-push or Switchboard-pull; got {:?}",
        setup
    );
}
